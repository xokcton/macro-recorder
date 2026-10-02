//! Реализация [`InputBackend`] поверх прямого чтения `/dev/input/event*`
//! (крейт `evdev`) — только Linux, включается фичей `evdev`.
//!
//! Зачем: на чистом Wayland композитор не даёт сторонним приложениям глобальный
//! хук ввода — X11-механизмы, через которые работает [`super::RdevinBackend`],
//! там недоступны. Единственный способ видеть ввод без поддержки со стороны
//! композитора — читать события напрямую из ядра через evdev. Требуется право
//! чтения `/dev/input/*` (обычно — членство в группе `input`).
//!
//! ## Статус (W1–W2 плана Wayland-поддержки)
//! - ЗАПИСЬ ([`InputBackend::listen`]) — реализована: клавиатура, мышь
//!   (кнопки, движение, скролл).
//! - ВОСПРОИЗВЕДЕНИЕ ([`InputBackend::simulate`]) — реализовано через
//!   `/dev/uinput`: создаётся виртуальное устройство (клавиатура + относительная
//!   мышь + колесо), события отправляются от его имени. Композиторы видят его как
//!   обычную мышь/клавиатуру, поэтому клики и текст попадают куда нужно.
//!   Нужен доступ на запись в `/dev/uinput` (обычно тоже группа `input`).
//! - Абсолютные координаты `MouseMove` переводятся в ОТНОСИТЕЛЬНЫЕ дельты:
//!   первое движение после создания устройства служит «якорём» (ничего не
//!   отправляет, только фиксирует точку отсчёта), дальше форма траектории
//!   повторяется точно, но стартует с того места, где курсор находится сейчас.
//!   Спросить у Wayland реальные координаты курсора невозможно.
//! - `Key::Unknown` и неизвестные боковые кнопки при воспроизведении
//!   СОЗНАТЕЛЬНО отклоняются ошибкой: код в `Unknown` зависит от бэкенда записи
//!   (X11-keycode у rdevin, evdev-код у нас), и вслепую отправлять его ядру —
//!   значит молча нажать случайную клавишу.
//! - Устройства сканируются один раз при [`InputBackend::listen`; виртуальное
//!   устройство создаётся лениво — при первом `simulate` (если uinput недоступен,
//!   запись всё равно работает).
//!
//! Как и `rdevin`, рассчитан на ОДИН вызов [`InputBackend::listen`] на всё
//! время жизни процесса: каждый ридер живёт в собственном потоке до конца
//! работы приложения.

use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::error::CoreError;
use crate::event::{InputEvent, Key, MouseButton};

use super::InputBackend;

/// Диапазон кодов кнопок мыши (BTN_LEFT..BTN_TASK) — стабильный UAPI из
/// include/uapi/linux/input-event-codes.h. Всё, что EV_KEY вне этого диапазона,
/// считается клавишей; джойстики/геймпады (BTN_*, начиная с 0x118+) игнорируются.
const BTN_MOUSE_RANGE: std::ops::RangeInclusive<u16> = 0x110..=0x117;
/// Колбэк слушателя — алиас ради читаемых сигнатур (иначе clippy считает
/// `Arc<Mutex<Box<dyn FnMut(...)>>>` слишком сложным типом).
type ListenCallback = Box<dyn FnMut(InputEvent) + Send>;

/// Бэкенд чтения И воспроизведения ввода напрямую через ядро
/// (`/dev/input` + `/dev/uinput`, только Linux, фича `evdev`).
///
/// Не выбирается автоматически: чтобы им воспользоваться, его нужно явно
/// сконструировать вместо [`super::RdevinBackend`] при создании `Recorder`
/// или `Player`. Авто-выбор и переключалка — следующие фазы плана.
#[derive(Debug, Default, Clone)]
pub struct EvdevBackend {
    /// Состояние воспроизведения. Ленивое: виртуальное устройство создаётся
    /// при первом `simulate`, а не при старте процесса.
    playback: Arc<Mutex<PlaybackState>>,
}

/// Всё, что нужно для отправки событий: само виртуальное устройство и последняя
/// отправленная позиция курсора (для пересчёта абсолютных координат макроса
/// в относительные дельты uinput).
#[derive(Debug, Default)]
struct PlaybackState {
    device: Option<evdev::uinput::VirtualDevice>,
    last_pos: Option<(f64, f64)>,
}

/// Классификация устройства — для диагностики в логах и спайке.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Keyboard,
    Mouse,
    KeyboardAndMouse,
}

/// Сводка по одному устройству `/dev/input/event*` — элемент результата
/// [`probe_devices`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub path: PathBuf,
    pub name: String,
    pub kind: DeviceKind,
}

/// Перечисляет устройства `/dev/input/event*` и классифицирует их.
///
/// Возвращает `(найденные, ошибки)`: во втором списке — человекочитаемые
/// причины, почему устройство открыть не удалось (почти всегда Permission
/// denied — типичная ситуация на Wayland, лечится добавлением пользователя
/// в группу `input`). Специально НЕ используем готовый `evdev::enumerate()`:
/// он молча пропускает нечитаемые устройства, а пользователю важно увидеть
/// причину, а не загадочное «устройства не найдены».
pub fn probe_devices() -> (Vec<DeviceInfo>, Vec<String>) {
    let mut found = Vec::new();
    let mut errors = Vec::new();

    let entries = match std::fs::read_dir("/dev/input") {
        Ok(entries) => entries,
        Err(err) => {
            errors.push(format!("/dev/input не читается: {err}"));
            return (found, errors);
        }
    };

    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        // имена вида "event0"; остальное в /dev/input (by-id, mice, js0) —
        // не evdev-узлы
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("event"))
        })
        .collect();
    paths.sort();

    for path in paths {
        let file = match open_device_file(&path) {
            Ok(file) => file,
            Err(message) => {
                errors.push(message);
                continue;
            }
        };
        let device = match evdev::Device::from_fd(OwnedFd::from(file)) {
            Ok(device) => device,
            Err(err) => {
                errors.push(format!("{}: {err}", path.display()));
                continue;
            }
        };

        let keys = device.supported_keys();
        let rel_axes = device.supported_relative_axes();
        let is_mouse = keys.is_some_and(|keys| keys.contains(evdev::KeyCode::BTN_LEFT))
            || rel_axes.is_some_and(|axes| axes.contains(evdev::RelativeAxisCode::REL_X));
        let has_keyboard = keys.is_some_and(|keys| keys.contains(evdev::KeyCode::KEY_A));
        if !is_mouse && !has_keyboard {
            // кнопки питания, ИК-приёмники и прочее — не наш ввод
            continue;
        }
        let kind = if has_keyboard && is_mouse {
            DeviceKind::KeyboardAndMouse
        } else if has_keyboard {
            DeviceKind::Keyboard
        } else {
            DeviceKind::Mouse
        };
        found.push(DeviceInfo {
            path,
            name: device.name().unwrap_or("<без имени>").to_string(),
            kind,
        });
    }

    (found, errors)
}

