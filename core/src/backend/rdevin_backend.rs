//! Реализация [`InputBackend`] поверх крейта `rdevin`.
//!
//! Это единственный файл во всём проекте, который импортирует `rdevin`. Вся
//! конвертация между нашей доменной моделью (`crate::event`) и типами `rdevin`
//! живёт здесь же.
//!
//! ## Известное ограничение: доп. кнопки мыши (Button4/5, "назад"/"вперёд") на macOS
//!
//! Проверено по исходникам `rdevin` 0.1.0 для всех трёх ОС:
//! - **Linux (X11)**: любая кнопка мыши — это просто номер (1=левая, 2=средняя,
//!   3=правая, обычно 8/9=боковые "назад"/"вперёд", но может отличаться у разных
//!   мышей/драйверов). `rdevin` захватывает и воспроизводит этот номер как есть
//!   через `XTestFakeButtonEvent` — работает для любой кнопки, не только
//!   именованных.
//! - **Windows**: боковые кнопки — это `WM_XBUTTONDOWN/UP` с кодом XBUTTON1/2.
//!   `rdevin` корректно достаёт этот код при записи и воспроизводит его через
//!   `MOUSEEVENTF_XDOWN/XUP` — тоже работает.
//! - **macOS**: `rdevin` в файлах `macos/common.rs` (захват) и `macos/simulate.rs`
//!   (воспроизведение) обрабатывает **только `Left` и `Right`**. События
//!   `CGEventType::OtherMouseDown/Up` (средняя кнопка и боковые) вообще не
//!   перечислены при захвате, а при воспроизведении `Button::Middle` и
//!   `Button::Unknown(_)` явно уходят в `_ => return None` — то есть тихо
//!   игнорируются. **На macOS в текущем виде библиотеки боковые кнопки и
//!   средний клик не запишутся и не воспроизведутся.**
//!
//! Это ограничение самой библиотеки, а не нашей архитектуры: `from_rdevin_button`
//! ниже уже полностью и без потерь поддерживает произвольный код кнопки — если
//! это когда-нибудь починят в `rdevin` (или мы форкнём с патчем, добавив кейсы
//! `OtherMouseDown/Up` в `common.rs` и `Button::Middle | Button::Unknown` в
//! `simulate.rs`), у нас в проекте не придётся менять ничего, кроме этого файла.

use crate::error::CoreError;
use crate::event::{InputEvent, Key, MouseButton};

use super::InputBackend;

/// Бэкенд ввода на основе `rdevin` (Windows / macOS / X11-Linux).
#[derive(Debug, Default, Clone, Copy)]
pub struct RdevinBackend;

impl InputBackend for RdevinBackend {
    fn listen(&self, mut callback: Box<dyn FnMut(InputEvent) + Send>) -> Result<(), CoreError> {
        rdevin::listen(move |event| {
            if let Some(mapped) = from_rdevin_event(&event) {
                callback(mapped);
            }
        })
        .map_err(|e| CoreError::ListenFailed(format!("{e:?}")))
    }

    fn simulate(&self, event: InputEvent) -> Result<(), CoreError> {
        let event_type = to_rdevin_event_type(event);
        rdevin::simulate(&event_type).map_err(|e| CoreError::SimulateFailed(format!("{e:?}")))
    }
}

fn from_rdevin_event(event: &rdevin::Event) -> Option<InputEvent> {
    Some(match event.event_type {
        rdevin::EventType::MouseMove { x, y } => InputEvent::MouseMove { x, y },
        rdevin::EventType::Wheel { delta_x, delta_y } => {
            InputEvent::MouseWheel { delta_x, delta_y }
        }
        rdevin::EventType::ButtonPress(b) => InputEvent::MouseButton {
            button: from_rdevin_button(b),
            pressed: true,
        },
        rdevin::EventType::ButtonRelease(b) => InputEvent::MouseButton {
            button: from_rdevin_button(b),
            pressed: false,
        },
        rdevin::EventType::KeyPress(k) => InputEvent::Key {
            key: from_rdevin_key(k, event.platform_code),
            pressed: true,
        },
        rdevin::EventType::KeyRelease(k) => InputEvent::Key {
            key: from_rdevin_key(k, event.platform_code),
            pressed: false,
        },
    })
}

