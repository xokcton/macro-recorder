//! Тестовая замена реального захвата экрана: отдаёт заранее подготовленные снимки.

use std::sync::Mutex;

use super::{RegionSnapshot, ScreenGuard};
use crate::error::CoreError;

pub struct MockScreenGuard {
    current: Mutex<RegionSnapshot>,
}

impl MockScreenGuard {
    pub fn new(initial: RegionSnapshot) -> Self {
        Self {
            current: Mutex::new(initial),
        }
    }

    /// Подменяет снимок, который вернёт следующий вызов `capture_region` —
    /// используется в тестах, чтобы имитировать "экран изменился".
    pub fn set(&self, snapshot: RegionSnapshot) {
        *self.current.lock().unwrap() = snapshot;
    }
}

impl ScreenGuard for MockScreenGuard {
    fn capture_region(&self, _x: f64, _y: f64, _size: u32) -> Result<RegionSnapshot, CoreError> {
        Ok(self.current.lock().unwrap().clone())
    }
}
