//! Реализация [`ScreenGuard`] поверх крейта `xcap`.
//!
//! ⚠️ Честно о состоянии этого файла: API сверен напрямую по исходникам `xcap`
//! 0.9.6 (не только по документации) — `Monitor::from_point`, `.x()`/`.y()`/
//! `.width()`/`.height()`, `.capture_region(x, y, w, h) -> XCapResult<RgbaImage>`
//! существуют и выглядят именно так. НО в этой песочнице сам файл не собран:
//! на Linux `xcap` тянет `rand 0.9` → цепочку до `zbus`/актуального `uuid`,
//! чья версия требует Cargo 1.85+ (edition2024), а здесь доступен только
//! системный Cargo 1.75 без интернет-доступа к rustup. На машине с современным
//! Rust это должно собраться как есть — но именно поэтому первая команда,
//! которую стоит прогнать после клонирования, это
//! `cargo check -p mrcore --features screen-guard`.

use xcap::Monitor;

use super::{RegionSnapshot, ScreenGuard};
use crate::error::CoreError;

fn capture_err(e: impl std::fmt::Display) -> CoreError {
    CoreError::ScreenCaptureFailed(e.to_string())
}

/// `ScreenGuard` на реальном экране — то, что использует `Recorder`/`Player` в
/// собранном с фичей `screen-guard` приложении.
#[derive(Debug, Default, Clone, Copy)]
pub struct XcapScreenGuard;

impl ScreenGuard for XcapScreenGuard {
    fn capture_region(&self, x: f64, y: f64, size: u32) -> Result<RegionSnapshot, CoreError> {
        let half = (size / 2) as i32;
        let global_x = x.round() as i32;
        let global_y = y.round() as i32;

        let monitor = Monitor::from_point(global_x, global_y).map_err(capture_err)?;
        let mon_x = monitor.x().map_err(capture_err)?;
        let mon_y = monitor.y().map_err(capture_err)?;
        let mon_w = monitor.width().map_err(capture_err)?;
        let mon_h = monitor.height().map_err(capture_err)?;

        // capture_region у xcap принимает координаты, относительные конкретному
        // монитору (u32) — переводим глобальные экранные координаты клика в них
        // и обрезаем область, чтобы не выйти за границы монитора.
        let local_x = (global_x - mon_x - half).max(0) as u32;
        let local_y = (global_y - mon_y - half).max(0) as u32;
        let local_x = local_x.min(mon_w.saturating_sub(1));
        let local_y = local_y.min(mon_h.saturating_sub(1));
        let width = size.min(mon_w.saturating_sub(local_x)).max(1);
        let height = size.min(mon_h.saturating_sub(local_y)).max(1);

        let image = monitor
            .capture_region(local_x, local_y, width, height)
            .map_err(capture_err)?;

        Ok(RegionSnapshot {
            width: image.width(),
            height: image.height(),
            rgba: image.into_raw(),
        })
    }
}
