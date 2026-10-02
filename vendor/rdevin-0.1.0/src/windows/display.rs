use crate::rdevin::DisplayError;
use std::convert::TryInto;
use winapi::um::winuser::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

pub fn display_size() -> Result<(u64, u64), DisplayError> {
    let w = unsafe { GetSystemMetrics(SM_CXSCREEN).try_into()? };
    let h = unsafe { GetSystemMetrics(SM_CYSCREEN).try_into()? };
    Ok((w, h))
}
