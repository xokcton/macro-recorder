//! Доменные типы горячих клавиш. Само связывание с ОС (регистрация системных
//! accelerator'ов через крейт `global-hotkey`) живёт в `app`-крейте, потому что
//! требует цикла событий окна — здесь только то, о чём должны договориться
//! `core` и `app`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HotkeyAction {
    ToggleRecording,
    /// Пауза того, что сейчас активно — записи или воспроизведения. Не два
    /// отдельных действия на одну и ту же комбинацию: ОС не позволяет
    /// зарегистрировать один физический хоткей дважды, поэтому "пауза" — одна
    /// кнопка, а какой процесс она ставит на паузу, решает `app` по текущему
    /// состоянию.
    TogglePause,
    TogglePlayback,
    StopAll,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HotkeyBinding {
    pub action: HotkeyAction,
    /// Человекочитаемая комбинация, например "Ctrl+Alt+R". Парсинг в системный
    /// accelerator — на стороне `app`.
    pub combo: String,
}

/// Комбинации по умолчанию — то, с чем приложение запустится "из коробки".
pub fn default_bindings() -> Vec<HotkeyBinding> {
    vec![
        HotkeyBinding {
            action: HotkeyAction::ToggleRecording,
            combo: "Ctrl+Alt+R".into(),
        },
        HotkeyBinding {
            action: HotkeyAction::TogglePause,
            combo: "Ctrl+Alt+Space".into(),
        },
        HotkeyBinding {
            action: HotkeyAction::TogglePlayback,
            combo: "Ctrl+Alt+P".into(),
        },
        HotkeyBinding {
            action: HotkeyAction::StopAll,
            combo: "Ctrl+Alt+Escape".into(),
        },
    ]
}
