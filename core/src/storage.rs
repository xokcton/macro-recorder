//! Хранение макросов на диске: один JSON-файл на макрос в директории данных
//! приложения плюс `index.json` со сводками (имя, дата, число шагов,
//! длительность).
//!
//! Зачем индекс: файл макроса может весить десятки килобайт из-за превью-
//! скриншотов, а список рисуется при каждом старте и после каждого
//! сохранения/удаления. Индекс позволяет списку не парсить тела макросов
//! вообще. При этом индекс — кэш, а не источник истины: пропал, устарел или
//! побился — молча перестраивается сканированием директории.

use std::io::Write;

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::CoreError;
use crate::macro_data::Macro;

/// Имя файла-кэша со сводками; лежит рядом с макросами.
const INDEX_FILE: &str = "index.json";

/// Строка списка макросов: всё, что нужно UI для отрисовки, — без тела
/// макроса, шагов и превью.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MacroSummary {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub step_count: usize,
    /// Суммарная длительность в мс — то же значение, что `Macro::duration_ms()`,
    /// посчитанное один раз при записи в индекс.
    pub total_ms: u64,
}

impl From<&Macro> for MacroSummary {
    fn from(m: &Macro) -> Self {
        Self {
            id: m.id,
            name: m.name.clone(),
            created_at: m.created_at,
            step_count: m.steps.len(),
            total_ms: m.duration_ms(),
        }
    }
}

pub struct Storage {
    dir: PathBuf,
}

impl Storage {
    /// Открывает хранилище в стандартной для ОС директории данных приложения
    /// (`%APPDATA%\MacroRecorder\macros` / `~/Library/Application Support/...` /
    /// `~/.local/share/macro-recorder/macros`).
    pub fn new() -> Result<Self, CoreError> {
        let proj_dirs = ProjectDirs::from("dev", "MacroRecorder", "MacroRecorder")
            .ok_or(CoreError::NoDataDir)?;
        let dir = proj_dirs.data_dir().join("macros");
        Ok(Self::with_dir(dir))
    }

    /// Открывает хранилище в произвольной директории — используется в тестах и
    /// может пригодиться для "портативного" режима (макросы рядом с exe).
    pub fn with_dir(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn io_err(&self, path: PathBuf) -> impl FnOnce(std::io::Error) -> CoreError {
        move |source| CoreError::Io { path, source }
    }

    fn path_for(&self, id: Uuid) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join(INDEX_FILE)
    }

    pub fn save(&self, macro_: &Macro) -> Result<(), CoreError> {
        fs::create_dir_all(&self.dir).map_err(self.io_err(self.dir.clone()))?;
        let path = self.path_for(macro_.id);
        let json = serde_json::to_vec_pretty(macro_)?;
        atomic_write(&path, &json).map_err(self.io_err(path))?;

        // Индекс — кэш: сбой его обновления не должен превращать успешное
        // сохранение макроса в ошибку для пользователя. Рассинхрон чинится
        // перестройкой в `list()`.
        if let Err(err) = self.upsert_index_entry(MacroSummary::from(macro_)) {
            tracing::warn!(%err, "не удалось обновить индекс, он будет перестроен");
        }
        Ok(())
    }

