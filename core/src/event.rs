//! Доменная модель событий ввода.
//!
//! Специально НЕ переиспользуем типы `rdevin::Key` / `rdevin::EventType` напрямую
//! в публичном API и в формате хранения макросов. `rdevin` сам себя описывает как
//! "subject to extreme change" — если его enum когда-нибудь поменяется, старые
//! сохранённые макросы не должны от этого сломаться. Вся конвертация в/из rdevin
//! изолирована в `backend::rdevin_backend` и нигде больше не всплывает.

use serde::{Deserialize, Serialize};

/// Клавиша клавиатуры. Соответствует раскладке QWERTY (как и в большинстве
/// низкоуровневых библиотек ввода) — конкретный отображаемый символ на нестандартных
/// раскладках может отличаться, для автоматизации это обычно не важно.
///
/// `Unknown(u32)` — запасной вариант для клавиш, которых нет в списке ниже
/// (например, специфичные мультимедиа-клавиши некоторых клавиатур). Код внутри —
/// платформенный код клавиши на момент записи, этого достаточно, чтобы клавиша
/// была воспроизведена клавиатурой той же раскладки на той же ОС даже без
/// присвоенного нами имени.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Key {
    Alt,
    AltGr,
    Backspace,
    CapsLock,
    ControlLeft,
    ControlRight,
    Delete,
    DownArrow,
    End,
    Escape,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
    F21,
    F22,
    F23,
    F24,
    Home,
    LeftArrow,
    MetaLeft,
    MetaRight,
    PageDown,
    PageUp,
    Return,
    RightArrow,
    ShiftLeft,
    ShiftRight,
    Space,
    Tab,
    UpArrow,
    PrintScreen,
    ScrollLock,
    Pause,
    NumLock,
    BackQuote,
    Num0,
    Num1,
    Num2,
    Num3,
    Num4,
    Num5,
    Num6,
    Num7,
    Num8,
    Num9,
    Minus,
    Equal,
    KeyA,
    KeyB,
    KeyC,
    KeyD,
    KeyE,
    KeyF,
    KeyG,
    KeyH,
    KeyI,
    KeyJ,
    KeyK,
    KeyL,
    KeyM,
    KeyN,
    KeyO,
    KeyP,
    KeyQ,
    KeyR,
    KeyS,
    KeyT,
    KeyU,
    KeyV,
    KeyW,
    KeyX,
    KeyY,
    KeyZ,
    LeftBracket,
    RightBracket,
    SemiColon,
    Quote,
    BackSlash,
    IntlBackslash,
    Comma,
    Dot,
    Slash,
    Insert,
    KpReturn,
    KpMinus,
    KpPlus,
    KpMultiply,
    KpDivide,
    KpDecimal,
    KpEqual,
    KpComma,
    Kp0,
    Kp1,
    Kp2,
    Kp3,
    Kp4,
    Kp5,
    Kp6,
    Kp7,
    Kp8,
    Kp9,
    VolumeUp,
    VolumeDown,
    VolumeMute,
    Function,
    /// Запасной вариант: платформенный код клавиши, которую мы не распознали по имени.
    Unknown(u32),
}

/// Кнопка мыши.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    /// Запасной вариант для нестандартных кнопок (доп. кнопки игровых мышей и т.п.)
    Unknown(u8),
}

/// Единичное событие ввода — ровно то, что физически произошло.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
    MouseMove {
        x: f64,
        y: f64,
    },
    MouseButton {
        button: MouseButton,
        pressed: bool,
    },
    /// Положительная дельта — скролл вверх/вправо (как в rdevin).
    MouseWheel {
        delta_x: i64,
        delta_y: i64,
    },
    Key {
        key: Key,
        pressed: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_event_roundtrips_through_json() {
        let events = [
            InputEvent::MouseMove { x: 120.5, y: 40.0 },
            InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true,
            },
            InputEvent::MouseWheel {
                delta_x: 0,
                delta_y: -3,
            },
            InputEvent::Key {
                key: Key::KeyA,
                pressed: true,
            },
            InputEvent::Key {
                key: Key::Unknown(12345),
                pressed: false,
            },
        ];

        for event in events {
            let json = serde_json::to_string(&event).expect("сериализация не должна падать");
            let restored: InputEvent =
                serde_json::from_str(&json).expect("десериализация не должна падать");
            assert_eq!(event, restored);
        }
    }
}
