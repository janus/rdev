extern crate winapi;

mod common;
mod display;
#[cfg(feature = "grab")]
mod grab;
mod keycodes;
mod listen;
mod simulate;

pub use crate::windows::display::display_size;
#[cfg(feature = "grab")]
pub use crate::windows::grab::grab;
pub use crate::windows::listen::listen;
pub use crate::windows::simulate::simulate;
