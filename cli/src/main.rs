//! Headless-воспроизведение макросов без GUI.
//!
//! Примеры:
//!   mrec play ~/.local/share/macro-recorder/macros/<uuid>.json
//!   mrec play login.json --speed 2.0 --times 5
//!   mrec play loop.json --forever
//!
//! Ctrl-C штатно завершает процесс (стандартная обработка SIGINT): для
//! одноразового проигрывания отдельный флаг остановки не нужен —
//! воспроизведение умирает вместе с процессом.

use std::path::PathBuf;
use std::sync::Arc;

#[cfg(all(target_os = "linux", feature = "evdev"))]
use mrcore::backend::EvdevBackend;
use mrcore::backend::{InputBackend, RdevinBackend};
use mrcore::{InputBackendChoice, LoopMode, Macro, Player};

const USAGE: &str = "\
Использование:
  mrec play <файл.json> [--speed N] [--times N] [--forever] [--backend rdevin|evdev]

Опции:
  --speed N       множитель скорости, положительное число (по умолчанию 1.0)
  --times N       переопределить режим повтора из файла: ровно N проходов
  --forever       переопределить режим повтора из файла: бесконечно (остановка — Ctrl-C)
  --backend NAME  rdevin (по умолчанию) или evdev — прямая симуляция через
                  /dev/uinput, единственный способ воспроизвести макрос на
                  чистом Wayland без X11/XWayland. Нужна сборка с фичей evdev
                  (cargo build --features evdev) и доступ к /dev/uinput
                  (обычно группа `input`, см. README)
  -h, --help      эта справка";

struct Args {
    path: PathBuf,
    speed: f32,
    /// Переопределение режима из файла на `Times(n)`, если задано.
    times: Option<u32>,
    /// Переопределение режима из файла на бесконечный.
    forever: bool,
    backend: InputBackendChoice,
}

/// Та же эвристика, что использует GUI (`app::input_backend::is_wayland_session`):
/// `WAYLAND_DISPLAY`/`XDG_SESSION_TYPE` выставляет композитор или менеджер сессии,
/// а не сам rdevin, поэтому по ним можно заранее предупредить пользователя.
fn is_wayland_session() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        || matches!(std::env::var("XDG_SESSION_TYPE").as_deref(), Ok("wayland"))
}

fn main() {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(err) = run(args) {
        eprintln!("ошибка: {err}");
        std::process::exit(1);
    }
}

fn parse_args<I: Iterator<Item = String>>(mut args: I) -> Result<Args, String> {
    let mut path: Option<String> = None;
    let mut speed = 1.0f32;
    let mut times = None;
    let mut forever = false;
    let mut backend = InputBackendChoice::Rdevin;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "--backend" => {
                let raw = args
                    .next()
                    .ok_or("--backend требует значение: rdevin или evdev")?;
                backend = match raw.as_str() {
                    "rdevin" => InputBackendChoice::Rdevin,
                    "evdev" => InputBackendChoice::Evdev,
                    other => {
                        return Err(format!(
                            "--backend: неизвестное значение «{other}», ожидается rdevin или evdev"
                        ));
                    }
                };
            }
            "--speed" => {
                let raw = args.next().ok_or("--speed требует число")?;
                speed = raw
                    .parse()
                    .map_err(|_| format!("--speed: «{raw}» не число"))?;
                if !speed.is_finite() || speed <= 0.0 {
                    return Err(format!(
                        "--speed: должно быть положительным числом, получено {speed}"
                    ));
                }
            }
            "--times" => {
                let raw = args.next().ok_or("--times требует число")?;
                let n: u32 = raw
                    .parse()
                    .map_err(|_| format!("--times: «{raw}» не целое число"))?;
                if n == 0 {
                    return Err("--times: минимум 1".into());
                }
                times = Some(n);
            }
            "--forever" => forever = true,
            _ if arg.starts_with('-') => return Err(format!("неизвестный флаг «{arg}»")),
            _ => {
                if path.replace(arg).is_some() {
                    return Err("указан больше чем один файл макроса".into());
                }
            }
        }
    }

    Ok(Args {
        path: PathBuf::from(path.ok_or("не указан файл макроса")?),
        speed,
        times,
        forever,
        backend,
    })
}

fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read(&args.path)
        .map_err(|e| format!("не удалось прочитать {}: {e}", args.path.display()))?;
    let mut macro_: Macro = serde_json::from_slice(&bytes)
        .map_err(|e| format!("{} не похож на файл макроса: {e}", args.path.display()))?;

    // Переопределения командной строки главнее значения из файла.
    if let Some(n) = args.times {
        macro_.loop_mode = LoopMode::Times(n);
    }
    if args.forever {
        macro_.loop_mode = LoopMode::Infinite;
    }

    println!("▶ {} ({} шагов)", macro_.name, macro_.steps.len());

    if matches!(args.backend, InputBackendChoice::Rdevin) && is_wayland_session() {
        eprintln!(
            "предупреждение: похоже на Wayland-сессию — rdevin, скорее всего, не сможет \
             отправить события воспроизведения; попробуйте --backend evdev (нужна сборка \
             с фичей evdev и доступ к /dev/uinput, обычно группа `input`)"
        );
    }

    // Тот же стек, что у GUI (см. app/src/input_backend.rs): выбор бэкенда —
    // единственная разница между X11/XWayland и чистым Wayland.
    let backend: Arc<dyn InputBackend> = match args.backend {
        InputBackendChoice::Rdevin => Arc::new(RdevinBackend),
        #[cfg(all(target_os = "linux", feature = "evdev"))]
        InputBackendChoice::Evdev => Arc::new(EvdevBackend::default()),
        #[cfg(not(all(target_os = "linux", feature = "evdev")))]
        InputBackendChoice::Evdev => {
            return Err("запрошен --backend evdev, но mrec собран без фичи evdev; \
                 пересоберите: cargo build --release -p macro-recorder-cli --features evdev"
                .into());
        }
    };
    let player = Player::new(backend);
    let handle = player.play(macro_, args.speed);

    handle.wait(); // блокируемся до конца; Ctrl-C убивает процесс штатно
    println!("готово");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(list: &[&str]) -> Result<Args, String> {
        parse_args(list.iter().map(|s| (*s).to_string()))
    }

    #[test]
    fn parses_path_speed_and_overrides() {
        let a = parsed(&["m.json", "--speed", "2.5", "--times", "7"]).unwrap();
        assert_eq!(a.path, PathBuf::from("m.json"));
        assert_eq!(a.speed, 2.5);
        assert_eq!(a.times, Some(7));
        assert!(!a.forever);
        assert_eq!(a.backend, InputBackendChoice::Rdevin);
    }

    #[test]
    fn parses_backend_flag() {
        let a = parsed(&["m.json", "--backend", "evdev"]).unwrap();
        assert_eq!(a.backend, InputBackendChoice::Evdev);
        let a = parsed(&["m.json", "--backend", "rdevin"]).unwrap();
        assert_eq!(a.backend, InputBackendChoice::Rdevin);
    }

    #[test]
    fn rejects_unknown_backend() {
        assert!(parsed(&["m.json", "--backend", "wat"]).is_err());
        assert!(parsed(&["m.json", "--backend"]).is_err());
    }

    #[test]
    fn rejects_invalid_values() {
        assert!(parsed(&["m.json", "--speed", "0"]).is_err());
        assert!(parsed(&["m.json", "--speed", "abc"]).is_err());
        assert!(parsed(&["m.json", "--times", "0"]).is_err());
        assert!(parsed(&["m.json", "--times", "-3"]).is_err());
    }

    #[test]
    fn requires_exactly_one_path() {
        assert!(parsed(&[]).is_err());
        assert!(parsed(&["a.json", "b.json"]).is_err());
    }

    #[test]
    fn unknown_flag_is_rejected() {
        assert!(parsed(&["m.json", "--wat"]).is_err());
    }
}
