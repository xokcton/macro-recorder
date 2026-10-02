//! Абстракция над платформенным вводом.
//!
//! `Recorder` и `Player` работают только через этот trait и никогда не видят
//! `rdevin` напрямую. Две причины:
//! 1. Тестируемость — в тестах подставляется `mock_backend`, не трогая реальную ОС.
//! 2. Изоляция риска — `rdevin` открыто предупреждает, что его API может сильно
//!    измениться; если это случится, переписывать нужно только `rdevin_backend.rs`.

use crate::error::CoreError;
use crate::event::InputEvent;

#[cfg(test)]
pub mod mock_backend;
pub mod rdevin_backend;
// Экспериментальный бэкенд прямого чтения /dev/input — единственный путь
// записи ввода на чистом Wayland. Собирается только на Linux и только
// под явной фичей, чтобы дефолтная сборка оставалась прежней.
#[cfg(all(target_os = "linux", feature = "evdev"))]
pub mod evdev_backend;

#[cfg(all(target_os = "linux", feature = "evdev"))]
pub use evdev_backend::EvdevBackend;
pub use rdevin_backend::RdevinBackend;

/// Источник и приёмник событий ввода уровня ОС.
pub trait InputBackend: Send + Sync + 'static {
    /// Подписывается на глобальные события ввода и вызывает `callback` для каждого.
    ///
    /// # Важно
    /// Это блокирующий вызов, который на большинстве бэкендов не возвращается,
    /// пока не произойдёт фатальная ошибка (у `rdevin`, как и у большинства
    /// подобных библиотек, нет публичного способа остановить хук программно).
    /// Поэтому вызывающая сторона (см. `Recorder`) запускает его один раз в
    /// отдельном потоке на всё время жизни приложения и управляет тем,
    /// "слушать по-настоящему" или нет, через сам `callback`, а не через
    /// остановку этого вызова.
    fn listen(&self, callback: Box<dyn FnMut(InputEvent) + Send>) -> Result<(), CoreError>;

    /// Отправляет одно событие ввода в ОС (для воспроизведения макроса).
    fn simulate(&self, event: InputEvent) -> Result<(), CoreError>;
}
