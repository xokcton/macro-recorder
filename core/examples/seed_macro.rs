//! Вспомогательный инструмент для ручного тестирования UI: кладёт в реальное
//! хранилище (то же самое, что использует GUI-приложение) один заранее
//! известный макрос, чтобы не гадать с координатами кликов при записи через
//! автоматизированный (xdotool) UI-тест.
//!
//! Необязательный аргумент — имя макроса (по умолчанию "Тестовый макрос").
//! Один из шагов клика намеренно снабжён синтетическим (не реальным)
//! screen-guard отпечатком+миниатюрой ярко-бирюзового цвета — удобно для
//! проверки, что превью в редакторе шагов реально рисуется нужным цветом,
//! а не просто "что-то есть".

use mrcore::screen_guard::RegionSnapshot;
use mrcore::{
    InputEvent, Key, LoopMode, Macro, MacroStep, MouseButton, ScreenFingerprint, ScreenThumbnail,
    Storage,
};

fn main() {
    let storage = Storage::new().expect("не удалось открыть хранилище");
    let name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Тестовый макрос".to_string());

    let synthetic_snapshot = RegionSnapshot {
        width: 64,
        height: 64,
        rgba: {
            let mut px = Vec::with_capacity(64 * 64 * 4);
            for _ in 0..(64 * 64) {
                px.extend_from_slice(&[0, 200, 180, 255]); // бирюзовый
            }
            px
        },
    };
    let fingerprint = ScreenFingerprint::from_snapshot(&synthetic_snapshot);
    let thumbnail = ScreenThumbnail::from_snapshot(&synthetic_snapshot);

    let steps = vec![
        MacroStep::new(InputEvent::MouseMove { x: 120.0, y: 80.0 }, 0),
        MacroStep::with_fingerprint_and_thumbnail(
            InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true,
            },
            250,
            fingerprint,
            thumbnail,
        ),
        MacroStep::new(
            InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: false,
            },
            10,
        ),
        MacroStep::new(
            InputEvent::Key {
                key: Key::KeyA,
                pressed: true,
            },
            400,
        ),
        MacroStep::new(
            InputEvent::Key {
                key: Key::KeyA,
                pressed: false,
            },
            50,
        ),
        MacroStep::new(
            InputEvent::MouseWheel {
                delta_x: 0,
                delta_y: -3,
            },
            150,
        ),
        MacroStep::new(
            InputEvent::MouseButton {
                button: MouseButton::Unknown(8),
                pressed: true,
            },
            300,
        ),
        MacroStep::new(
            InputEvent::MouseButton {
                button: MouseButton::Unknown(8),
                pressed: false,
            },
            10,
        ),
    ];

    let mut m = Macro::new(name, steps);
    m.loop_mode = LoopMode::Once;

    storage.save(&m).expect("не удалось сохранить макрос");
    println!(
        "Сохранён макрос '{}' ({} шагов), id={}",
        m.name,
        m.steps.len(),
        m.id
    );
}
