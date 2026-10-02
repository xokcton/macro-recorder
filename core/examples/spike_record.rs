//! Технический спайк (Phase 0.5 из плана): проверяет, что `RdevinBackend`
//! реально ловит события ОС, а не только компилируется. Требует X-сервер
//! (реальный или Xvfb) и запускается вручную:
//!
//! ```text
//! cargo run -p mrcore --example spike_record
//! ```
//!
//! Во время работы сгенерируйте реальный ввод (подвигайте мышью, покликайте,
//! понажимайте клавиши) — по завершении будет напечатано, что записалось.

use std::sync::Arc;
use std::time::Duration;

use mrcore::backend::RdevinBackend;
use mrcore::Recorder;

fn main() {
    let backend = Arc::new(RdevinBackend);
    let recorder = Recorder::new(backend);

    recorder.start().expect("не удалось начать запись");
    println!("Записываю 3 секунды — подвигайте мышью / покликайте / понажимайте клавиши...");
    std::thread::sleep(Duration::from_secs(3));

    let steps = recorder.stop().expect("не удалось остановить запись");
    println!("Захвачено шагов: {}", steps.len());
    for (i, step) in steps.iter().take(15).enumerate() {
        println!("  [{i}] {:?}  (delay {} мс)", step.event, step.delay_ms);
    }
    if steps.len() > 15 {
        println!("  ... и ещё {} шагов", steps.len() - 15);
    }

    if steps.is_empty() {
        eprintln!(
            "ВНИМАНИЕ: ничего не захвачено — либо ввода не было, либо `rdevin::listen` не смог \
             подключиться к дисплею (проверьте переменную DISPLAY / права)."
        );
        std::process::exit(1);
    }
}