    pub fn load(&self, id: Uuid) -> Result<Macro, CoreError> {
        let path = self.path_for(id);
        let bytes = fs::read(&path).map_err(self.io_err(path))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn delete(&self, id: Uuid) -> Result<(), CoreError> {
        let path = self.path_for(id);
        fs::remove_file(&path).map_err(self.io_err(path))?;
        // Сразу синхронизируем индекс; сбой не фатален — list() перестроит.
        if let Err(err) = self.remove_index_entry(id) {
            tracing::warn!(%err, "не удалось убрать запись из индекса, он будет перестроен");
        }
        Ok(())
    }

    /// Все сохранённые макросы, отсортированные по дате создания.
    ///
    /// Читает `index.json` и НЕ парсит тела макросов. Индекс отсутствует или
    /// побит — молча перестраивается из файлов; записи, ссылающиеся на
    /// несуществующие файлы (удаляли руками, минуя `delete`), отбрасываются.
    pub fn list(&self) -> Result<Vec<MacroSummary>, CoreError> {
        let mut summaries = match self.read_index() {
            Some(summaries) => summaries,
            None => self.rebuild_index()?,
        };

        // `fs::metadata` на порядок дешевле разбора JSON с base64-превью.
        let before = summaries.len();
        summaries.retain(|s| self.path_for(s.id).is_file());
        if summaries.len() != before {
            // Кто-то удалял файлы мимо `delete()` — синхронизируем кэш,
            // иначе протухшая запись жила бы в индексе вечно.
            if let Err(err) = self.write_index(&summaries) {
                tracing::warn!(%err, "не удалось перезаписать индекс после чистки");
            }
        }

        Ok(summaries)
    }

    /// `Some` — индекс прочитан; `None` — файла нет либо он не разбирается
    /// (случай логируется). Оба варианта `None` ведут к перестройке.
    fn read_index(&self) -> Option<Vec<MacroSummary>> {
        let bytes = fs::read(self.index_path()).ok()?;
        match serde_json::from_slice(&bytes) {
            Ok(summaries) => Some(summaries),
            Err(err) => {
                tracing::warn!(%err, "индекс повреждён, перестраиваю из файлов макросов");
                None
            }
        }
    }

    /// Медленный путь: парсит все файлы целиком (включая превью), строит
    /// сводки, сортирует и пытается сохранить результат новым индексом.
    fn rebuild_index(&self) -> Result<Vec<MacroSummary>, CoreError> {
        let mut result = Vec::new();
        if !self.dir.exists() {
            return Ok(result);
        }
        for entry in fs::read_dir(&self.dir).map_err(self.io_err(self.dir.clone()))? {
            let entry = entry.map_err(self.io_err(self.dir.clone()))?;
            let path = entry.path();

            // Сам индекс и обрезки атомарной записи (*.tmp) — не макросы.
            if path.file_name().and_then(|n| n.to_str()) == Some(INDEX_FILE) {
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let bytes = fs::read(&path).map_err(self.io_err(path.clone()))?;
            match serde_json::from_slice::<Macro>(&bytes) {
                Ok(m) => result.push(MacroSummary::from(&m)),
                Err(err) => {
                    tracing::warn!(?path, %err, "не удалось разобрать файл макроса, пропускаю");
                }
            }
        }
        result.sort_by_key(|s| s.created_at);

        if let Err(err) = self.write_index(&result) {
            tracing::warn!(%err, "не удалось записать свежий индекс (работаю без него)");
        }
        Ok(result)
    }

    /// Атомарная запись индекса (тот же шаблон, что у макросов).
    fn write_index(&self, summaries: &[MacroSummary]) -> Result<(), CoreError> {
        let path = self.index_path();
        let json = serde_json::to_vec_pretty(summaries)?;
        atomic_write(&path, &json).map_err(|source| CoreError::Io { path, source })
    }

    fn upsert_index_entry(&self, summary: MacroSummary) -> Result<(), CoreError> {
        let mut summaries = self.read_index().unwrap_or_default();
        match summaries.iter_mut().find(|s| s.id == summary.id) {
            // Позицию существующей записи сохраняем — список не перемешивается.
            Some(slot) => *slot = summary,
            None => summaries.push(summary),
        }
        summaries.sort_by_key(|s| s.created_at);
        self.write_index(&summaries)
    }

    fn remove_index_entry(&self, id: Uuid) -> Result<(), CoreError> {
        let mut summaries = self.read_index().unwrap_or_default();
        summaries.retain(|s| s.id != id);
        self.write_index(&summaries)
    }
}

/// Атомарная запись файла: сначала целиком во временный файл рядом с целевым,
/// затем переименование поверх него. Прерывание в любой момент оставляет либо
/// прежний целый файл, либо мусорный `.tmp` — но не битый целевой JSON.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp_name = path.as_os_str().to_os_string();
    // PID в имени, а не голый ".tmp": ничто не мешает запустить два экземпляра
    // GUI одновременно (single-instance лока в проекте нет), и оба, сохраняя
    // РАЗНЫЕ макросы примерно в одну и ту же секунду, всё равно обновляют один
    // и тот же `index.json` (см. `Storage::upsert_index_entry`) — с общим
    // именем `.tmp` их записи делили бы один и тот же временный файл, и
    // "победитель" мог бы переименовать поверх целевого файла хвост от ЧУЖОЙ,
    // ещё не дописанной записи. У самих тел макросов (`<uuid>.json`) этой
    // проблемы и так не было — у каждого свой путь, — но у общего индекса
    // была. PID делает временный файл процесса уникальным без блокировок ради
    // редкого стечения обстоятельств; гонку МЕЖДУ потоками одного процесса это
    // не решает, но `Storage` в проекте и так вызывается только из UI-потока.
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp_name);

