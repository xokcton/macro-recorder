#![allow(static_mut_refs)]

// TODO: How TF does any of this work?

extern crate libc;
extern crate x11;

mod common;
mod display;
mod grab;
mod keyboard;
mod listen;
mod simulate;

pub use crate::linux::display::display_size;
pub use crate::linux::grab::{
    disable_grab, enable_grab, exit_grab_listen, is_grabbed, start_grab_listen, Error as GrabError,
};
pub use crate::linux::keyboard::Keyboard;
pub use crate::linux::listen::{listen, ListenError};
pub use crate::linux::simulate::{simulate, simulate_char, simulate_unicode};
