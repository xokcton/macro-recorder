#![allow(static_mut_refs)]

extern crate winapi;

mod common;
mod display;
mod grab;
mod keyboard;
mod listen;
mod simulate;

pub use crate::windows::common::*;
pub use crate::windows::display::display_size;
pub use crate::windows::grab::{
    exit_grab, grab, is_grabbed, set_event_popup, set_get_key_unicode, Error as GrabError,
};
pub use crate::windows::keyboard::Keyboard;
pub use crate::windows::listen::{listen, ListenError};
pub use crate::windows::simulate::*;
