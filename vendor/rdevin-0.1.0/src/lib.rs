//! Cross-platform simulation and global listening for keyboard and mouse input.
//!
//! <div class="warning">This crate is subject to extreme change. There is still great room for improvement. It is only presently published for use by <a href="https://github.com/justdeeevin/nuhxboard">NuhxBoard</a>.</div>
//!
//! ## Listening for input
//!
//! The [`listen`] and [`grab`][^1] functions can be used to run a callback for all input events.
//!
//! ```no_run
//! rdevin::listen(|e| dbg!(e))?;
//! ```
//!
//! ## Simulating input
//!
//! The [`simulate`] function can be used to send input events.
//!
//! ```no_run
//! use rdevin::{simulate, EventType, Key};
//!
//! simulate(&EventType::KeyPress(Key::KeyS))?;
//! ```
//!
//! ## Serialization
//!
//! Serde support is gated behind the `serde` feature.
//!
//! ## Acknowledgements
//! - This crate is a fork of a fork of a fork of [Narsil's `rdev`
//!   crate](https://crates.io/crates/rdev), created to ensure continued maintenance and to make
//!   Rustdesk's many useful additions available on crates.io.
//! - [Enigo](https://github.com/Enigo-rs/Enigo), an input simulation library, served as inspiration and reference for Narsil's original crate.
//!
//! [^1]: Not available on Linux

mod rdevin;
pub use crate::rdevin::{
    Button, DisplayError, Event, EventType, GrabError, Key, KeyCode, KeyboardState, RawKey,
    SimulateError, UnicodeInfo,
};

/// Different OSes use different numererical representations for keys. Functions within this module
/// provide simple, reliable conversions between the [`Key`] enum and OS-specific keycodes.
pub mod keycodes;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

pub mod codes_conv;

#[cfg(target_os = "macos")]
pub use crate::keycodes::macos::{code_from_key, key_from_code};
#[cfg(target_os = "macos")]
use crate::macos::{
    display_size as _display_size, grab as _grab, listen as _listen, simulate as _simulate,
};
#[cfg(target_os = "macos")]
pub use crate::macos::{Keyboard, ListenError};

#[cfg(any(target_os = "android", target_os = "linux"))]
pub use crate::keycodes::linux::{code_from_key, key_from_code};
#[cfg(target_os = "linux")]
use crate::linux::{display_size as _display_size, listen as _listen, simulate as _simulate};
#[cfg(target_os = "linux")]
pub use crate::linux::{Keyboard, ListenError};

#[cfg(target_os = "windows")]
pub use crate::keycodes::windows::{code_from_key, key_from_code};
#[cfg(target_os = "windows")]
use crate::windows::{
    display_size as _display_size, grab as _grab, listen as _listen, simulate as _simulate,
};
#[cfg(target_os = "windows")]
pub use crate::windows::{Keyboard, ListenError};

/// React to global input events.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn listen<T>(callback: T) -> Result<(), ListenError>
where
    T: FnMut(Event) + 'static,
{
    _listen(callback)
}

/// Simulate an input event.
///
/// # Example
///
/// ```no_run
/// use rdevin::{simulate, Button, EventType, Key};
///
/// simulate(&EventType::KeyPress(Key::KeyS));
/// simulate(&EventType::KeyRelease(Key::KeyS));
///
/// simulate(&EventType::MouseMove { x: 0.0, y: 0.0 });
/// simulate(&EventType::MouseMove { x: 400.0, y: 400.0 });
/// simulate(&EventType::ButtonPress(Button::Left));
/// simulate(&EventType::ButtonRelease(Button::Right));
/// simulate(&EventType::Wheel {
///     delta_x: 0,
///     delta_y: 1,
/// });
/// ```
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn simulate(event_type: &EventType) -> Result<(), SimulateError> {
    _simulate(event_type)
}

/// Returns the size in pixels of the main screen.
///
/// First tuple item is width, second is height.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn display_size() -> Result<(u64, u64), DisplayError> {
    _display_size()
}

/// React to global input events, optionally preventing the event from being sent to applications.
///
/// The callback can return `None` to ignore the event, or the event it was given to allow it to pass. **The event cannot be modified.**
///
/// # Example
///
/// ```no_run
/// use rdevin::{grab, Event, EventType, Key};
///
/// fn callback(event: Event) -> Option<Event> {
///     println!("My callback {:?}", event);
///     match event.event_type{
///         EventType::KeyPress(Key::Tab) => None,
///         _ => Some(event),
///     }
/// }
/// fn main(){
///     if let Err(error) = grab(callback) {
///         println!("Error: {:?}", error)
///     }
/// }
/// ```
#[cfg(not(any(target_os = "android", target_os = "ios", target_os = "linux")))]
pub fn grab<T>(callback: T) -> Result<(), GrabError>
where
    T: Fn(Event) -> Option<Event> + 'static,
{
    _grab(callback)
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub(crate) fn keyboard_only() -> bool {
    !std::env::var("KEYBOARD_ONLY")
        .unwrap_or_default()
        .is_empty()
}