impl InputBackend for EvdevBackend {
    fn listen(&self, callback: Box<dyn FnMut(InputEvent) + Send>) -> Result<(), CoreError> {
        let (devices, errors) = probe_devices();
        for message in &errors {
            tracing::warn!("{message}");
        }
        if devices.is_empty() {
            return Err(CoreError::ListenFailed(if errors.is_empty() {
                "в /dev/input не найдено ни одной клавиатуры или мыши".to_string()
            } else {
                format!(
                    "ни одно устройство ввода открыть не удалось: {}",
                    errors.join("; ")
                )
            }));
        }

        // Одна позиция курсора на все устройства: движение может прийти и от
        // тачпада, и от мыши — накапливаться оно должно в общую копилку.
        let cursor = Arc::new(Mutex::new((0.0_f64, 0.0_f64)));
        // Callback один на все ридеры, а trait отдаёт владение Box<dyn FnMut>,
        // а не Arc — оборачиваем сами.
        let callback = Arc::new(Mutex::new(callback));

        for info in devices {
            let path = info.path.clone();
            let cursor = Arc::clone(&cursor);
            let callback = Arc::clone(&callback);
            // Потоки сознательно НЕ join-ятся и живут до конца процесса — та же
            // политика, что у слушателя в Recorder (см. комментарий в recorder.rs).
            if let Err(err) = thread::Builder::new()
                .name(format!("mrcore-evdev:{}", path.display()))
                .spawn(move || run_device_loop(path, cursor, callback))
            {
                return Err(CoreError::ListenFailed(format!(
                    "не удалось запустить поток чтения {}: {err}",
                    info.path.display()
                )));
            }
        }
        Ok(())
    }

    fn simulate(&self, event: InputEvent) -> Result<(), CoreError> {
        let mut state = self.playback.lock().unwrap();
        // Сначала перевод события (он заимствует только last_pos), и лишь потом
        // достаём/создаём устройство — иначе два одновременных &mut на state.
        let batch = translate_to_uinput(event, &mut state.last_pos)?;
        if batch.is_empty() {
            // Бывает только на «якорном» первом движении — отправлять нечего.
            return Ok(());
        }
        let device = match state.device.as_mut() {
            Some(device) => device,
            None => state.device.insert(create_virtual_device()?),
        };
        // SYN_REPORT emit() добавляет сам (см. документацию VirtualDevice::emit).
        device
            .emit(&batch)
            .map_err(|err| CoreError::SimulateFailed(format!("uinput отверг пакет событий: {err}")))
    }
}

/// Открывает узел устройства обычным `File` — так ошибка доступа приходит как
/// обычный io::ErrorKind::PermissionDenied с понятным текстом, а не как
/// внутренняя ошибка крейта.
fn open_device_file(path: &Path) -> Result<std::fs::File, String> {
    std::fs::File::open(path).map_err(|err| match err.kind() {
        std::io::ErrorKind::PermissionDenied => format!(
            "{}: доступ запрещён. Обычно лечится добавлением пользователя \
            в группу input: sudo usermod -aG input $USER (и перелогиниться)",
            path.display()
        ),
        _ => format!("{}: {err}", path.display()),
    })
}

/// Цикл одного ридера: блокирующее чтение своего устройства до конца процесса
/// (или до отключения устройства — тогда поток завершается с предупреждением).
fn run_device_loop(
    path: PathBuf,
    cursor: Arc<Mutex<(f64, f64)>>,
    callback: Arc<Mutex<ListenCallback>>,
) {
    let file = match open_device_file(&path) {
        Ok(file) => file,
        Err(message) => {
            tracing::warn!("{message}");
            return;
        }
    };
    let mut device = match evdev::Device::from_fd(OwnedFd::from(file)) {
        Ok(device) => device,
        Err(err) => {
            tracing::warn!(
                "{}: не удалось инициализировать evdev: {err}",
                path.display()
            );
            return;
        }
    };

    loop {
        // fd открыт в блокирующем режиме, так что fetch_events спит, пока нет
        // данных, и просыпается на каждую пачку событий — CPU в простое не ест.
        let events = match device.fetch_events() {
            Ok(events) => events,
            Err(err) => {
                // например ENODEV — устройство отключили (выдернули донгл мыши)
                tracing::warn!("{}: чтение событий прервано: {err}", path.display());
                return;
            }
        };
        for event in events {
            let input_event = {
                let mut cursor = cursor.lock().unwrap();
                translate_event(event, &mut cursor)
            };
            if let Some(input_event) = input_event {
                let mut callback = callback.lock().unwrap();
                callback(input_event);
            }
        }
    }
}

