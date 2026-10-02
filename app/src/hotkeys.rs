//! Регистрация системных хоткеев и перепривязка через настройки.
//!
//! Нажатия вычитывает фоновый поток (`crate::events::spawn_hotkey_forwarder`)
//! и пересылает идентификаторы событий в UI; здесь остаётся только маппинг
//! «идентификатор события → действие», который выполняется в UI-потоке в
//! момент обработки — поэтому видит свежие привязки сразу после `rebind`.

use std::collections::HashMap;

use global_hotkey::hotkey::HotKey;
use global_hotkey::GlobalHotKeyManager;
use mrcore::{HotkeyAction, HotkeyBinding};

/// Почему не удалось перепривязать хоткей — UI сам решает, как это показать
/// (перевести на нужный язык и т.п.), поэтому здесь только структурированная
/// причина, без готового текста.
#[derive(Debug)]
pub enum RebindError {
    /// Глобальные хоткеи в принципе недоступны на этой системе.
    Unavailable,
    /// Строка комбинации не разобралась (например, введено что-то без
    /// поддерживаемой клавиши).
    ParseFailed,
    /// Эта комбинация уже занята другим действием — сначала нужно освободить.
    Conflict(HotkeyAction),
    /// ОС отказалась регистрировать хоткей (например, комбинация уже занята
    /// другой программой в системе).
    OsRejected(String),
}

pub struct AppHotkeys {
    // `None`, если инициализация менеджера в целом не удалась (например, нет
    // прав на этой ОС) — приложение всё равно остаётся работоспособным через
    // кнопки, просто перепривязка/срабатывание хоткеев недоступны.
    manager: Option<GlobalHotKeyManager>,
    id_to_action: HashMap<u32, HotkeyAction>,
    bindings: Vec<HotkeyBinding>,
}

impl AppHotkeys {
    /// Регистрирует переданные привязки. Если не удалась регистрация
    /// ОТДЕЛЬНОЙ комбинации — молча пропускаем именно её (лог + остальные
    /// хоткеи продолжают работать); действие в этом случае просто не будет
    /// реагировать на хоткей, пока пользователь не перепривяжет его в
    /// настройках.
    pub fn new(bindings: &[HotkeyBinding]) -> Self {
        let manager = match GlobalHotKeyManager::new() {
            Ok(m) => Some(m),
            Err(err) => {
                tracing::warn!(%err, "не удалось инициализировать глобальные хоткеи — доступны только кнопки");
                None
            }
        };

        let mut id_to_action = HashMap::new();
        if let Some(manager) = &manager {
            for binding in bindings {
                register(manager, binding, &mut id_to_action);
            }
        }

        Self {
            manager,
            id_to_action,
            bindings: bindings.to_vec(),
        }
    }

    pub fn is_available(&self) -> bool {
        self.manager.is_some()
    }

    /// Текущие привязки — для отображения в настройках.
    pub fn bindings(&self) -> &[HotkeyBinding] {
        &self.bindings
    }

    pub fn combo_for(&self, action: HotkeyAction) -> Option<&str> {
        self.bindings
            .iter()
            .find(|b| b.action == action)
            .map(|b| b.combo.as_str())
    }

    /// Маппит сырой идентификатор события `global-hotkey` в действие.
    /// Вызывается из UI-потока в момент обработки события, поэтому даже
    /// сразу после `rebind` отвечает по актуальным привязкам.
    pub fn action_for_id(&self, id: u32) -> Option<HotkeyAction> {
        self.id_to_action.get(&id).copied()
    }

    /// Пытается перепривязать `action` на `new_combo`. При успехе старая
    /// комбинация снята, новая зарегистрирована и запомнена. При ошибке
    /// ничего не меняется — старая привязка остаётся рабочей.
    pub fn rebind(&mut self, action: HotkeyAction, new_combo: &str) -> Result<(), RebindError> {
        let Some(manager) = &self.manager else {
            return Err(RebindError::Unavailable);
        };

        let new_hotkey: HotKey = new_combo.parse().map_err(|_| RebindError::ParseFailed)?;

        if let Some(&existing_action) = self.id_to_action.get(&new_hotkey.id()) {
            if existing_action != action {
                return Err(RebindError::Conflict(existing_action));
            }
            // Тот же хоткей уже привязан к этому же действию — нечего делать.
            return Ok(());
        }

        manager
            .register(new_hotkey)
            .map_err(|err| RebindError::OsRejected(err.to_string()))?;

        // Старую регистрацию снимаем только ПОСЛЕ успешной новой — если бы
        // порядок был обратным, при ошибке выше действие осталось бы вообще
        // без рабочего хоткея.
        if let Some(old) = self.bindings.iter().find(|b| b.action == action) {
            if let Ok(old_hotkey) = old.combo.parse::<HotKey>() {
                let _ = manager.unregister(old_hotkey);
            }
        }

        self.id_to_action.retain(|_, a| *a != action);
        self.id_to_action.insert(new_hotkey.id(), action);
        match self.bindings.iter_mut().find(|b| b.action == action) {
            Some(b) => b.combo = new_combo.to_string(),
            None => self.bindings.push(HotkeyBinding {
                action,
                combo: new_combo.to_string(),
            }),
        }

        Ok(())
    }
}

fn register(
    manager: &GlobalHotKeyManager,
    binding: &HotkeyBinding,
    id_to_action: &mut HashMap<u32, HotkeyAction>,
) {
    let hotkey: HotKey = match binding.combo.parse() {
        Ok(hk) => hk,
        Err(err) => {
            tracing::warn!(combo = %binding.combo, ?err, "не удалось разобрать комбинацию клавиш");
            return;
        }
    };
    match manager.register(hotkey) {
        Ok(()) => {
            id_to_action.insert(hotkey.id(), binding.action);
        }
        Err(err) => {
            tracing::warn!(combo = %binding.combo, %err, "не удалось зарегистрировать хоткей");
        }
    }
}
