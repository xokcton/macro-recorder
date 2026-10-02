//! Настройки приложения (не путать с макросами в `storage.rs`): один JSON-файл
//! на всё — сейчас там хоткеи и выбранный язык, но структура рассчитана на то,
//! что сюда естественно добавлять новые поля по мере роста приложения.
//!
//! Ключевое отличие от `Storage`: загрузка настроек **никогда не должна
//! мешать приложению стартовать**. Если файла нет, он повреждён или на диске
//! лежит что-то от старой версии без части полей — просто используем разумные
//! значения по умолчанию (`#[serde(default)]` на каждом поле) вместо того,
//! чтобы возвращать `Result` и заставлять `app` решать, что делать с ошибкой
//! при каждом запуске.

use std::fs;
use std::path::PathBuf;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::hotkeys::{default_bindings, HotkeyBinding};
use crate::screen_guard::ScreenCaptureConfig;
use crate::storage::atomic_write;

/// Какой бэкенд ввода использовать. Выбирается один раз при старте
/// приложения: слушатель записи живёт весь процесс (см. `recorder.rs`),
/// поэтому смена «на лету» невозможна в принципе — настройка применяется
/// после перезапуска.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum InputBackendChoice {
    /// Хуки через rdevin: X11, Windows, macOS. Дефолт.
    #[default]
    Rdevin,
    /// Прямой доступ к `/dev/input` + `/dev/uinput` — единственный полный
    /// вариант на чистом Wayland. Нужна сборка с фичей `evdev`;
    /// без неё приложение тихо откатится на rdevin.
    Evdev,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default = "default_bindings")]
    pub hotkeys: Vec<HotkeyBinding>,
    /// Код языка UI (например "ru"/"en"). `None` — язык по умолчанию
    /// (конкретное значение "по умолчанию" решает `app`, ядро про UI не знает).
    #[serde(default)]
    pub language: Option<String>,
    /// Разрешить привязку глобального хоткея совсем без модификатора
    /// (Ctrl/Alt/Shift/Cmd) — по умолчанию `app::settings_panel` при захвате
    /// комбинации требует хотя бы один, потому что голая клавиша,
    /// зарегистрированная как системный хоткей, перехватывала бы обычный ввод
    /// (например, `A`) везде в системе, а не только в этом приложении.
    /// Включение — осознанный выбор продвинутого пользователя через чекбокс в
    /// настройках, поэтому по умолчанию `false`.
    #[serde(default)]
    pub allow_bare_hotkeys: bool,
    /// Что сохранять при клике, если экран-сторож вообще доступен в этой
    /// сборке (Cargo-фича `screen-guard` у `app`, см. `app::screen_guard`).
    /// Раньше отпечаток (для автопаузы при воспроизведении) и миниатюра (для
    /// превью в редакторе шагов) всегда шли парой — теперь это независимые
    /// флаги. По умолчанию оба выключены: ничего лишнего не пишем на диск,
    /// пока пользователь явно не попросил.
    #[serde(default)]
    pub screen_capture: ScreenCaptureConfig,
    /// Сжимать авто-повтор клавиш (удержания) в один шаг с суммарной задержкой.
    #[serde(default)]
    pub compress_key_repeats: bool,
    /// Какой бэкенд ввода использовать — применяется при следующем запуске
    /// приложения (см. `InputBackendChoice`).
    #[serde(default)]
    pub input_backend: InputBackendChoice,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkeys: default_bindings(),
            language: None,
            allow_bare_hotkeys: false,
            screen_capture: ScreenCaptureConfig::NONE,
            compress_key_repeats: false,
            input_backend: InputBackendChoice::default(),
        }
    }
}

pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new() -> Result<Self, CoreError> {
        let proj_dirs = ProjectDirs::from("dev", "MacroRecorder", "MacroRecorder")
            .ok_or(CoreError::NoDataDir)?;
        Ok(Self::with_path(proj_dirs.data_dir().join("settings.json")))
    }

    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Загружает настройки. Никогда не возвращает ошибку наружу — файла нет
    /// при первом запуске, это нормальное состояние, не сбой.
    pub fn load(&self) -> Settings {
        fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, settings: &Settings) -> Result<(), CoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| CoreError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let json = serde_json::to_vec_pretty(settings)?;
        atomic_write(&self.path, &json).map_err(|source| CoreError::Io {
            path: self.path.clone(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkeys::HotkeyAction;

    #[test]
    fn missing_file_loads_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::with_path(tmp.path().join("does-not-exist.json"));
        assert_eq!(store.load(), Settings::default());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::with_path(tmp.path().join("settings.json"));

        let mut settings = Settings {
            compress_key_repeats: true,
            language: Some("en".into()),
            ..Settings::default()
        };
        settings.hotkeys[0].combo = "Ctrl+Shift+R".into();

        store.save(&settings).unwrap();
        assert_eq!(store.load(), settings);
    }

    #[test]
    fn corrupted_file_falls_back_to_defaults_instead_of_panicking() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        fs::write(&path, b"{ not valid json at all").unwrap();
        let store = SettingsStore::with_path(path);
        assert_eq!(store.load(), Settings::default());
    }

    #[test]
    fn old_file_missing_new_fields_still_loads_via_serde_default() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        // Имитация файла от более старой версии — только часть полей.
        fs::write(&path, br#"{"language": "ru"}"#).unwrap();
        let store = SettingsStore::with_path(path);
        let loaded = store.load();
        assert_eq!(loaded.language, Some("ru".to_string()));
        assert_eq!(loaded.hotkeys, default_bindings());
        assert!(
            !loaded.allow_bare_hotkeys,
            "старый файл не знает об этом поле — должен подставиться безопасный дефолт"
        );
        assert_eq!(
            loaded.screen_capture,
            ScreenCaptureConfig::NONE,
            "старый файл не знает об этом поле — ничего не должно включаться само по себе"
        );
        assert_eq!(
            loaded.input_backend,
            InputBackendChoice::Rdevin,
            "старый файл не знает о бэкенде — должен остаться безопасный дефолт"
        );
    }

    #[test]
    fn allow_bare_hotkeys_and_screen_capture_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SettingsStore::with_path(tmp.path().join("settings.json"));

        let settings = Settings {
            allow_bare_hotkeys: true,
            screen_capture: ScreenCaptureConfig {
                fingerprint: true,
                thumbnail: false,
            },
            input_backend: InputBackendChoice::Evdev,
            ..Settings::default()
        };

        store.save(&settings).unwrap();
        assert_eq!(store.load(), settings);
    }

    #[test]
    fn default_bindings_cover_every_action_exactly_once() {
        let bindings = default_bindings();
        let actions = [
            HotkeyAction::ToggleRecording,
            HotkeyAction::TogglePause,
            HotkeyAction::TogglePlayback,
            HotkeyAction::StopAll,
        ];
        for action in actions {
            assert_eq!(
                bindings.iter().filter(|b| b.action == action).count(),
                1,
                "действие {action:?} должно встречаться в default_bindings() ровно один раз"
            );
        }
    }
}