/// Переводит одно сырое evdev-событие в доменное [`InputEvent`]. Чистая функция
/// без побочных эффектов, кроме накопления позиции курсора — поэтому её можно
/// полноценно тестировать без железа.
fn translate_event(event: evdev::InputEvent, cursor: &mut (f64, f64)) -> Option<InputEvent> {
    match event.destructure() {
        evdev::EventSummary::Key(_, code, value) => {
            // value: 0 = отпускание, 1 = нажатие, 2 = авто-повтор ядра.
            // Повтор сознательно проходит как нажатие — существующая логика
            // авто-повтора Recorder'а (compress_key_repeats) разберётся сама.
            let pressed = value != 0;
            let raw = code.code();
            if BTN_MOUSE_RANGE.contains(&raw) {
                button_from_code(raw).map(|button| InputEvent::MouseButton { button, pressed })
            } else {
                Some(InputEvent::Key {
                    key: key_from_code(raw),
                    pressed,
                })
            }
        }
        evdev::EventSummary::RelativeAxis(_, code, value) => {
            let delta = i64::from(value);
            if code == evdev::RelativeAxisCode::REL_X {
                cursor.0 += f64::from(value);
                Some(InputEvent::MouseMove {
                    x: cursor.0,
                    y: cursor.1,
                })
            } else if code == evdev::RelativeAxisCode::REL_Y {
                cursor.1 += f64::from(value);
                Some(InputEvent::MouseMove {
                    x: cursor.0,
                    y: cursor.1,
                })
            } else if code == evdev::RelativeAxisCode::REL_WHEEL {
                // Знак совпадает с rdevin/X11: положительная дельта — вверх/вправо.
                Some(InputEvent::MouseWheel {
                    delta_x: 0,
                    delta_y: delta,
                })
            } else if code == evdev::RelativeAxisCode::REL_HWHEEL {
                Some(InputEvent::MouseWheel {
                    delta_x: delta,
                    delta_y: 0,
                })
            } else {
                // REL_WHEEL_HI_RES / REL_HWHEEL_HI_RES сознательно пропускаем:
                // ядра с high-res колесом шлют ИХ и грубые REL_WHEEL одновременно,
                // учёт обоих задвоил бы скролл.
                None
            }
        }
        // ABS_* (тачскрины, графические планшеты) требуют знания геометрии экрана,
        // которой на Wayland у нас нет; SYN/LED/MISC и прочее — служебное.
        _ => None,
    }
}

/// Боковые кнопки намеренно переводятся в нумерацию X11 (8 = «назад», 9 =
/// «вперёд»), чтобы макросы были совместимы между записью через rdevin
/// (X11/Windows/macOS) и записью через evdev — см. комментарий про Button4/5
/// в rdevin_backend.rs.
fn button_from_code(raw: u16) -> Option<MouseButton> {
    match raw {
        0x110 => Some(MouseButton::Left),               // BTN_LEFT
        0x111 => Some(MouseButton::Right),              // BTN_RIGHT
        0x112 => Some(MouseButton::Middle),             // BTN_MIDDLE
        0x113 | 0x116 => Some(MouseButton::Unknown(8)), // BTN_SIDE | BTN_BACK («назад»)
        0x114 | 0x115 => Some(MouseButton::Unknown(9)), // BTN_EXTRA | BTN_FORWARD («вперёд»)
        0x117 => Some(MouseButton::Unknown(10)),        // BTN_TASK — редкая доп. кнопка мыши
        _ => None, // джойстики/геймпады (BTN_* вне диапазона мыши) — не наш ввод
    }
}

/// Переводит сырое клавиатурное значение `KEY_*` (Linux UAPI,
/// include/uapi/linux/input-event-codes.h) в доменный [`Key`].
///
/// Нераспознанные клавиши уходят в [`Key::Unknown`] с сырым evdev-кодом — так же,
/// как rdevin-бэкенд кладёт в `Unknown` свой платформенный код. Важно знать: эти
/// коды РАЗНЫЕ (у rdevin под Linux это keycode X11, здесь — код evdev), поэтому
/// макрос с `Unknown`-клавишами гарантированно совместим только между записью и
/// воспроизведением ОДНОГО бэкенда; именованные клавиши совместимы всегда.
///
/// `KEY_RIGHTALT` отображён в `AltGr`: это каноническое назначение правого Alt в
/// Linux-UAPI, и главное — отображение симметрично (при воспроизведении через
/// будущий uinput-бэкенд `AltGr` вернётся в `KEY_RIGHTALT`), так что на запись и
/// повтор макроса выбор имени не влияет.
fn key_from_code(raw: u16) -> Key {
    // KEY_F1..KEY_F24 — два непрерывных блока в UAPI (59..68 и 183..194).
    const FN_KEYS: [Key; 24] = [
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
        Key::F13,
        Key::F14,
        Key::F15,
        Key::F16,
        Key::F17,
        Key::F18,
        Key::F19,
        Key::F20,
        Key::F21,
        Key::F22,
        Key::F23,
        Key::F24,
    ];

    match raw {
        0x01 => Key::Escape,
        // Цифровой ряд: KEY_1..KEY_9 (2..10), KEY_0 (11).
        0x02 => Key::Num1,
        0x03 => Key::Num2,
        0x04 => Key::Num3,
        0x05 => Key::Num4,
        0x06 => Key::Num5,
        0x07 => Key::Num6,
        0x08 => Key::Num7,
        0x09 => Key::Num8,
        0x0A => Key::Num9,
        0x0B => Key::Num0,
        0x0C => Key::Minus,
        0x0D => Key::Equal,
        0x0E => Key::Backspace,
        0x0F => Key::Tab,
        // Ряд QWERTYUIOP.
        0x10 => Key::KeyQ,
        0x11 => Key::KeyW,
        0x12 => Key::KeyE,
        0x13 => Key::KeyR,
        0x14 => Key::KeyT,
        0x15 => Key::KeyY,
        0x16 => Key::KeyU,
        0x17 => Key::KeyI,
        0x18 => Key::KeyO,
        0x19 => Key::KeyP,
        0x1A => Key::LeftBracket,
        0x1B => Key::RightBracket,
        0x1C => Key::Return,
        0x1D => Key::ControlLeft,
        // Ряд ASDFGHJKL.
        0x1E => Key::KeyA,
        0x1F => Key::KeyS,
        0x20 => Key::KeyD,
        0x21 => Key::KeyF,
        0x22 => Key::KeyG,
        0x23 => Key::KeyH,
        0x24 => Key::KeyJ,
        0x25 => Key::KeyK,
        0x26 => Key::KeyL,
        0x27 => Key::SemiColon,
        0x28 => Key::Quote,
        0x29 => Key::BackQuote,
        0x2A => Key::ShiftLeft,
        0x2B => Key::BackSlash,
        // Ряд ZXCVBNM.
        0x2C => Key::KeyZ,
        0x2D => Key::KeyX,
        0x2E => Key::KeyC,
        0x2F => Key::KeyV,
        0x30 => Key::KeyB,
        0x31 => Key::KeyN,
        0x32 => Key::KeyM,
        0x33 => Key::Comma,
        0x34 => Key::Dot,
        0x35 => Key::Slash,
        0x36 => Key::ShiftRight,
        0x37 => Key::KpMultiply, // KEY_KPASTERISK
        0x38 => Key::Alt,
        0x39 => Key::Space,
        0x3A => Key::CapsLock,
        raw @ 0x3B..=0x44 => FN_KEYS[usize::from(raw - 0x3B)], // F1..F10
        0x45 => Key::NumLock,
        0x46 => Key::ScrollLock,
        0x47 => Key::Kp7,
        0x48 => Key::Kp8,
        0x49 => Key::Kp9,
        0x4A => Key::KpMinus,
        0x4B => Key::Kp4,
        0x4C => Key::Kp5,
        0x4D => Key::Kp6,
        0x4E => Key::KpPlus,
        0x4F => Key::Kp1,
        0x50 => Key::Kp2,
        0x51 => Key::Kp3,
        0x52 => Key::Kp0,
        0x53 => Key::KpDecimal,     // KEY_KPDOT
        0x56 => Key::IntlBackslash, // KEY_102ND («<>» на некоторых раскладках)
        0x57 => Key::F11,
        0x58 => Key::F12,
        0x60 => Key::KpReturn, // KEY_KPENTER
        0x61 => Key::ControlRight,
        0x62 => Key::KpDivide,    // KEY_KPSLASH
        0x63 => Key::PrintScreen, // KEY_SYSRQ — физически та же клавиша PrintScreen
        0x64 => Key::AltGr,       // KEY_RIGHTALT (см. комментарий к функции)
        0x66 => Key::Home,
        0x67 => Key::UpArrow,
        0x68 => Key::PageUp,
        0x69 => Key::LeftArrow,
        0x6A => Key::RightArrow,
        0x6B => Key::End,
        0x6C => Key::DownArrow,
        0x6D => Key::PageDown,
        0x6E => Key::Insert,
        0x6F => Key::Delete,
        0x71 => Key::VolumeMute, // KEY_MUTE
        0x72 => Key::VolumeDown,
        0x73 => Key::VolumeUp,
        0x75 => Key::KpEqual, // KEY_KPEQUAL
        0x77 => Key::Pause,
        0x79 => Key::KpComma,   // KEY_KPCOMMA
        0x7D => Key::MetaLeft,  // KEY_LEFTMETA
        0x7E => Key::MetaRight, // KEY_RIGHTMETA
        raw @ 0xB7..=0xC2 => FN_KEYS[usize::from(raw - 0xB7 + 12)], // F13..F24
        0xD2 => Key::PrintScreen, // KEY_PRINT
        0x1D0 => Key::Function, // KEY_FN
        // Японские/корейские (RO, KATAKANA, HENKAN...), COMPOSE, POWER, YEN и
        // прочее — без собственных вариантов домена, как и в rdevin-бэкенде.
        _ => Key::Unknown(u32::from(raw)),
    }
}

