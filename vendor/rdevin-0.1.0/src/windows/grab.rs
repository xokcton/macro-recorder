use crate::{
    rdevin::{Event, EventType, GrabError},
    windows::common::{convert, get_scan_code, HookError, KEYBOARD},
};
use std::{io::Error as IoError, ptr::null_mut, sync::Mutex, time::SystemTime};
use winapi::{
    shared::{
        basetsd::ULONG_PTR,
        minwindef::{DWORD, FALSE},
        ntdef::NULL,
        windef::{HHOOK, POINT},
    },
    um::{
        errhandlingapi::GetLastError,
        processthreadsapi::GetCurrentThreadId,
        winuser::{
            CallNextHookEx, DispatchMessageA, GetMessageA, PostThreadMessageA, SetWindowsHookExA,
            TranslateMessage, UnhookWindowsHookEx, HC_ACTION, MSG, PKBDLLHOOKSTRUCT,
            PMOUSEHOOKSTRUCT, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_USER,
        },
    },
};

static mut GLOBAL_CALLBACK: Option<Box<dyn FnMut(Event) -> Option<Event>>> = None;
static mut GET_KEY_UNICODE: bool = true;

lazy_static::lazy_static! {
    static ref CUR_HOOK_THREAD_ID: Mutex<DWORD> = Mutex::new(0);
}

const WM_USER_EXIT_HOOK: u32 = WM_USER + 1;

pub fn set_get_key_unicode(b: bool) {
    unsafe {
        GET_KEY_UNICODE = b;
    }
}

pub fn set_event_popup(b: bool) {
    KEYBOARD.lock().unwrap().set_event_popup(b);
}

unsafe fn raw_callback(
    code: i32,
    param: usize,
    lpdata: isize,
    f_get_extra_data: impl FnOnce(isize) -> ULONG_PTR,
) -> isize {
    if code == HC_ACTION {
        let (opt, code) = convert(param, lpdata);
        if let Some(event_type) = opt {
            let unicode = if GET_KEY_UNICODE {
                match &event_type {
                    EventType::KeyPress(_key) => match (*KEYBOARD).lock() {
                        Ok(mut keyboard) => keyboard.get_unicode(lpdata),
                        Err(_) => None,
                    },
                    _ => None,
                }
            } else {
                None
            };
            let event = Event {
                event_type,
                time: SystemTime::now(),
                unicode,
                platform_code: code as _,
                position_code: get_scan_code(lpdata),
                usb_hid: 0,
                extra_data: f_get_extra_data(lpdata),
            };
            if let Some(callback) = &mut GLOBAL_CALLBACK {
                if callback(event).is_none() {
                    // https://stackoverflow.com/questions/42756284/blocking-windows-mouse-click-using-setwindowshookex
                    // https://android.developreference.com/article/14560004/Blocking+windows+mouse+click+using+SetWindowsHookEx()
                    // https://cboard.cprogramming.com/windows-programming/99678-setwindowshookex-wm_keyboard_ll.html
                    // let _result = CallNextHookEx(hhk, code, param, lpdata);
                    return 1;
                }
            }
        }
    }
    CallNextHookEx(null_mut(), code, param, lpdata)
}

unsafe extern "system" fn raw_callback_mouse(code: i32, param: usize, lpdata: isize) -> isize {
    raw_callback(code, param, lpdata, |data: isize| unsafe {
        (*(data as PMOUSEHOOKSTRUCT)).dwExtraInfo
    })
}

unsafe extern "system" fn raw_callback_keyboard(code: i32, param: usize, lpdata: isize) -> isize {
    raw_callback(code, param, lpdata, |data: isize| unsafe {
        (*(data as PKBDLLHOOKSTRUCT)).dwExtraInfo
    })
}

fn do_hook<T>(callback: T) -> Result<(HHOOK, HHOOK), GrabError>
where
    T: FnMut(Event) -> Option<Event> + 'static,
{
    let mut cur_hook_thread_id = CUR_HOOK_THREAD_ID.lock().unwrap();
    if *cur_hook_thread_id != 0 {
        // already hooked
        return Ok((null_mut(), null_mut()));
    }

    let hook_keyboard;
    let mut hook_mouse = null_mut();
    unsafe {
        GLOBAL_CALLBACK = Some(Box::new(callback));
        hook_keyboard =
            SetWindowsHookExA(WH_KEYBOARD_LL, Some(raw_callback_keyboard), null_mut(), 0);
        if hook_keyboard.is_null() {
            return Err(HookError::Key(GetLastError()).into());
        }

        if !crate::keyboard_only() {
            hook_mouse = SetWindowsHookExA(WH_MOUSE_LL, Some(raw_callback_mouse), null_mut(), 0);
            if hook_mouse.is_null() {
                if FALSE == UnhookWindowsHookEx(hook_keyboard) {
                    // Fatal error
                    log::error!("UnhookWindowsHookEx keyboard {}", IoError::last_os_error());
                }
                return Err(HookError::Mouse(GetLastError()).into());
            }
        }
        *cur_hook_thread_id = GetCurrentThreadId();
    }

    Ok((hook_keyboard, hook_mouse))
}

#[inline]
pub fn is_grabbed() -> bool {
    *CUR_HOOK_THREAD_ID.lock().unwrap() != 0
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Error disabling hook: {0}")]
    ExitGrabError(u32),
    #[error("Error setting hook")]
    Hook(#[from] HookError),
}

pub fn grab<T>(callback: T) -> Result<(), GrabError>
where
    T: FnMut(Event) -> Option<Event> + 'static,
{
    if is_grabbed() {
        return Ok(());
    }

    unsafe {
        let (mut hook_keyboard, hook_mouse) = do_hook(callback)?;
        if hook_keyboard.is_null() && hook_mouse.is_null() {
            return Ok(());
        }

        let mut msg = MSG {
            hwnd: NULL as _,
            message: 0 as _,
            wParam: 0 as _,
            lParam: 0 as _,
            time: 0 as _,
            pt: POINT {
                x: 0 as _,
                y: 0 as _,
            },
        };
        while FALSE != GetMessageA(&mut msg, NULL as _, 0, 0) {
            if msg.message == WM_USER_EXIT_HOOK {
                if !hook_keyboard.is_null() {
                    if FALSE == UnhookWindowsHookEx(hook_keyboard as _) {
                        log::error!(
                            "Failed UnhookWindowsHookEx keyboard {}",
                            IoError::last_os_error()
                        );
                        continue;
                    }
                    hook_keyboard = null_mut();
                }

                if !hook_mouse.is_null() && FALSE == UnhookWindowsHookEx(hook_mouse as _) {
                    log::error!(
                        "Failed UnhookWindowsHookEx mouse {}",
                        IoError::last_os_error()
                    );
                    continue;
                }
                break;
            }

            TranslateMessage(&msg);
            DispatchMessageA(&msg);
        }

        *CUR_HOOK_THREAD_ID.lock().unwrap() = 0;
    }
    Ok(())
}

pub fn exit_grab() -> Result<(), GrabError> {
    unsafe {
        let mut cur_hook_thread_id = CUR_HOOK_THREAD_ID.lock().unwrap();
        if *cur_hook_thread_id != 0
            && FALSE == PostThreadMessageA(*cur_hook_thread_id, WM_USER_EXIT_HOOK, 0, 0)
        {
            return Err(Error::ExitGrabError(GetLastError()).into());
        }
        *cur_hook_thread_id = 0;
    }
    Ok(())
}
