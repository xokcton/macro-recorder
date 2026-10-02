//! Мост между настройкой `Settings::input_backend` и конкретным типом
//! бэкенда для `Recorder`/`Player`.
//!
//! Ключевой приём — тип-алиас [`AppBackend`]: пока фича `evdev` выключена,
//! это просто псевдоним `RdevinBackend`, и весь код приложения компилируется
//! так же, как раньше; при включённой фиче это перечисление-обёртка,
//! диспетчеризующее вызовы в выбранный бэкенд. Ни одна сигнатура в `app.rs`,
//! кроме точки создания бэкенда, от выбора не зависит.

use mrcore::backend::RdevinBackend;
use mrcore::InputBackendChoice;
// Всё, что нужно только evdev-обёртке, — под тем же cfg, что и она сама:
// без фичи ни trait, ни типы событий здесь не используются.
#[cfg(all(target_os = "linux", feature = "evdev"))]
use mrcore::backend::{EvdevBackend, InputBackend};
#[cfg(all(target_os = "linux", feature = "evdev"))]
use mrcore::{CoreError, InputEvent};

/// Тип бэкенда, которым параметризованы `Recorder`/`Player` в приложении.
///
/// Без фичи `evdev` выбор деградирует: `build_backend` вернёт rdevin
/// и запишет предупреждение в лог.
#[cfg(not(all(target_os = "linux", feature = "evdev")))]
pub type AppBackend = RdevinBackend;

/// С фичей `evdev` — настоящая обёртка над двумя реализациями.
#[cfg(all(target_os = "linux", feature = "evdev"))]
#[derive(Debug)]
pub enum AppBackend {
    Rdevin(RdevinBackend),
    Evdev(EvdevBackend),
}

#[cfg(all(target_os = "linux", feature = "evdev"))]
impl InputBackend for AppBackend {
    fn listen(&self, callback: Box<dyn FnMut(InputEvent) + Send>) -> Result<(), CoreError> {
        match self {
            Self::Rdevin(backend) => backend.listen(callback),
            Self::Evdev(backend) => backend.listen(callback),
        }
    }

    fn simulate(&self, event: InputEvent) -> Result<(), CoreError> {
        match self {
            Self::Rdevin(backend) => backend.simulate(event),
            Self::Evdev(backend) => backend.simulate(event),
        }
    }
}

/// Строит бэкенд по сохранённой настройке. Единственное место в приложении,
/// где знание «какой бэкенд реален» сходится с настройкой пользователя.
pub fn build_backend(choice: InputBackendChoice) -> AppBackend {
    #[cfg(all(target_os = "linux", feature = "evdev"))]
    match choice {
        InputBackendChoice::Rdevin => AppBackend::Rdevin(RdevinBackend),
        InputBackendChoice::Evdev => AppBackend::Evdev(EvdevBackend::default()),
    }

    #[cfg(not(all(target_os = "linux", feature = "evdev")))]
    {
        if choice == InputBackendChoice::Evdev {
            tracing::warn!(
                "в настройках выбран evdev-бэкенд, но эта сборка собрана без \
                 фичи evdev — использую rdevin"
            );
        }
        RdevinBackend
    }
}

/// Похоже ли, что сессия — чистый Wayland. Стандартная эвристика по окружению:
/// `WAYLAND_DISPLAY` выставляет композитор, `XDG_SESSION_TYPE` — менеджер
/// сессии. На X11/XWayland обе либо молчат, либо говорят «x11».
pub fn is_wayland_session() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        || matches!(std::env::var("XDG_SESSION_TYPE").as_deref(), Ok("wayland"))
}