/// Создаёт виртуальное устройство uinput с ровно теми возможностями, которые
/// умеем генерировать: все именованные клавиши из таблицы `key_from_code`,
/// кнопки мыши `BTN_MOUSE_RANGE` и оси X/Y/WHEEL/HWHEEL.
fn create_virtual_device() -> Result<evdev::uinput::VirtualDevice, CoreError> {
    let denied = |what: &str, err: std::io::Error| {
        CoreError::SimulateFailed(format!(
            "{what}: {err}. Обычно причина — нет доступа к /dev/uinput; \
         на Wayland добавьте пользователя в группу input: \
         sudo usermod -aG input $USER (и перелогиниться)"
        ))
    };

    // Клавиатурные коды берём как ОБРАЗ таблицы key_from_code — набор
    // возможностей устройства никогда не разойдётся с тем, что мы шлём.
    let mut keys = evdev::AttributeSet::<evdev::KeyCode>::new();
    for raw in 0x01..=u16::MAX {
        // Одна проверка вместо двух отдельных вызовов key_from_code(raw): это
        // чистая функция без побочных эффектов, так что раньше здесь просто
        // дважды пересчитывалось одно и то же на каждой из ~65 тысяч
        // итераций впустую (второй раз — уже внутри keys.insert ниже).
        let key = key_from_code(raw);
        if matches!(key, Key::Unknown(_)) {
            continue;
        }
        keys.insert(evdev::KeyCode(key_to_code(&key)));
    }
    for raw in BTN_MOUSE_RANGE {
        keys.insert(evdev::KeyCode(raw)); // кнопки — тоже EV_KEY
    }

    let mut rels = evdev::AttributeSet::<evdev::RelativeAxisCode>::new();
    rels.insert(evdev::RelativeAxisCode::REL_X);
    rels.insert(evdev::RelativeAxisCode::REL_Y);
    rels.insert(evdev::RelativeAxisCode::REL_WHEEL);
    rels.insert(evdev::RelativeAxisCode::REL_HWHEEL);

    // `VirtualDeviceBuilder::new()` в этой версии крейта помечен deprecated —
    // рекомендуемый вход в билдер теперь через ассоциированную функцию
    // самого VirtualDevice.
    evdev::uinput::VirtualDevice::builder()
        .map_err(|err| denied("не удалось открыть /dev/uinput", err))?
        .name("macro-recorder virtual input")
        .with_keys(&keys)
        .map_err(|err| denied("не удалось объявить клавиши виртуального устройства", err))?
        .with_relative_axes(&rels)
        .map_err(|err| denied("не удалось объявить оси виртуального устройства", err))?
        .build()
        .map_err(|err| denied("не удалось создать виртуальное устройство uinput", err))
}

/// Обратное к [`button_from_code`]. Для пар-дубликатов выбирается канонический
/// представитель (SIDE для «назад», EXTRA для «вперёд»): оба кода пары означают
/// одно действие, но обратное отображение обязано быть однозначным.
/// `None` — кнопку воспроизвести нельзя (см. `translate_to_uinput`).
fn button_to_code(button: &MouseButton) -> Option<u16> {
    Some(match button {
        MouseButton::Left => 0x110,        // BTN_LEFT
        MouseButton::Right => 0x111,       // BTN_RIGHT
        MouseButton::Middle => 0x112,      // BTN_MIDDLE
        MouseButton::Unknown(8) => 0x113,  // BTN_SIDE («назад»)
        MouseButton::Unknown(9) => 0x114,  // BTN_EXTRA («вперёд»)
        MouseButton::Unknown(10) => 0x117, // BTN_TASK
        _ => None?,
    })
}

