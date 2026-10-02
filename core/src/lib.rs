//! `mrcore` — платформонезависимое ядро macro recorder/player.
//!
//! Ничего не знает про GUI. Всё, что касается конкретной ОС, изолировано в
//! `backend` (ввод) — при появлении screen-guard'а (см. ROADMAP) то же самое
//! правило будет применяться и к нему.

pub mod backend;
pub mod error;
pub mod event;
pub mod hotkeys;
pub mod macro_data;
pub mod player;
pub mod recorder;
pub mod screen_guard;
pub mod settings;
pub mod storage;
pub mod timing;

pub use error::CoreError;
pub use event::{InputEvent, Key, MouseButton};
pub use hotkeys::{HotkeyAction, HotkeyBinding};
pub use macro_data::{LoopMode, Macro, MacroStep};
pub use player::{PlaybackHandle, PlaybackWarning, Player};
pub use recorder::Recorder;
pub use screen_guard::{ScreenCaptureConfig, ScreenFingerprint, ScreenGuard, ScreenThumbnail};
pub use settings::{InputBackendChoice, Settings, SettingsStore};
pub use storage::{atomic_write, MacroSummary, Storage};
