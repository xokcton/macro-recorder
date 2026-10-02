//! Мост между Cargo-фичей `screen-guard` (см. `app/Cargo.toml` и
//! `core/Cargo.toml`) и остальным приложением: единственное место, которое
//! обращается к `XcapScreenGuard` напрямую.
//!
//! Как и `tray` (см. `tray.rs`), модуль экспортирует одну и ту же публичную
//! функцию в обеих сборках — [`capability`] — просто в сборке без фичи она
//! всегда возвращает `None`. Так `app.rs` не разбросан `#[cfg(...)]`-ами:
//! отсутствие фичи выглядит так же, как "в этой сборке снимать экран
//! технически нечем" — состояние, с которым остальной код (см.
//! `ScreenCaptureConfig`) уже умеет жить и без сборки с этой фичей.

use std::sync::Arc;

use mrcore::ScreenGuard;

#[cfg(feature = "screen-guard")]
pub fn capability() -> Option<Arc<dyn ScreenGuard>> {
    Some(Arc::new(mrcore::screen_guard::xcap_guard::XcapScreenGuard))
}

#[cfg(not(feature = "screen-guard"))]
pub fn capability() -> Option<Arc<dyn ScreenGuard>> {
    None
}