fn to_rdevin_event_type(event: InputEvent) -> rdevin::EventType {
    match event {
        InputEvent::MouseMove { x, y } => rdevin::EventType::MouseMove { x, y },
        InputEvent::MouseWheel { delta_x, delta_y } => {
            rdevin::EventType::Wheel { delta_x, delta_y }
        }
        InputEvent::MouseButton {
            button,
            pressed: true,
        } => rdevin::EventType::ButtonPress(to_rdevin_button(button)),
        InputEvent::MouseButton {
            button,
            pressed: false,
        } => rdevin::EventType::ButtonRelease(to_rdevin_button(button)),
        InputEvent::Key { key, pressed: true } => rdevin::EventType::KeyPress(to_rdevin_key(key)),
        InputEvent::Key {
            key,
            pressed: false,
        } => rdevin::EventType::KeyRelease(to_rdevin_key(key)),
    }
}

fn from_rdevin_button(button: rdevin::Button) -> MouseButton {
    match button {
        rdevin::Button::Left => MouseButton::Left,
        rdevin::Button::Right => MouseButton::Right,
        rdevin::Button::Middle => MouseButton::Middle,
        rdevin::Button::Unknown(code) => MouseButton::Unknown(code),
    }
}

fn to_rdevin_button(button: MouseButton) -> rdevin::Button {
    match button {
        MouseButton::Left => rdevin::Button::Left,
        MouseButton::Right => rdevin::Button::Right,
        MouseButton::Middle => rdevin::Button::Middle,
        MouseButton::Unknown(code) => rdevin::Button::Unknown(code),
    }
}

/// `platform_code` берётся из внешнего `rdevin::Event` и используется только как
/// запасной идентификатор для клавиш, которых нет в нашем перечислении (см.
/// `Key::Unknown`) — благодаря ему такая клавиша всё равно корректно
/// воспроизводится той же ОС при симуляции (rdevin умеет отправлять "сырой" код).
fn from_rdevin_key(key: rdevin::Key, platform_code: u32) -> Key {
    use rdevin::Key as R;
    match key {
        R::Alt => Key::Alt,
        R::AltGr => Key::AltGr,
        R::Backspace => Key::Backspace,
        R::CapsLock => Key::CapsLock,
        R::ControlLeft => Key::ControlLeft,
        R::ControlRight => Key::ControlRight,
        R::Delete => Key::Delete,
        R::DownArrow => Key::DownArrow,
        R::End => Key::End,
        R::Escape => Key::Escape,
        R::F1 => Key::F1,
        R::F2 => Key::F2,
        R::F3 => Key::F3,
        R::F4 => Key::F4,
        R::F5 => Key::F5,
        R::F6 => Key::F6,
        R::F7 => Key::F7,
        R::F8 => Key::F8,
        R::F9 => Key::F9,
        R::F10 => Key::F10,
        R::F11 => Key::F11,
        R::F12 => Key::F12,
        R::F13 => Key::F13,
        R::F14 => Key::F14,
        R::F15 => Key::F15,
        R::F16 => Key::F16,
        R::F17 => Key::F17,
        R::F18 => Key::F18,
        R::F19 => Key::F19,
        R::F20 => Key::F20,
        R::F21 => Key::F21,
        R::F22 => Key::F22,
        R::F23 => Key::F23,
        R::F24 => Key::F24,
        R::Home => Key::Home,
        R::LeftArrow => Key::LeftArrow,
        R::MetaLeft => Key::MetaLeft,
        R::MetaRight => Key::MetaRight,
        R::PageDown => Key::PageDown,
        R::PageUp => Key::PageUp,
        R::Return => Key::Return,
        R::RightArrow => Key::RightArrow,
        R::ShiftLeft => Key::ShiftLeft,
        R::ShiftRight => Key::ShiftRight,
        R::Space => Key::Space,
        R::Tab => Key::Tab,
        R::UpArrow => Key::UpArrow,
        R::PrintScreen => Key::PrintScreen,
        R::ScrollLock => Key::ScrollLock,
        R::Pause => Key::Pause,
        R::NumLock => Key::NumLock,
        R::BackQuote => Key::BackQuote,
        R::Num0 => Key::Num0,
        R::Num1 => Key::Num1,
        R::Num2 => Key::Num2,
        R::Num3 => Key::Num3,
        R::Num4 => Key::Num4,
        R::Num5 => Key::Num5,
        R::Num6 => Key::Num6,
        R::Num7 => Key::Num7,
        R::Num8 => Key::Num8,
        R::Num9 => Key::Num9,
        R::Minus => Key::Minus,
        R::Equal => Key::Equal,
        R::KeyA => Key::KeyA,
        R::KeyB => Key::KeyB,
        R::KeyC => Key::KeyC,
        R::KeyD => Key::KeyD,
        R::KeyE => Key::KeyE,
        R::KeyF => Key::KeyF,
        R::KeyG => Key::KeyG,
        R::KeyH => Key::KeyH,
        R::KeyI => Key::KeyI,
        R::KeyJ => Key::KeyJ,
        R::KeyK => Key::KeyK,
        R::KeyL => Key::KeyL,
        R::KeyM => Key::KeyM,
        R::KeyN => Key::KeyN,
        R::KeyO => Key::KeyO,
        R::KeyP => Key::KeyP,
        R::KeyQ => Key::KeyQ,
        R::KeyR => Key::KeyR,
        R::KeyS => Key::KeyS,
        R::KeyT => Key::KeyT,
        R::KeyU => Key::KeyU,
        R::KeyV => Key::KeyV,
        R::KeyW => Key::KeyW,
        R::KeyX => Key::KeyX,
        R::KeyY => Key::KeyY,
        R::KeyZ => Key::KeyZ,
        R::LeftBracket => Key::LeftBracket,
        R::RightBracket => Key::RightBracket,
        R::SemiColon => Key::SemiColon,
        R::Quote => Key::Quote,
        R::BackSlash => Key::BackSlash,
        R::IntlBackslash => Key::IntlBackslash,
        R::Comma => Key::Comma,
        R::Dot => Key::Dot,
        R::Slash => Key::Slash,
        R::Insert => Key::Insert,
        R::KpReturn => Key::KpReturn,
        R::KpMinus => Key::KpMinus,
        R::KpPlus => Key::KpPlus,
        R::KpMultiply => Key::KpMultiply,
        R::KpDivide => Key::KpDivide,
        R::KpDecimal => Key::KpDecimal,
        R::KpEqual => Key::KpEqual,
        R::KpComma => Key::KpComma,
        R::Kp0 => Key::Kp0,
        R::Kp1 => Key::Kp1,
        R::Kp2 => Key::Kp2,
        R::Kp3 => Key::Kp3,
        R::Kp4 => Key::Kp4,
        R::Kp5 => Key::Kp5,
        R::Kp6 => Key::Kp6,
        R::Kp7 => Key::Kp7,
        R::Kp8 => Key::Kp8,
        R::Kp9 => Key::Kp9,
        R::VolumeUp => Key::VolumeUp,
        R::VolumeDown => Key::VolumeDown,
        R::VolumeMute => Key::VolumeMute,
        R::Function => Key::Function,
        R::Unknown(code) => Key::Unknown(code),
        // Редкие локаль-специфичные и legacy клавиши (IntlRo/IntlYen/KanaMode, Lang1-5,
        // Apps/Cancel/Clear/Kana/Hangul/Junja/Final/Hanja/Hanji/Print/Select/Execute/
        // Help/Sleep/Separator) и RawKey(_) сознательно не заведены как отдельные
        // варианты нашего Key — они редки для сценариев автоматизации кликов/скроллов.
        // Раскодированы через platform_code, поэтому при воспроизведении всё равно
        // отправится правильная физическая клавиша.
        _ => Key::Unknown(platform_code),
    }
}

