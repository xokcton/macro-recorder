//! Тестовый бэкенд: не трогает реальную ОС, позволяет тестам "подсовывать"
//! события ввода вручную и проверять, что именно было бы отправлено на симуляцию.

use std::sync::{Arc, Condvar, Mutex};

use super::InputBackend;
use crate::error::CoreError;
use crate::event::InputEvent;

type Callback = Box<dyn FnMut(InputEvent) + Send>;

#[derive(Default)]
struct Inner {
    callback: Option<Callback>,
    released: bool,
    /// Если задано — следующий `listen()` вернёт ошибку с этой причиной
    /// вместо регистрации колбэка (имитация Wayland и прочей недоступности).
    fail_listen: Option<String>,
}

#[derive(Default)]
pub struct MockBackend {
    inner: Mutex<Inner>,
    cv: Condvar,
    pub simulated: Mutex<Vec<InputEvent>>,
}

impl MockBackend {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Имитирует поступление события от ОС. Блокируется, пока `listen()` не
    /// зарегистрирует callback (обычно это доли миллисекунды после старта записи).
    pub fn feed(&self, event: InputEvent) {
        let mut guard = self.inner.lock().unwrap();
        while guard.callback.is_none() {
            guard = self.cv.wait(guard).unwrap();
        }
        if let Some(cb) = guard.callback.as_mut() {
            cb(event);
        }
    }

    /// Заставляет `listen()` провалиться с заданной причиной — см. тесты
    /// `listen_failure_*`.
    pub fn fail_listen(&self, reason: &str) {
        self.inner.lock().unwrap().fail_listen = Some(reason.to_string());
    }

    /// Отпускает блокирующий `listen()`. У настоящего `rdevin` такого рычага нет —
    /// это условность теста, а не то, что должен уметь production-код.
    pub fn release(&self) {
        let mut guard = self.inner.lock().unwrap();
        guard.released = true;
        self.cv.notify_all();
    }
}

impl InputBackend for MockBackend {
    fn listen(&self, callback: Callback) -> Result<(), CoreError> {
        {
            let mut guard = self.inner.lock().unwrap();
            if let Some(reason) = guard.fail_listen.take() {
                return Err(CoreError::ListenFailed(reason));
            }
            guard.callback = Some(callback);
            self.cv.notify_all();
        }
        let mut guard = self.inner.lock().unwrap();
        while !guard.released {
            guard = self.cv.wait(guard).unwrap();
        }
        Ok(())
    }

    fn simulate(&self, event: InputEvent) -> Result<(), CoreError> {
        self.simulated.lock().unwrap().push(event);
        Ok(())
    }
}
