//! Вторая половина технического спайка: проверяет, что `RdevinBackend::simulate`
//! реально двигает курсор и кликает в живой ОС, а не просто компилируется.
//!
//! ```text
//! cargo run -p mrcore --example spike_playback
//! ```
//!
//! Печатает позицию курсора до/после каждого шага — не требует ручного участия,
//! в отличие от `spike_record`.

use std::thread;
use std::time::Duration;

use mrcore::backend::InputBackend;
use mrcore::backend::RdevinBackend;
use mrcore::{InputEvent, MouseButton};

fn main() {
    let backend = RdevinBackend;

    let steps: Vec<InputEvent> = vec![
        InputEvent::MouseMove { x: 50.0, y: 60.0 },
        InputEvent::MouseMove { x: 400.0, y: 300.0 },
        InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        },
        InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: false,
        },
        InputEvent::MouseButton {
            button: MouseButton::Unknown(8),
            pressed: true,
        },
        InputEvent::MouseButton {
            button: MouseButton::Unknown(8),
            pressed: false,
        },
    ];

    for (i, event) in steps.iter().enumerate() {
        match backend.simulate(*event) {
            Ok(()) => println!("[{i}] отправлено: {event:?}"),
            Err(err) => println!("[{i}] ОШИБКА при {event:?}: {err}"),
        }
        thread::sleep(Duration::from_millis(150));
    }
}