fn to_rdevin_key(key: Key) -> rdevin::Key {
    use rdevin::Key as R;
    match key {
        Key::Alt => R::Alt,
        Key::AltGr => R::AltGr,
        Key::Backspace => R::Backspace,
        Key::CapsLock => R::CapsLock,
        Key::ControlLeft => R::ControlLeft,
        Key::ControlRight => R::ControlRight,
        Key::Delete => R::Delete,
        Key::DownArrow => R::DownArrow,
        Key::End => R::End,
        Key::Escape => R::Escape,
        Key::F1 => R::F1,
        Key::F2 => R::F2,
        Key::F3 => R::F3,
        Key::F4 => R::F4,
        Key::F5 => R::F5,
        Key::F6 => R::F6,
        Key::F7 => R::F7,
        Key::F8 => R::F8,
        Key::F9 => R::F9,
        Key::F10 => R::F10,
        Key::F11 => R::F11,
        Key::F12 => R::F12,
        Key::F13 => R::F13,
        Key::F14 => R::F14,
        Key::F15 => R::F15,
        Key::F16 => R::F16,
        Key::F17 => R::F17,
        Key::F18 => R::F18,
        Key::F19 => R::F19,
        Key::F20 => R::F20,
        Key::F21 => R::F21,
        Key::F22 => R::F22,
        Key::F23 => R::F23,
        Key::F24 => R::F24,
        Key::Home => R::Home,
        Key::LeftArrow => R::LeftArrow,
        Key::MetaLeft => R::MetaLeft,
        Key::MetaRight => R::MetaRight,
        Key::PageDown => R::PageDown,
        Key::PageUp => R::PageUp,
        Key::Return => R::Return,
        Key::RightArrow => R::RightArrow,
        Key::ShiftLeft => R::ShiftLeft,
        Key::ShiftRight => R::ShiftRight,
        Key::Space => R::Space,
        Key::Tab => R::Tab,
        Key::UpArrow => R::UpArrow,
        Key::PrintScreen => R::PrintScreen,
        Key::ScrollLock => R::ScrollLock,
        Key::Pause => R::Pause,
        Key::NumLock => R::NumLock,
        Key::BackQuote => R::BackQuote,
        Key::Num0 => R::Num0,
        Key::Num1 => R::Num1,
        Key::Num2 => R::Num2,
        Key::Num3 => R::Num3,
        Key::Num4 => R::Num4,
        Key::Num5 => R::Num5,
        Key::Num6 => R::Num6,
        Key::Num7 => R::Num7,
        Key::Num8 => R::Num8,
        Key::Num9 => R::Num9,
        Key::Minus => R::Minus,
        Key::Equal => R::Equal,
        Key::KeyA => R::KeyA,
        Key::KeyB => R::KeyB,
        Key::KeyC => R::KeyC,
        Key::KeyD => R::KeyD,
        Key::KeyE => R::KeyE,
        Key::KeyF => R::KeyF,
        Key::KeyG => R::KeyG,
        Key::KeyH => R::KeyH,
        Key::KeyI => R::KeyI,
        Key::KeyJ => R::KeyJ,
        Key::KeyK => R::KeyK,
        Key::KeyL => R::KeyL,
        Key::KeyM => R::KeyM,
        Key::KeyN => R::KeyN,
        Key::KeyO => R::KeyO,
        Key::KeyP => R::KeyP,
        Key::KeyQ => R::KeyQ,
        Key::KeyR => R::KeyR,
        Key::KeyS => R::KeyS,
        Key::KeyT => R::KeyT,
        Key::KeyU => R::KeyU,
        Key::KeyV => R::KeyV,
        Key::KeyW => R::KeyW,
        Key::KeyX => R::KeyX,
        Key::KeyY => R::KeyY,
        Key::KeyZ => R::KeyZ,
        Key::LeftBracket => R::LeftBracket,
        Key::RightBracket => R::RightBracket,
        Key::SemiColon => R::SemiColon,
        Key::Quote => R::Quote,
        Key::BackSlash => R::BackSlash,
        Key::IntlBackslash => R::IntlBackslash,
        Key::Comma => R::Comma,
        Key::Dot => R::Dot,
        Key::Slash => R::Slash,
        Key::Insert => R::Insert,
        Key::KpReturn => R::KpReturn,
        Key::KpMinus => R::KpMinus,
        Key::KpPlus => R::KpPlus,
        Key::KpMultiply => R::KpMultiply,
        Key::KpDivide => R::KpDivide,
        Key::KpDecimal => R::KpDecimal,
        Key::KpEqual => R::KpEqual,
        Key::KpComma => R::KpComma,
        Key::Kp0 => R::Kp0,
        Key::Kp1 => R::Kp1,
        Key::Kp2 => R::Kp2,
        Key::Kp3 => R::Kp3,
        Key::Kp4 => R::Kp4,
        Key::Kp5 => R::Kp5,
        Key::Kp6 => R::Kp6,
        Key::Kp7 => R::Kp7,
        Key::Kp8 => R::Kp8,
        Key::Kp9 => R::Kp9,
        Key::VolumeUp => R::VolumeUp,
        Key::VolumeDown => R::VolumeDown,
        Key::VolumeMute => R::VolumeMute,
        Key::Function => R::Function,
        // rdevin::simulate умеет отправить событие по сырому platform-коду и для
        // Unknown — это официальный, документированный в исходниках rdevin путь.
        Key::Unknown(code) => R::Unknown(code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_mapping_is_lossless_for_known_keys() {
        let sample = [Key::KeyA, Key::F12, Key::Space, Key::ControlLeft, Key::Kp5];
        for key in sample {
            let rdevin_key = to_rdevin_key(key);
            let back = from_rdevin_key(rdevin_key, 0);
            assert_eq!(key, back);
        }
    }

    #[test]
    fn unknown_key_roundtrips_via_platform_code() {
        let code = 12345u32;
        let rdevin_key = to_rdevin_key(Key::Unknown(code));
        assert!(matches!(rdevin_key, rdevin::Key::Unknown(c) if c == code));
    }

    #[test]
    fn button_mapping_roundtrips() {
        for button in [
            MouseButton::Left,
            MouseButton::Right,
            MouseButton::Middle,
            MouseButton::Unknown(7),
        ] {
            let mapped = to_rdevin_button(button);
            assert_eq!(from_rdevin_button(mapped), button);
        }
    }
}