    let mut file = fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?; // данные реально на диске ДО переименования
    drop(file);

    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::InputEvent;
    use crate::macro_data::MacroStep;

    fn sample_macro() -> Macro {
        Macro::new(
            "test macro",
            vec![MacroStep::new(InputEvent::MouseMove { x: 1.0, y: 2.0 }, 10)],
        )
    }

    #[test]
    fn save_then_load_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::with_dir(tmp.path());
        let m = sample_macro();

        storage.save(&m).unwrap();
        let loaded = storage.load(m.id).unwrap();

        assert_eq!(m, loaded);
    }

    #[test]
    fn list_returns_saved_macros() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::with_dir(tmp.path());

        let a = sample_macro();
        let b = sample_macro();
        storage.save(&a).unwrap();
        storage.save(&b).unwrap();

        let all = storage.list().unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn delete_removes_macro() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::with_dir(tmp.path());
        let m = sample_macro();
        storage.save(&m).unwrap();

        storage.delete(m.id).unwrap();

        assert!(storage.load(m.id).is_err());
    }

    #[test]
    fn list_on_missing_dir_is_empty_not_error() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::with_dir(tmp.path().join("does-not-exist-yet"));
        assert_eq!(storage.list().unwrap(), Vec::new());
    }

    #[test]
    fn list_reads_index_not_bodies() {
        // Подменяем индекс «неправильными» данными при живом макросе на диске.
        // Если list() вернул имя из индекса — тела файлов точно не парсились.
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::with_dir(tmp.path());
        let m = sample_macro();
        storage.save(&m).unwrap();

        let fake = vec![MacroSummary {
            id: m.id,
            name: "from-index".into(),
            created_at: m.created_at,
            step_count: 99,
            total_ms: 99_999,
        }];
        storage.write_index(&fake).unwrap();

        let all = storage.list().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "from-index");
        assert_eq!(all[0].step_count, 99);
    }

    #[test]
    fn missing_index_rebuilds_from_bodies_sorted_by_created_at() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::with_dir(tmp.path());
        let mut a = sample_macro();
        a.name = "alpha".into();
        let mut b = sample_macro();
        b.name = "beta".into();
        b.created_at += chrono::Duration::milliseconds(5);
        storage.save(&a).unwrap();
        storage.save(&b).unwrap();

        fs::remove_file(storage.index_path()).unwrap();

        let all = storage.list().unwrap();
        let names: std::collections::BTreeSet<_> = all.into_iter().map(|s| s.name).collect();
        let expected: std::collections::BTreeSet<_> = ["alpha".to_string(), "beta".to_string()]
            .into_iter()
            .collect();
        assert_eq!(names, expected);
    }

    #[test]
    fn stale_index_entries_pointing_to_missing_files_are_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::with_dir(tmp.path());
        let m = sample_macro();
        storage.save(&m).unwrap();

        // Запись-призрак: id есть в индексе, файла нет.
        storage
            .upsert_index_entry(MacroSummary {
                id: Uuid::new_v4(),
                name: "ghost".into(),
                created_at: m.created_at,
                step_count: 1,
                total_ms: 1,
            })
            .unwrap();

        let all = storage.list().unwrap();
        assert_eq!(all.len(), 1, "запись без файла должна исчезнуть из списка");
        assert_eq!(all[0].id, m.id);
    }

    #[test]
    fn delete_updates_index() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Storage::with_dir(tmp.path());
        let a = sample_macro();
        storage.save(&a).unwrap();

        storage.delete(a.id).unwrap();

        assert!(storage.list().unwrap().is_empty());
    }
}