/// Обратное к [`key_from_code`] — точная инверсия для всех именованных вариантов
/// (инъективность и симметричность гарантируются тестами ниже).
/// Исключение: `PrintScreen` канонизируется в KEY_SYSRQ (0x63) — физически это
/// одна и та же клавиша, в таблице вперед просто два исходных кода (0x63 и 0xD2).
/// `Key::Unknown` инверсии не имеет и отклоняется раньше.
fn key_to_code(key: &Key) -> u16 {
    match key {
        Key::Escape => 0x01,
        Key::Num1 => 0x02,
        Key::Num2 => 0x03,
        Key::Num3 => 0x04,
        Key::Num4 => 0x05,
        Key::Num5 => 0x06,
        Key::Num6 => 0x07,
        Key::Num7 => 0x08,
        Key::Num8 => 0x09,
        Key::Num9 => 0x0A,
        Key::Num0 => 0x0B,
        Key::Minus => 0x0C,
        Key::Equal => 0x0D,
        Key::Backspace => 0x0E,
        Key::Tab => 0x0F,
        Key::KeyQ => 0x10,
        Key::KeyW => 0x11,
        Key::KeyE => 0x12,
        Key::KeyR => 0x13,
        Key::KeyT => 0x14,
        Key::KeyY => 0x15,
        Key::KeyU => 0x16,
        Key::KeyI => 0x17,
        Key::KeyO => 0x18,
        Key::KeyP => 0x19,
        Key::LeftBracket => 0x1A,
        Key::RightBracket => 0x1B,
        Key::Return => 0x1C,
        Key::ControlLeft => 0x1D,
        Key::KeyA => 0x1E,
        Key::KeyS => 0x1F,
        Key::KeyD => 0x20,
        Key::KeyF => 0x21,
        Key::KeyG => 0x22,
        Key::KeyH => 0x23,
        Key::KeyJ => 0x24,
        Key::KeyK => 0x25,
        Key::KeyL => 0x26,
        Key::SemiColon => 0x27,
        Key::Quote => 0x28,
        Key::BackQuote => 0x29,
        Key::ShiftLeft => 0x2A,
        Key::BackSlash => 0x2B,
        Key::KeyZ => 0x2C,
        Key::KeyX => 0x2D,
        Key::KeyC => 0x2E,
        Key::KeyV => 0x2F,
        Key::KeyB => 0x30,
        Key::KeyN => 0x31,
        Key::KeyM => 0x32,
        Key::Comma => 0x33,
        Key::Dot => 0x34,
        Key::Slash => 0x35,
        Key::ShiftRight => 0x36,
        Key::KpMultiply => 0x37,
        Key::Alt => 0x38,
        Key::Space => 0x39,
        Key::CapsLock => 0x3A,
        Key::F1 => 0x3B,
        Key::F2 => 0x3C,
        Key::F3 => 0x3D,
        Key::F4 => 0x3E,
        Key::F5 => 0x3F,
        Key::F6 => 0x40,
        Key::F7 => 0x41,
        Key::F8 => 0x42,
        Key::F9 => 0x43,
        Key::F10 => 0x44,
        Key::NumLock => 0x45,
        Key::ScrollLock => 0x46,
        Key::Kp7 => 0x47,
        Key::Kp8 => 0x48,
        Key::Kp9 => 0x49,
        Key::KpMinus => 0x4A,
        Key::Kp4 => 0x4B,
        Key::Kp5 => 0x4C,
        Key::Kp6 => 0x4D,
        Key::KpPlus => 0x4E,
        Key::Kp1 => 0x4F,
        Key::Kp2 => 0x50,
        Key::Kp3 => 0x51,
        Key::Kp0 => 0x52,
        Key::KpDecimal => 0x53,
        Key::IntlBackslash => 0x56,
        Key::F11 => 0x57,
        Key::F12 => 0x58,
        Key::KpReturn => 0x60,
        Key::ControlRight => 0x61,
        Key::KpDivide => 0x62,
        Key::PrintScreen => 0x63, // KEY_SYSRQ (каноничный из двух кодов, см. выше)
        Key::AltGr => 0x64,       // KEY_RIGHTALT
        Key::Home => 0x66,
        Key::UpArrow => 0x67,
        Key::PageUp => 0x68,
        Key::LeftArrow => 0x69,
        Key::RightArrow => 0x6A,
        Key::End => 0x6B,
        Key::DownArrow => 0x6C,
        Key::PageDown => 0x6D,
        Key::Insert => 0x6E,
        Key::Delete => 0x6F,
        Key::VolumeMute => 0x71,
        Key::VolumeDown => 0x72,
        Key::VolumeUp => 0x73,
        Key::KpEqual => 0x75,
        Key::Pause => 0x77,
        Key::KpComma => 0x79,
        Key::MetaLeft => 0x7D,
        Key::MetaRight => 0x7E,
        Key::F13 => 0xB7,
        Key::F14 => 0xB8,
        Key::F15 => 0xB9,
        Key::F16 => 0xBA,
        Key::F17 => 0xBB,
        Key::F18 => 0xBC,
        Key::F19 => 0xBD,
        Key::F20 => 0xBE,
        Key::F21 => 0xBF,
        Key::F22 => 0xC0,
        Key::F23 => 0xC1,
        Key::F24 => 0xC2,
        Key::Function => 0x1D0,
        // Сюда попасть нельзя: translate_to_uinput отклоняет Unknown раньше.
        Key::Unknown(_) => unreachable!("Key::Unknown должен быть отклонён в translate_to_uinput"),
    }
}

