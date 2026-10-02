//! Ручной спайк evdev-бэкенда (фаза W1 Wayland-поддержки).
//!
//! Запуск (требует право чтения /dev/input — см. сообщения при запуске):
//!
//! ```bash
//! cargo run -p mrcore --features evdev --example spike_evdev
//! ```
//!
//! Работает и под чистым Wayland — в отличие от spike_record, которому нужен X11.
//! Остановка: Ctrl-C.

use mrcore::backend::{EvdevBackend, InputBackend};

fn main() {
    let (devices, errors) = mrcore::backend::evdev_backend::probe_devices();
    println!("Найдено устройств ввода: {}", devices.len());
    for device in &devices {
        println!(
            "  {} — {:?}: {}",
            device.path.display(),
            device.kind,
            device.name
        );
    }
    for message in &errors {
        println!("  ПРОПУЩЕНО: {message}");
    }
    if devices.is_empty() {
        println!("Продолжать нечего — исправьте доступ (сообщения выше) и запустите снова.");
        return;
    }

    println!("\nСлушаю ввод (остановка — Ctrl-C)...");
    EvdevBackend::default()
        .listen(Box::new(|event| println!("{event:?}")))
        .expect("слушатель evdev");
}
