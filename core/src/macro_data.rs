//! Модель макроса: последовательность шагов + метаданные.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::event::InputEvent;
use crate::screen_guard::{ScreenFingerprint, ScreenThumbnail};

/// Один шаг макроса: событие ввода + пауза перед ним.
///
/// `delay_ms` — это задержка ПЕРЕД событием, отсчитанная от предыдущего события
/// (а для самого первого шага — от начала воспроизведения). Так удобнее и для
/// записи (пришло новое событие — знаем дельту от прошлого), и для редактора шагов
/// в UI (пользователь правит "подождать N мс перед этим кликом").
///
/// Больше не `Copy` (раньше был) — миниатюра экрана внутри хранит `Vec<u8>`.
/// Код, который раньше полагался на копирование по значению, теперь должен
/// явно `.clone()`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MacroStep {
    pub event: InputEvent,
    pub delay_ms: u64,
    /// Отпечаток области экрана вокруг курсора в момент клика — заполняется,
    /// только если при записи был включён screen-guard. `#[serde(default)]`
    /// специально: старые сохранённые макросы без этого поля должны и дальше
    /// нормально загружаться.
    #[serde(default)]
    pub screen_fingerprint: Option<ScreenFingerprint>,
    /// Маленькое превью того же снимка, из которого посчитан отпечаток выше —
    /// только для показа в редакторе шагов, движок воспроизведения его не
    /// использует. Тоже `#[serde(default)]` ради обратной совместимости.
    #[serde(default)]
    pub screen_thumbnail: Option<ScreenThumbnail>,
}

impl MacroStep {
    /// Шаг без отпечатка экрана (обычный случай, если screen-guard выключен).
    pub fn new(event: InputEvent, delay_ms: u64) -> Self {
        Self {
            event,
            delay_ms,
            screen_fingerprint: None,
            screen_thumbnail: None,
        }
    }

    pub fn with_fingerprint(
        event: InputEvent,
        delay_ms: u64,
        fingerprint: ScreenFingerprint,
    ) -> Self {
        Self {
            event,
            delay_ms,
            screen_fingerprint: Some(fingerprint),
            screen_thumbnail: None,
        }
    }

    /// То же самое, но также сохраняет и превью — обычный случай при реальной
    /// записи со включённым screen-guard (оба считаются из одного снимка).
    pub fn with_fingerprint_and_thumbnail(
        event: InputEvent,
        delay_ms: u64,
        fingerprint: ScreenFingerprint,
        thumbnail: ScreenThumbnail,
    ) -> Self {
        Self {
            event,
            delay_ms,
            screen_fingerprint: Some(fingerprint),
            screen_thumbnail: Some(thumbnail),
        }
    }
}

/// Сколько раз воспроизводить макрос.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LoopMode {
    #[default]
    Once,
    Times(u32),
    /// Бесконечно, пока пользователь не остановит воспроизведение вручную (хоткей/кнопка).
    Infinite,
}

/// Именованный, сохраняемый сценарий.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Macro {
    pub id: Uuid,
    pub name: String,
    pub steps: Vec<MacroStep>,
    /// `#[serde(default)]` — та же страховка, что у полей шага: файл,
    /// сохранённый до появления этой фичи или отредактированный вручную
    /// без этого поля, загрузится как `LoopMode::Once`, а не упадёт.
    #[serde(default)]
    pub loop_mode: LoopMode,
    pub created_at: DateTime<Utc>,
}

impl Macro {
    /// Создаёт новый макрос из только что записанных шагов.
    pub fn new(name: impl Into<String>, steps: Vec<MacroStep>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            steps,
            loop_mode: LoopMode::default(),
            created_at: Utc::now(),
        }
    }

    /// Суммарная длительность одного прохода макроса.
    pub fn duration_ms(&self) -> u64 {
        self.steps.iter().map(|s| s.delay_ms).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Key, MouseButton};

    fn sample_steps() -> Vec<MacroStep> {
        vec![
            MacroStep::new(InputEvent::MouseMove { x: 10.0, y: 10.0 }, 0),
            MacroStep::new(
                InputEvent::MouseButton {
                    button: MouseButton::Left,
                    pressed: true,
                },
                120,
            ),
            MacroStep::new(
                InputEvent::Key {
                    key: Key::KeyA,
                    pressed: true,
                },
                300,
            ),
        ]
    }

    #[test]
    fn duration_is_sum_of_delays() {
        let m = Macro::new("test", sample_steps());
        assert_eq!(m.duration_ms(), 120 + 300);
    }

    #[test]
    fn serializes_to_stable_json_shape() {
        let m = Macro::new("test", sample_steps());
        let json = serde_json::to_string_pretty(&m).unwrap();
        let restored: Macro = serde_json::from_str(&json).unwrap();
        assert_eq!(m, restored);
    }

    #[test]
    fn loop_mode_with_payload_survives_json_roundtrip() {
        let mut m = Macro::new("test", sample_steps());
        m.loop_mode = LoopMode::Times(5);

        let json = serde_json::to_string_pretty(&m).unwrap();
        let restored: Macro = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.loop_mode, LoopMode::Times(5));
    }
}