/// Переводит доменное событие в сырой uinput-пакет (SYN_REPORT добавит сам
/// `VirtualDevice::emit`). Пустой пакет валиден — так выглядит «якорное» первое
/// движение. Чистая функция — тестируется без ядра.
fn translate_to_uinput(
    event: InputEvent,
    last_pos: &mut Option<(f64, f64)>,
) -> Result<Vec<evdev::InputEvent>, CoreError> {
    // Коды осей — стабильный Linux UAPI (те же, что в тестах ниже).
    const REL_X: u16 = 0x00;
    const REL_Y: u16 = 0x01;
    const REL_HWHEEL: u16 = 0x06;
    const REL_WHEEL: u16 = 0x08;

    fn key_event(code: u16, value: i32) -> evdev::InputEvent {
        evdev::InputEvent::new(evdev::EventType::KEY.0, code, value)
    }
    fn rel_event(code: u16, value: i32) -> evdev::InputEvent {
        evdev::InputEvent::new(evdev::EventType::RELATIVE.0, code, value)
    }
    fn clamp_to_i32(v: f64) -> i32 {
        v.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
    }

    match event {
        InputEvent::MouseMove { x, y } => {
            let Some((lx, ly)) = *last_pos else {
                // Якорь: где курсор сейчас — не знаем, поэтому фиксируем точку
                // отсчёта и сохраняем ФОРМУ траектории относительно неё.
                *last_pos = Some((x, y));
                return Ok(Vec::new());
            };
            *last_pos = Some((x, y));
            let (dx, dy) = (clamp_to_i32(x - lx), clamp_to_i32(y - ly));
            if dx == 0 && dy == 0 {
                return Ok(Vec::new());
            }
            Ok(vec![rel_event(REL_X, dx), rel_event(REL_Y, dy)])
        }
        InputEvent::MouseButton { button, pressed } => {
            let Some(code) = button_to_code(&button) else {
                return Err(CoreError::SimulateFailed(format!(
                    "кнопка {button:?} не имеет соответствия среди кнопок мыши evdev \
                    (поддерживаются Left/Right/Middle и дополнительные 8/9/10)"
                )));
            };
            Ok(vec![key_event(code, i32::from(pressed))])
        }
        InputEvent::MouseWheel { delta_x, delta_y } => {
            let mut batch = Vec::with_capacity(2);
            // Поля домена — i64, uinput ждёт i32. Дельты колеса на практике
            // крошечные; зажим — просто защита от теоретического переполнения.
            let dx = delta_x.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
            let dy = delta_y.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
            if dx != 0 {
                batch.push(rel_event(REL_HWHEEL, dx));
            }
            if dy != 0 {
                batch.push(rel_event(REL_WHEEL, dy));
            }
            Ok(batch)
        }
        InputEvent::Key { key, pressed } => {
            if matches!(key, Key::Unknown(_)) {
                return Err(CoreError::SimulateFailed(
                    "в шаге макроса нераспознанная клавиша (Key::Unknown): её код зависит \
                    от бэкенда записи, и отправлять его через evdev вслепую нельзя. \
                    Перезапишите этот шаг, записав макрос через evdev-бэкенд"
                        .to_string(),
                ));
            }
            Ok(vec![key_event(key_to_code(&key), i32::from(pressed))])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Собирает сырое evdev-событие типа KEY (`EventType::KEY.0` — это u16,
    /// ровно такой тип ждёт `InputEvent::new`).
    fn key_event(code: evdev::KeyCode, value: i32) -> evdev::InputEvent {
        evdev::InputEvent::new(evdev::EventType::KEY.0, code.code(), value)
    }

    /// Собирает сырое evdev-событие относительной оси (коды — Linux UAPI:
    /// REL_X=0x00, REL_Y=0x01, REL_HWHEEL=0x06, REL_WHEEL=0x08,
    /// REL_WHEEL_HI_RES=0x0B, REL_HWHEEL_HI_RES=0x0C).
    fn rel_event(code: u16, value: i32) -> evdev::InputEvent {
        evdev::InputEvent::new(evdev::EventType::RELATIVE.0, code, value)
    }

    #[test]
    fn key_table_matches_evdev_constants() {
        let cases = [
            (evdev::KeyCode::KEY_ESC, Key::Escape),
            (evdev::KeyCode::KEY_1, Key::Num1),
            (evdev::KeyCode::KEY_0, Key::Num0),
            (evdev::KeyCode::KEY_MINUS, Key::Minus),
            (evdev::KeyCode::KEY_EQUAL, Key::Equal),
            (evdev::KeyCode::KEY_BACKSPACE, Key::Backspace),
            (evdev::KeyCode::KEY_TAB, Key::Tab),
            (evdev::KeyCode::KEY_Q, Key::KeyQ),
            (evdev::KeyCode::KEY_P, Key::KeyP),
            (evdev::KeyCode::KEY_LEFTBRACE, Key::LeftBracket),
            (evdev::KeyCode::KEY_RIGHTBRACE, Key::RightBracket),
            (evdev::KeyCode::KEY_ENTER, Key::Return),
            (evdev::KeyCode::KEY_LEFTCTRL, Key::ControlLeft),
            (evdev::KeyCode::KEY_A, Key::KeyA),
            (evdev::KeyCode::KEY_L, Key::KeyL),
            (evdev::KeyCode::KEY_Z, Key::KeyZ),
            (evdev::KeyCode::KEY_M, Key::KeyM),
            (evdev::KeyCode::KEY_SEMICOLON, Key::SemiColon),
            (evdev::KeyCode::KEY_APOSTROPHE, Key::Quote),
            (evdev::KeyCode::KEY_GRAVE, Key::BackQuote),
            (evdev::KeyCode::KEY_LEFTSHIFT, Key::ShiftLeft),
            (evdev::KeyCode::KEY_BACKSLASH, Key::BackSlash),
            (evdev::KeyCode::KEY_102ND, Key::IntlBackslash),
            (evdev::KeyCode::KEY_COMMA, Key::Comma),
            (evdev::KeyCode::KEY_DOT, Key::Dot),
            (evdev::KeyCode::KEY_SLASH, Key::Slash),
            (evdev::KeyCode::KEY_RIGHTSHIFT, Key::ShiftRight),
            (evdev::KeyCode::KEY_KPASTERISK, Key::KpMultiply),
            (evdev::KeyCode::KEY_LEFTALT, Key::Alt),
            (evdev::KeyCode::KEY_RIGHTALT, Key::AltGr),
            (evdev::KeyCode::KEY_SPACE, Key::Space),
            (evdev::KeyCode::KEY_CAPSLOCK, Key::CapsLock),
            (evdev::KeyCode::KEY_F1, Key::F1),
            (evdev::KeyCode::KEY_F10, Key::F10),
            (evdev::KeyCode::KEY_F11, Key::F11),
            (evdev::KeyCode::KEY_F12, Key::F12),
            (evdev::KeyCode::KEY_F13, Key::F13),
            (evdev::KeyCode::KEY_F24, Key::F24),
            (evdev::KeyCode::KEY_NUMLOCK, Key::NumLock),
            (evdev::KeyCode::KEY_SCROLLLOCK, Key::ScrollLock),
            (evdev::KeyCode::KEY_KP7, Key::Kp7),
            (evdev::KeyCode::KEY_KP0, Key::Kp0),
            (evdev::KeyCode::KEY_KPMINUS, Key::KpMinus),
            (evdev::KeyCode::KEY_KPPLUS, Key::KpPlus),
            (evdev::KeyCode::KEY_KPDOT, Key::KpDecimal),
            (evdev::KeyCode::KEY_KPENTER, Key::KpReturn),
            (evdev::KeyCode::KEY_KPSLASH, Key::KpDivide),
            (evdev::KeyCode::KEY_KPEQUAL, Key::KpEqual),
            (evdev::KeyCode::KEY_KPCOMMA, Key::KpComma),
            (evdev::KeyCode::KEY_RIGHTCTRL, Key::ControlRight),
            (evdev::KeyCode::KEY_SYSRQ, Key::PrintScreen),
            (evdev::KeyCode::KEY_HOME, Key::Home),
            (evdev::KeyCode::KEY_UP, Key::UpArrow),
            (evdev::KeyCode::KEY_PAGEUP, Key::PageUp),
            (evdev::KeyCode::KEY_LEFT, Key::LeftArrow),
            (evdev::KeyCode::KEY_RIGHT, Key::RightArrow),
            (evdev::KeyCode::KEY_END, Key::End),
            (evdev::KeyCode::KEY_DOWN, Key::DownArrow),
            (evdev::KeyCode::KEY_PAGEDOWN, Key::PageDown),
            (evdev::KeyCode::KEY_INSERT, Key::Insert),
            (evdev::KeyCode::KEY_DELETE, Key::Delete),
            (evdev::KeyCode::KEY_MUTE, Key::VolumeMute),
            (evdev::KeyCode::KEY_VOLUMEDOWN, Key::VolumeDown),
            (evdev::KeyCode::KEY_VOLUMEUP, Key::VolumeUp),
            (evdev::KeyCode::KEY_PAUSE, Key::Pause),
            (evdev::KeyCode::KEY_LEFTMETA, Key::MetaLeft),
            (evdev::KeyCode::KEY_RIGHTMETA, Key::MetaRight),
            (evdev::KeyCode::KEY_FN, Key::Function),
        ];
        for (code, expected) in cases {
            assert_eq!(
                key_from_code(code.code()),
                expected,
                "неверное отображение {:?}",
                code
            );
        }
    }

    #[test]
    fn unknown_key_falls_back_to_raw_evdev_code() {
        assert_eq!(
            key_from_code(evdev::KeyCode::KEY_YEN.code()),
            Key::Unknown(124)
        );
    }

    #[test]
    fn buttons_map_to_x11_numbering() {
        // Боковые кнопки нумеруются как в X11/rdevin (8 = «назад», 9 = «вперёд»),
        // чтобы макросы были переносимы между бэкендами.
        let cases = [
            (evdev::KeyCode::BTN_LEFT, MouseButton::Left),
            (evdev::KeyCode::BTN_RIGHT, MouseButton::Right),
            (evdev::KeyCode::BTN_MIDDLE, MouseButton::Middle),
            (evdev::KeyCode::BTN_SIDE, MouseButton::Unknown(8)),
            (evdev::KeyCode::BTN_BACK, MouseButton::Unknown(8)),
            (evdev::KeyCode::BTN_EXTRA, MouseButton::Unknown(9)),
            (evdev::KeyCode::BTN_FORWARD, MouseButton::Unknown(9)),
            (evdev::KeyCode::BTN_TASK, MouseButton::Unknown(10)),
        ];
        for (code, expected) in cases {
            assert_eq!(
                button_from_code(code.code()),
                Some(expected),
                "неверное отображение {:?}",
                code
            );
        }
    }

    #[test]
    fn press_autorepeat_and_release_pass_through() {
        // value: 1 = нажатие, 2 = авто-повтор ядра (проходит как нажатие —
        // разбираться будет существующая логика Recorder'а), 0 = отпускание.
        let mut cursor = (0.0, 0.0);
        assert_eq!(
            translate_event(key_event(evdev::KeyCode::KEY_A, 1), &mut cursor),
            Some(InputEvent::Key {
                key: Key::KeyA,
                pressed: true
            })
        );
        assert_eq!(
            translate_event(key_event(evdev::KeyCode::KEY_A, 2), &mut cursor),
            Some(InputEvent::Key {
                key: Key::KeyA,
                pressed: true
            })
        );
        assert_eq!(
            translate_event(key_event(evdev::KeyCode::KEY_A, 0), &mut cursor),
            Some(InputEvent::Key {
                key: Key::KeyA,
                pressed: false
            })
        );
    }

    #[test]
    fn relative_motion_accumulates_into_cursor_position() {
        let mut cursor = (0.0, 0.0);
        assert_eq!(
            translate_event(rel_event(0x00, 5), &mut cursor), // REL_X +5
            Some(InputEvent::MouseMove { x: 5.0, y: 0.0 })
        );
        assert_eq!(
            translate_event(rel_event(0x01, 3), &mut cursor), // REL_Y +3
            Some(InputEvent::MouseMove { x: 5.0, y: 3.0 })
        );
        assert_eq!(
            translate_event(rel_event(0x00, -2), &mut cursor), // REL_X −2
            Some(InputEvent::MouseMove { x: 3.0, y: 3.0 })
        );
    }

    #[test]
    fn wheel_signs_follow_rdevin_convention() {
        let mut cursor = (0.0, 0.0);
        assert_eq!(
            translate_event(rel_event(0x08, 1), &mut cursor), // REL_WHEEL вверх
            Some(InputEvent::MouseWheel {
                delta_x: 0,
                delta_y: 1
            })
        );
        assert_eq!(
            translate_event(rel_event(0x08, -1), &mut cursor),
            Some(InputEvent::MouseWheel {
                delta_x: 0,
                delta_y: -1
            })
        );
        assert_eq!(
            translate_event(rel_event(0x06, -2), &mut cursor), // REL_HWHEEL влево
            Some(InputEvent::MouseWheel {
                delta_x: -2,
                delta_y: 0
            })
        );
    }

    #[test]
    fn hi_res_wheel_is_dropped_to_avoid_double_scroll() {
        let mut cursor = (0.0, 0.0);
        // Ядра с high-res колесом шлют И HI_RES, И грубый REL_WHEEL — учёт обоих
        // задвоил бы скролл, поэтому HI_RES отбрасывается.
        assert_eq!(translate_event(rel_event(0x0B, 120), &mut cursor), None); // REL_WHEEL_HI_RES
        assert_eq!(translate_event(rel_event(0x0C, 120), &mut cursor), None); // REL_HWHEEL_HI_RES
    }

    #[test]
    fn key_reverse_mapping_roundtrips_every_named_key() {
        // Прогоняем ВСЕ возможные сырые коды: каждая именованная клавиша
        // обязана вернуться в себя через key_to_code -> key_from_code.
        for raw in 0x01..=u16::MAX {
            let key = key_from_code(raw);
            if matches!(key, Key::Unknown(_)) {
                continue;
            }
            assert_eq!(
                key_from_code(key_to_code(&key)),
                key,
                "roundtrip сломан для сырого кода {raw:#04x}"
            );
        }
    }

    #[test]
    fn key_reverse_mapping_is_injective() {
        // Разные именованные клавиши не могут делить один evdev-код,
        // иначе воспроизведение нажимало бы не ту клавишу.
        let mut named: Vec<Key> = Vec::new();
        for raw in 0x01..=u16::MAX {
            let key = key_from_code(raw);
            // Unknown пропускаем по определению; повторяющийся вариант
            // (PrintScreen приходит и с KEY_SYSRQ 0x63, и с KEY_PRINT 0xD2)
            // добавляем один раз — сравнивать нужно УНИКАЛЬНЫЕ клавиши.
            if matches!(key, Key::Unknown(_)) || named.contains(&key) {
                continue;
            }
            named.push(key);
        }
        for (index, key) in named.iter().enumerate() {
            for other in &named[index + 1..] {
                assert_ne!(
                    key_to_code(key),
                    key_to_code(other),
                    "коды совпали у {key:?} и {other:?}"
                );
            }
        }
    }

    #[test]
    fn button_reverse_mapping_roundtrips_x11_numbering() {
        let cases = [
            MouseButton::Left,
            MouseButton::Right,
            MouseButton::Middle,
            MouseButton::Unknown(8),
            MouseButton::Unknown(9),
            MouseButton::Unknown(10),
        ];
        for button in cases {
            let code = button_to_code(&button).expect("код кнопки");
            assert_eq!(button_from_code(code), Some(button));
        }
    }

    #[test]
    fn unknown_button_and_unknown_key_are_rejected_not_sent() {
        let mut anchor = None;
        assert!(translate_to_uinput(
            InputEvent::MouseButton {
                button: MouseButton::Unknown(42),
                pressed: true
            },
            &mut anchor
        )
        .is_err());
        assert!(translate_to_uinput(
            InputEvent::Key {
                key: Key::Unknown(999),
                pressed: true
            },
            &mut anchor
        )
        .is_err());
    }

    #[test]
    fn first_move_anchors_then_becomes_relative_deltas() {
        let mut last = None;
        // Первое движение — якорь: ничего не отправляет.
        assert!(
            translate_to_uinput(InputEvent::MouseMove { x: 100.0, y: 50.0 }, &mut last)
                .unwrap()
                .is_empty()
        );
        // Дальше — чистые дельты.
        assert_eq!(
            translate_to_uinput(InputEvent::MouseMove { x: 110.0, y: 55.0 }, &mut last).unwrap(),
            [
                evdev::InputEvent::new(evdev::EventType::RELATIVE.0, 0x00, 10),
                evdev::InputEvent::new(evdev::EventType::RELATIVE.0, 0x01, 5),
            ]
        );
        assert_eq!(
            translate_to_uinput(InputEvent::MouseMove { x: 105.0, y: 60.0 }, &mut last).unwrap(),
            [
                evdev::InputEvent::new(evdev::EventType::RELATIVE.0, 0x00, -5),
                evdev::InputEvent::new(evdev::EventType::RELATIVE.0, 0x01, 5),
            ]
        );
    }

    #[test]
    fn wheel_translates_both_axes_with_rdevin_signs() {
        let mut anchor = None;
        assert_eq!(
            translate_to_uinput(
                InputEvent::MouseWheel {
                    delta_x: -2,
                    delta_y: 3
                },
                &mut anchor
            )
            .unwrap(),
            [
                evdev::InputEvent::new(evdev::EventType::RELATIVE.0, 0x06, -2), // REL_HWHEEL
                evdev::InputEvent::new(evdev::EventType::RELATIVE.0, 0x08, 3),  // REL_WHEEL
            ]
        );
        // Нулевые дельты не порождают событий вовсе.
        assert!(translate_to_uinput(
            InputEvent::MouseWheel {
                delta_x: 0,
                delta_y: 0
            },
            &mut anchor
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn key_press_release_translate_to_key_events() {
        let mut anchor = None;
        assert_eq!(
            translate_to_uinput(
                InputEvent::Key {
                    key: Key::KeyA,
                    pressed: true
                },
                &mut anchor
            )
            .unwrap(),
            [evdev::InputEvent::new(evdev::EventType::KEY.0, 0x1E, 1)] // KEY_A
        );
        assert_eq!(
            translate_to_uinput(
                InputEvent::Key {
                    key: Key::KeyA,
                    pressed: false
                },
                &mut anchor
            )
            .unwrap(),
            [evdev::InputEvent::new(evdev::EventType::KEY.0, 0x1E, 0)]
        );
    }
}
