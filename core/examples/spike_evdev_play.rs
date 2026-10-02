//! Ручной спайк ВОСПРОИЗВЕДЕНИЯ evdev-бэкенда (фаза W2 Wayland-поддержки).
//!
//! ```bash
//! cargo run -p mrcore --features evdev --example spike_evdev_play
//! ```
//!
//! Создаёт виртуальное устройство и: рисует курсором квадрат, щёлкает,
//! крутит колесо и «печатает» слово. Работает и под чистым Wayland.
//! Нужна запись в /dev/uinput (обычно группа `input`).

use std::thread::sleep;
use std::time::Duration;

use mrcore::backend::{EvdevBackend, InputBackend};
use mrcore::{InputEvent, Key, MouseButton};

fn main() {
    let backend = EvdevBackend::default();
    let move_to = |x: f64, y: f64| {
        backend
            .simulate(InputEvent::MouseMove { x, y })
            .expect("симуляция движения")
    };

    println!("1/4 рисую квадрат курсором...");
    let corners = [
        (300.0, 300.0),
        (600.0, 300.0),
        (600.0, 600.0),
        (300.0, 600.0),
    ];
    move_to(corners[0].0, corners[0].1); // первый вызов — «якорь», ничего не шлёт
    for round in 0..2 {
        for (index, corner) in corners.iter().enumerate() {
            let next = corners[(index + 1) % corners.len()];
            for step in 1..=40 {
                let t = f64::from(step) / 40.0;
                let x = corner.0 + (next.0 - corner.0) * t;
                let y = corner.1 + (next.1 - corner.1) * t;
                move_to(x, y);
                sleep(Duration::from_millis(if round == 0 { 12 } else { 6 }));
            }
        }
    }

    println!("2/4 клики...");
    for &pressed in &[true, false] {
        backend
            .simulate(InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed,
            })
            .expect("симуляция клика");
        sleep(Duration::from_millis(80));
    }

    println!("3/4 колесо вниз/вверх...");
    for &delta in &[1, 1, -1, -1] {
        backend
            .simulate(InputEvent::MouseWheel {
                delta_x: 0,
                delta_y: delta,
            })
            .expect("симуляция колеса");
        sleep(Duration::from_millis(150));
    }

    println!("4/4 печатаю \"ok\" и Enter...");
    for key in [Key::KeyO, Key::KeyK, Key::Return] {
        for &pressed in &[true, false] {
            backend
                .simulate(InputEvent::Key { key, pressed })
                .expect("симуляция клавиши");
            sleep(Duration::from_millis(30));
        }
    }

    println!("Готово. Если курсор нарисовал квадрат, кликнул, поскроллил и напечатал 'ok' — W2 работает.");
}
