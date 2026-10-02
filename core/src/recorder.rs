//! Движок записи.
//!
//! Ключевое архитектурное решение: у `rdevin::listen` (как и у большинства
//! низкоуровневых hook-библиотек) нет публичного способа программно остановить
//! уже запущенный блокирующий вызов. Поэтому слушатель запускается **один раз**,
//! в момент создания [`Recorder`], и живёт всё время работы приложения. "Старт" и
//! "стоп" записи не запускают/убивают этот поток, а лишь переключают, обрабатывает
//! ли внутренний callback приходящие события — см. `Shared::tracker`.

use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use crate::backend::InputBackend;
use crate::error::CoreError;
use crate::event::InputEvent;
use crate::macro_data::MacroStep;
use crate::screen_guard::{
    ScreenCaptureConfig, ScreenFingerprint, ScreenGuard, ScreenThumbnail, DEFAULT_REGION_SIZE,
};
use crate::timing::DelayTracker;
use crate::Key;

struct Shared {
    /// `None` — запись не идёт, входящие события ввода отбрасываются.
    /// `Some` — запись идёт (возможно, на паузе — это состояние живёт внутри трекера).
    tracker: Option<DelayTracker>,
    steps: Vec<MacroStep>,
    /// Последняя известная позиция курсора — нужна, чтобы знать, какую область
    /// экрана проверять в момент клика (сам клик координат не несёт).
    last_mouse_pos: (f64, f64),
    /// Что из отпечатка/миниатюры реально сохранять для следующего клика — под
    /// тем же локом, что и остальное состояние, потому что это должно
    /// меняться "на лету" (см. `Recorder::set_capture_config`), без
    /// пересоздания фонового потока.
    capture: ScreenCaptureConfig,
    compress_repeats: bool,
    /// Счётчик поколений записи: инкрементится в каждом `start()`. Нужен
    /// только колбэку: когда лок брошен на время снимка экрана, `stop()` и
    /// `start()` могут успеть пролететь ОБЕ — тогда после ре-лока `tracker`
    /// снова `Some`, но это уже другая запись, и зависший шаг попадать в
    /// неё не должен (см. проверку в `spawn_listener`).
    generation: u64,
    /// Причина, по которой захват ввода недоступен (`listen` упал в потоке
    /// листенера — например, Wayland-сессия без X11). `None`, пока захват
    /// жив или исход потока ещё неизвестен.
    listen_error: Option<String>,
}

pub struct Recorder {
    shared: Arc<Mutex<Shared>>,
    // Держим handle, чтобы поток не был "оторван" — join() в проекте сознательно
    // не вызываем нигде (поток живёт до конца процесса), но хранить его — хорошая
    // гигиена на случай будущего graceful shutdown.
    _listener_handle: JoinHandle<()>,
}

impl Recorder {
    pub fn new(backend: Arc<dyn InputBackend>) -> Self {
        Self::with_screen_guard(backend, None, ScreenCaptureConfig::NONE, false)
    }

    /// То же самое, но с опциональным screen-guard: если передан и `capture`
    /// просит хотя бы что-то (см. [`ScreenCaptureConfig::wants_capture`]), при
    /// каждом клике будет делаться снимок области экрана вокруг курсора (см.
    /// `crate::screen_guard`).
    pub fn with_screen_guard(
        backend: Arc<dyn InputBackend>,
        screen_guard: Option<Arc<dyn ScreenGuard>>,
        capture: ScreenCaptureConfig,
        compress_repeats: bool,
    ) -> Self {
        let shared = Arc::new(Mutex::new(Shared {
            tracker: None,
            steps: Vec::new(),
            last_mouse_pos: (0.0, 0.0),
            capture,
            compress_repeats,
            generation: 0,
            listen_error: None,
        }));
        let listener_handle = spawn_listener(backend, Arc::clone(&shared), screen_guard);
        Self {
            shared,
            _listener_handle: listener_handle,
        }
    }

    /// Меняет, что сохранять для следующих кликов, "на лету" — без
    /// пересоздания рекордера и его фонового потока (который живёт всё время
    /// работы приложения, см. комментарий в начале файла). Уже записанные шаги
    /// не трогает; действует немедленно, в т.ч. посреди активной записи.
    pub fn set_capture_config(&self, capture: ScreenCaptureConfig) {
        self.shared.lock().unwrap().capture = capture;
    }

    pub fn set_compress_repeats(&self, on: bool) {
        self.shared.lock().unwrap().compress_repeats = on;
    }

    pub fn capture_config(&self) -> ScreenCaptureConfig {
        self.shared.lock().unwrap().capture
    }

    /// Начинает запись. Ошибка, если запись уже идёт.
    pub fn start(&self) -> Result<(), CoreError> {
        let mut s = self.shared.lock().unwrap();
        if s.tracker.is_some() {
            return Err(CoreError::AlreadyRecording);
        }
        s.steps.clear();
        // Новое поколение записи: колбэк мог «висеть» в снимке экрана ещё со
        // времён предыдущей — по этому счётчику он поймёт, что его шаг уже
        // никому не нужен, даже если tracker снова Some.
        s.generation = s.generation.wrapping_add(1);
        s.tracker = Some(DelayTracker::new(Instant::now()));
        Ok(())
    }

    /// Ставит запись на паузу. Входящие события ввода будут отбрасываться, пока
    /// не вызван [`Recorder::resume`].
    pub fn pause(&self) -> Result<(), CoreError> {
        let mut s = self.shared.lock().unwrap();
        match s.tracker.as_mut() {
            Some(tracker) => {
                tracker.pause();
                Ok(())
            }
            None => Err(CoreError::NotRecording),
        }
    }

    /// Снимает запись с паузы. Время, проведённое на паузе, не попадёт в задержку
    /// следующего записанного шага.
    pub fn resume(&self) -> Result<(), CoreError> {
        let now = Instant::now();
        let mut s = self.shared.lock().unwrap();
        match s.tracker.as_mut() {
            Some(tracker) => {
                tracker.resume(now);
                Ok(())
            }
            None => Err(CoreError::NotRecording),
        }
    }

    /// Останавливает запись и возвращает накопленные шаги.
    pub fn stop(&self) -> Result<Vec<MacroStep>, CoreError> {
        let mut s = self.shared.lock().unwrap();
        if s.tracker.is_none() {
            return Err(CoreError::NotRecording);
        }
        s.tracker = None;
        Ok(std::mem::take(&mut s.steps))
    }

    pub fn is_recording(&self) -> bool {
        self.shared.lock().unwrap().tracker.is_some()
    }

    pub fn is_paused(&self) -> bool {
        self.shared
            .lock()
            .unwrap()
            .tracker
            .as_ref()
            .is_some_and(DelayTracker::is_paused)
    }

    /// Сколько шагов уже записано — удобно для индикатора в UI ("Запись... 42").
    pub fn recorded_step_count(&self) -> usize {
        self.shared.lock().unwrap().steps.len()
    }

    /// Причина недоступности захвата ввода, если он действительно недоступен.
    /// `Some` означает: события от ОС не поступят никогда, запись работать
    /// не будет — UI обязан показать это пользователю, а не делать вид,
    /// что всё хорошо (см. баннер в `draw_recording_section`).
    pub fn listen_error(&self) -> Option<String> {
        self.shared.lock().unwrap().listen_error.clone()
    }
}

/// Коалесинг движений мыши: подряд идущие `MouseMove` схлопываются в один шаг.
///
/// Реальное движение курсора генерирует событие почти на каждый пиксель — без
/// схлопывания секундное движение пишется сотнями шагов: пухнет JSON, медленный
/// редактор, а при воспроизведении каждое промежуточное событие — отдельный
/// дорогой вызов `simulate()`.
///
/// Семантика: позиция последнего шага заменяется на текущую, а всё время пути
/// суммируется в его `delay_ms`. Воспроизведение прыгает сразу в конечную точку,
/// потратив суммарное время пути, — как сделал бы пользователь, только без
/// промежуточных точек.
///
/// Возвращает `true`, если шаг поглощён предыдущим (пушить новый не нужно).
fn absorb_mouse_move(steps: &mut [MacroStep], x: f64, y: f64, delay_ms: u64) -> bool {
    let Some(last) = steps.last_mut() else {
        return false;
    };
    if !matches!(last.event, InputEvent::MouseMove { .. }) {
        return false;
    }
    last.event = InputEvent::MouseMove { x, y };
    // Переполнение u64 миллисекунд нереально, но и цена нулевая.
    last.delay_ms = last.delay_ms.saturating_add(delay_ms);
    true
}

/// Схлопывание авто-повтора клавиши в предыдущий шаг той же нажатой клавиши.
/// Ожидаемый случай: серия KeyPress одной клавиши без Release между ними —
/// это генерируемый ОС авто-повтор при удержании. Возвращает `true`,
/// если повтор поглощён; `false` — пусть колбэк запишет шаг как обычно.
fn absorb_key_repeat(steps: &mut [MacroStep], key: Key, delay_ms: u64) -> bool {
    let Some(last) = steps.last_mut() else {
        return false;
    };
    let matches_press =
        matches!(&last.event, InputEvent::Key { key: k, pressed: true } if *k == key);
    if matches_press {
        last.delay_ms = last.delay_ms.saturating_add(delay_ms);
    }
    matches_press
}

fn spawn_listener(
    backend: Arc<dyn InputBackend>,
    shared: Arc<Mutex<Shared>>,
    screen_guard: Option<Arc<dyn ScreenGuard>>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut pressed_keys: std::collections::HashSet<Key> = std::collections::HashSet::new();
        // Отдельный хэндл Shared для отчёта об ошибке запуска листенера: сам
        // `shared` следующим движением переезжает в колбэк (move-замыкание) и в
        // теле потока становится недоступен, а причину падения listen() нужно
        // записать именно в общий Shared — её прочитает Recorder::listen_error().
        let shared_for_listen_error = Arc::clone(&shared);
        let callback: Box<dyn FnMut(InputEvent) + Send> = Box::new(move |event| {
            // Классификация авто-повтора: всегда, даже вне записи и на паузе —
            // иначе пропущенные отпускания испортят классификацию после resume.
            let mut is_repeat_press = false;
            if let InputEvent::Key { key, pressed } = event {
                if pressed {
                    is_repeat_press = !pressed_keys.insert(key); // insert отвечает «была ли уже зажата»
                } else {
                    pressed_keys.remove(&key);
                }
            }
            let now = Instant::now();
            let mut s = shared.lock().unwrap();

            if let InputEvent::MouseMove { x, y } = event {
                s.last_mouse_pos = (x, y);
            }

            let delay_ms = match s.tracker.as_mut() {
                Some(tracker) => tracker.record_event(now),
                None => return, // запись не идёт — событие не наше дело
            };
            let Some(delay_ms) = delay_ms else { return }; // на паузе

            let is_click = matches!(event, InputEvent::MouseButton { pressed: true, .. });
            let (fingerprint, thumbnail) = if is_click {
                let capture = s.capture;
                if capture.wants_capture() {
                    if let Some(guard) = screen_guard.as_ref() {
                        let (x, y) = s.last_mouse_pos;
                        // Поколение записи на момент ухода в снимок. После ре-лока
                        // сравним: несовпадение значит, что за время снимка запись
                        // успели остановить и начать заново.
                        let generation = s.generation;
                        // Не держим лок Shared на время захвата экрана — это не
                        // микросекундная операция, а pause()/is_recording() из UI-потока
                        // не должны на неё натыкаться.
                        drop(s);
                        // Если нужны оба — отпечаток и миниатюра всё равно считаются
                        // из ОДНОГО снимка (иначе между двумя отдельными захватами
                        // экран мог бы успеть измениться, и превью показывало бы не
                        // совсем то, что сверяется при воспроизведении). А вот какие
                        // из них реально нужны — решает `capture`: раньше оба
                        // считались безусловно, теперь это независимые настройки
                        // (см. `ScreenCaptureConfig`).
                        let snapshot = guard.capture_region(x, y, DEFAULT_REGION_SIZE).ok();
                        let fp = if capture.fingerprint {
                            snapshot.as_ref().map(ScreenFingerprint::from_snapshot)
                        } else {
                            None
                        };
                        let thumb = if capture.thumbnail {
                            snapshot.as_ref().map(ScreenThumbnail::from_snapshot)
                        } else {
                            None
                        };
                        s = shared.lock().unwrap();
                        // Пока лок был брошен на время снимка, запись могли остановить
                        // (tracker станет None) или даже успеть остановить и начать
                        // заново (generation изменится). В обоих случаях шаг чужой:
                        // новый трекер считает время от собственного Instant::now(),
                        // наш delay_ms, посчитанный до снимка, для него — ложь.
                        if s.tracker.is_none() || s.generation != generation {
                            return;
                        }
                        (fp, thumb)
                    } else {
                        (None, None)
                    }
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            };

            // Схлопывать можно только движения; клики/скроллы/клавиши доходят
            // до push как есть. Движения всегда приходят без отпечатка и
            // миниатюры (захват делается только для кликов), так что поглощённый
            // шаг ничего нужного не теряет.
            if let InputEvent::MouseMove { x, y } = event {
                if absorb_mouse_move(&mut s.steps, x, y, delay_ms) {
                    return;
                }
            }

            // Поглощение авто-повтора (правка №5): только если это повтор той же
            // зажатой клавиши И включена настройка сжатия.
            if let InputEvent::Key { key, pressed: true } = event {
                if is_repeat_press
                    && s.compress_repeats
                    && absorb_key_repeat(&mut s.steps, key, delay_ms)
                {
                    return;
                }
            }

            s.steps.push(MacroStep {
                event,
                delay_ms,
                screen_fingerprint: fingerprint,
                screen_thumbnail: thumbnail,
            });
        });

        if let Err(err) = backend.listen(callback) {
            tracing::error!(%err, "поток записи событий ввода завершился с ошибкой");
            // Лог виден только тому, кто его читает; пользователь же видит
            // приложение, которое «зависает», но ничего не записывает.
            // Запоминаем причину — её покажет баннер в UI.
            shared_for_listen_error.lock().unwrap().listen_error = Some(err.to_string());
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Condvar;
    use std::time::Duration;

    use super::*;
    use crate::backend::mock_backend::MockBackend;
    use crate::screen_guard::mock_guard::MockScreenGuard;
    use crate::screen_guard::RegionSnapshot;

    #[test]
    fn events_before_start_are_ignored() {
        let backend = MockBackend::new();
        let recorder = Recorder::new(backend.clone());

        backend.feed(InputEvent::MouseMove { x: 0.0, y: 0.0 });

        recorder.start().unwrap();
        let steps = recorder.stop().unwrap();
        backend.release();

        assert!(steps.is_empty());
    }

    #[test]
    fn start_twice_is_an_error() {
        let backend = MockBackend::new();
        let recorder = Recorder::new(backend.clone());

        recorder.start().unwrap();
        assert!(matches!(recorder.start(), Err(CoreError::AlreadyRecording)));

        recorder.stop().unwrap();
        backend.release();
    }

    #[test]
    fn pause_without_recording_is_an_error() {
        let backend = MockBackend::new();
        let recorder = Recorder::new(backend.clone());

        assert!(matches!(recorder.pause(), Err(CoreError::NotRecording)));
        backend.release();
    }

    #[test]
    fn end_to_end_pause_excludes_paused_time_from_timing() {
        let backend = MockBackend::new();
        let recorder = Recorder::new(backend.clone());

        recorder.start().unwrap();

        backend.feed(InputEvent::Key {
            key: crate::event::Key::KeyA,
            pressed: true,
        });
        thread::sleep(Duration::from_millis(30));
        backend.feed(InputEvent::Key {
            key: crate::event::Key::KeyA,
            pressed: false,
        });

        recorder.pause().unwrap();
        assert!(recorder.is_paused());
        // события во время паузы игнорируются
        backend.feed(InputEvent::MouseMove { x: 999.0, y: 999.0 });
        // Пауза заведомо длиннее всех допусков ниже: если бы она хоть как-то
        // попала в тайминг следующего шага, тест увидел бы это с запасом,
        // а не на грани погрешности планировщика.
        thread::sleep(Duration::from_millis(400)); // имитация "зависшего интернета"
        recorder.resume().unwrap();
        assert!(!recorder.is_paused());

        thread::sleep(Duration::from_millis(20));
        backend.feed(InputEvent::MouseMove { x: 3.0, y: 3.0 });

        let steps = recorder.stop().unwrap();
        backend.release();

        assert_eq!(
            steps.len(),
            3,
            "событие во время паузы не должно быть записано"
        );
        // Допуски широкие сознательно: CI-раннер задерживает поток теста на
        // десятки-сотни мс (реальный случай: 30-мс сон дал дельту 158), и это
        // НЕ нарушение тестируемого свойства. Нарушение — только если в delay
        // попало ВРЕМЯ ПАУЗЫ (~400 мс): тогда значение превысит порог 300
        // гарантированно, а не из-за шума.
        assert!(
            steps[1].delay_ms <= 500,
            "сон 30 мс записался как {}: слишком много даже для CI",
            steps[1].delay_ms
        );
        assert!(
            steps[2].delay_ms <= 300,
            "пауза (400 мс) протекла в тайминг шага: delay_ms = {}",
            steps[2].delay_ms
        );
    }

    #[test]
    fn click_gets_screen_fingerprint_and_thumbnail_when_both_enabled() {
        let backend = MockBackend::new();
        let snapshot = RegionSnapshot {
            width: 4,
            height: 4,
            rgba: vec![10u8; 4 * 4 * 4],
        };
        let guard = Arc::new(MockScreenGuard::new(snapshot));
        let recorder = Recorder::with_screen_guard(
            backend.clone(),
            Some(guard),
            ScreenCaptureConfig {
                fingerprint: true,
                thumbnail: true,
            },
            false,
        );

        recorder.start().unwrap();
        backend.feed(InputEvent::MouseMove { x: 50.0, y: 60.0 });
        backend.feed(InputEvent::MouseButton {
            button: crate::event::MouseButton::Left,
            pressed: true,
        });

        let steps = recorder.stop().unwrap();
        backend.release();

        assert_eq!(steps.len(), 2);
        assert!(
            steps[0].screen_fingerprint.is_none(),
            "у движения мыши отпечатка быть не должно"
        );
        assert!(
            steps[1].screen_fingerprint.is_some(),
            "у клика должен появиться отпечаток"
        );
        assert!(
            steps[1].screen_thumbnail.is_some(),
            "у клика должна появиться и миниатюра — из того же снимка, что и отпечаток"
        );
        assert!(!steps[1].screen_thumbnail.as_ref().unwrap().rgba.is_empty());
    }

    /// Ключевой сценарий для задачи "раздельные настройки screen-guard": можно
    /// держать защиту от "уехавшего" интерфейса включённой, но не писать в
    /// файл макроса ни одного настоящего фрагмента экрана.
    #[test]
    fn click_can_capture_fingerprint_without_thumbnail() {
        let backend = MockBackend::new();
        let snapshot = RegionSnapshot {
            width: 4,
            height: 4,
            rgba: vec![10u8; 4 * 4 * 4],
        };
        let guard = Arc::new(MockScreenGuard::new(snapshot));
        let recorder = Recorder::with_screen_guard(
            backend.clone(),
            Some(guard),
            ScreenCaptureConfig {
                fingerprint: true,
                thumbnail: false,
            },
            false,
        );

        recorder.start().unwrap();
        backend.feed(InputEvent::MouseButton {
            button: crate::event::MouseButton::Left,
            pressed: true,
        });

        let steps = recorder.stop().unwrap();
        backend.release();

        assert_eq!(steps.len(), 1);
        assert!(steps[0].screen_fingerprint.is_some());
        assert!(
            steps[0].screen_thumbnail.is_none(),
            "миниатюра выключена настройкой — сохраняться не должна"
        );
    }

    /// Симметричный сценарий: удобное превью в редакторе шагов без активной
    /// автопаузы при воспроизведении (нет отпечатка — нечего сверять).
    #[test]
    fn click_can_capture_thumbnail_without_fingerprint() {
        let backend = MockBackend::new();
        let snapshot = RegionSnapshot {
            width: 4,
            height: 4,
            rgba: vec![10u8; 4 * 4 * 4],
        };
        let guard = Arc::new(MockScreenGuard::new(snapshot));
        let recorder = Recorder::with_screen_guard(
            backend.clone(),
            Some(guard),
            ScreenCaptureConfig {
                fingerprint: false,
                thumbnail: true,
            },
            false,
        );

        recorder.start().unwrap();
        backend.feed(InputEvent::MouseButton {
            button: crate::event::MouseButton::Left,
            pressed: true,
        });

        let steps = recorder.stop().unwrap();
        backend.release();

        assert_eq!(steps.len(), 1);
        assert!(
            steps[0].screen_fingerprint.is_none(),
            "отпечаток выключен настройкой — сохраняться не должен"
        );
        assert!(steps[0].screen_thumbnail.is_some());
    }

    #[test]
    fn set_capture_config_takes_effect_without_recreating_recorder() {
        let backend = MockBackend::new();
        let snapshot = RegionSnapshot {
            width: 4,
            height: 4,
            rgba: vec![10u8; 4 * 4 * 4],
        };
        let guard = Arc::new(MockScreenGuard::new(snapshot));
        // Стартуем вообще без захвата — как будто пользователь пока ничего не
        // включил в настройках.
        let recorder = Recorder::with_screen_guard(
            backend.clone(),
            Some(guard),
            ScreenCaptureConfig::NONE,
            false,
        );

        recorder.start().unwrap();
        backend.feed(InputEvent::MouseButton {
            button: crate::event::MouseButton::Left,
            pressed: true,
        });

        // Пользователь включил чекбокс прямо посреди записи.
        recorder.set_capture_config(ScreenCaptureConfig {
            fingerprint: true,
            thumbnail: true,
        });
        assert_eq!(
            recorder.capture_config(),
            ScreenCaptureConfig {
                fingerprint: true,
                thumbnail: true,
            }
        );

        backend.feed(InputEvent::MouseButton {
            button: crate::event::MouseButton::Right,
            pressed: true,
        });

        let steps = recorder.stop().unwrap();
        backend.release();

        assert_eq!(steps.len(), 2);
        assert!(
            steps[0].screen_fingerprint.is_none(),
            "клик до переключения настройки — без отпечатка"
        );
        assert!(
            steps[1].screen_fingerprint.is_some(),
            "клик после переключения настройки — с отпечатком"
        );
    }

    #[test]
    fn click_without_screen_guard_has_no_fingerprint() {
        let backend = MockBackend::new();
        let recorder = Recorder::new(backend.clone()); // без screen-guard

        recorder.start().unwrap();
        backend.feed(InputEvent::MouseButton {
            button: crate::event::MouseButton::Left,
            pressed: true,
        });

        let steps = recorder.stop().unwrap();
        backend.release();

        assert_eq!(steps.len(), 1);
        assert!(steps[0].screen_fingerprint.is_none());
    }

    #[test]
    fn consecutive_moves_collapse_into_one_step() {
        let mut steps = vec![MacroStep::new(InputEvent::MouseMove { x: 1.0, y: 1.0 }, 10)];

        assert!(absorb_mouse_move(&mut steps, 2.0, 2.0, 15));

        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].event, InputEvent::MouseMove { x: 2.0, y: 2.0 });
        assert_eq!(steps[0].delay_ms, 25);
    }

    #[test]
    fn first_move_is_not_absorbed_on_empty_history() {
        let mut steps = Vec::new();

        // Нечего поглощать: функция возвращает false и ничего не добавляет —
        // положить шаг в вектор — работа вызывающего кода (колбэка).
        assert!(!absorb_mouse_move(&mut steps, 1.0, 1.0, 4));
        assert!(steps.is_empty());
    }

    #[test]
    fn move_after_click_is_not_absorbed() {
        let mut steps = vec![MacroStep::new(
            InputEvent::MouseButton {
                button: crate::event::MouseButton::Left,
                pressed: true,
            },
            5,
        )];

        assert!(!absorb_mouse_move(&mut steps, 9.0, 9.0, 7));

        // Последний шаг — клик: он не тронут, новую запись добавит колбэк.
        assert_eq!(steps.len(), 1);
        assert!(matches!(steps[0].event, InputEvent::MouseButton { .. }));
        assert_eq!(steps[0].delay_ms, 5);
    }

    #[test]
    fn recorded_moves_are_coalesced_end_to_end() {
        let backend = MockBackend::new();
        let recorder = Recorder::new(backend.clone());
        recorder.start().unwrap();

        backend.feed(InputEvent::MouseMove { x: 1.0, y: 1.0 });
        backend.feed(InputEvent::MouseMove { x: 2.0, y: 2.0 });
        backend.feed(InputEvent::MouseButton {
            button: crate::event::MouseButton::Left,
            pressed: true,
        });
        backend.feed(InputEvent::MouseMove { x: 3.0, y: 3.0 });

        let steps = recorder.stop().unwrap();
        backend.release();

        assert_eq!(steps.len(), 3);
        assert_eq!(
            steps[0].event,
            InputEvent::MouseMove { x: 2.0, y: 2.0 },
            "два движения до клика схлопнулись в последнее"
        );
        assert_eq!(
            steps[2].event,
            InputEvent::MouseMove { x: 3.0, y: 3.0 },
            "движение после клика — отдельный шаг"
        );
    }

    #[test]
    fn key_repeat_absorbs_into_matching_press() {
        let mut steps = vec![MacroStep::new(
            InputEvent::Key {
                key: crate::event::Key::KeyA,
                pressed: true,
            },
            40,
        )];

        assert!(absorb_key_repeat(&mut steps, crate::event::Key::KeyA, 33));

        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].delay_ms, 73);
    }

    #[test]
    fn key_repeat_of_other_key_is_not_absorbed() {
        let mut steps = vec![MacroStep::new(
            InputEvent::Key {
                key: crate::event::Key::KeyA,
                pressed: true,
            },
            40,
        )];

        assert!(!absorb_key_repeat(&mut steps, crate::event::Key::KeyB, 33));

        assert_eq!(steps[0].delay_ms, 40);
    }

    #[test]
    fn auto_repeat_compressed_when_enabled() {
        let backend = MockBackend::new();
        let recorder = Recorder::new(backend.clone());
        recorder.set_compress_repeats(true);
        recorder.start().unwrap();

        let press_a = InputEvent::Key {
            key: crate::event::Key::KeyA,
            pressed: true,
        };
        let release_a = InputEvent::Key {
            key: crate::event::Key::KeyA,
            pressed: false,
        };
        backend.feed(press_a); // первое нажатие
        backend.feed(press_a); // авто-повтор → поглощается
        backend.feed(release_a);
        backend.feed(press_a); // новое нажатие после отпускания

        let steps = recorder.stop().unwrap();
        backend.release();

        assert_eq!(steps.len(), 3, "нажатие+повтор, отпускание, новое нажатие");
    }

    #[test]
    fn auto_repeat_kept_verbatim_by_default() {
        let backend = MockBackend::new();
        let recorder = Recorder::new(backend.clone());
        recorder.start().unwrap();

        let press_a = InputEvent::Key {
            key: crate::event::Key::KeyA,
            pressed: true,
        };
        let release_a = InputEvent::Key {
            key: crate::event::Key::KeyA,
            pressed: false,
        };
        backend.feed(press_a);
        backend.feed(press_a);
        backend.feed(release_a);

        let steps = recorder.stop().unwrap();
        backend.release();

        assert_eq!(steps.len(), 3, "без сжатия повтор остаётся отдельным шагом");
    }

    /// Guard с медленным захватом: растягивает окно гонки «stop()+start()
    /// за время снимка» из микросекунд в управляемые миллисекунды и
    /// сигнализирует барьером, когда колбэк вошёл в захват.
    struct SlowCaptureGuard {
        delay: Duration,
        started: Arc<(Mutex<bool>, Condvar)>,
    }

    impl ScreenGuard for SlowCaptureGuard {
        fn capture_region(
            &self,
            _x: f64,
            _y: f64,
            _size: u32,
        ) -> Result<RegionSnapshot, CoreError> {
            let (lock, cvar) = &*self.started;
            *lock.lock().unwrap() = true;
            cvar.notify_all();
            thread::sleep(self.delay);
            Ok(RegionSnapshot {
                width: 4,
                height: 4,
                rgba: vec![10u8; 4 * 4 * 4],
            })
        }
    }

    #[test]
    fn orphaned_click_does_not_leak_into_restarted_recording() {
        let backend = MockBackend::new();
        let started = Arc::new((Mutex::new(false), Condvar::new()));
        let guard = Arc::new(SlowCaptureGuard {
            delay: Duration::from_millis(300),
            started: Arc::clone(&started),
        });
        let recorder = Recorder::with_screen_guard(
            backend.clone(),
            Some(guard),
            ScreenCaptureConfig {
                fingerprint: true,
                thumbnail: false,
            },
            false,
        );

        recorder.start().unwrap(); // поколение 1
        backend.feed(InputEvent::MouseButton {
            button: crate::event::MouseButton::Left,
            pressed: true,
        });

        // Барьер: ждём, пока колбэк гарантированно войдёт в capture_region.
        let (lock, cvar) = &*started;
        let mut entered = lock.lock().unwrap();
        while !*entered {
            let (next, timeout) = cvar.wait_timeout(entered, Duration::from_secs(2)).unwrap();
            entered = next;
            assert!(
                !timeout.timed_out(),
                "колбэк так и не дошёл до захвата экрана"
            );
        }
        drop(entered);

        // Обе операции пролетают, пока колбэк спит внутри снимка.
        recorder.stop().unwrap(); // шаги записи 1 забраны
        recorder.start().unwrap(); // поколение 2

        // Даём колбэку выйти из снимка (300 мс сна + запас).
        thread::sleep(Duration::from_millis(500));

        let steps = recorder.stop().unwrap();
        backend.release();

        assert!(
            steps.is_empty(),
            "клик из остановленной записи не должен попасть в новую, а попало: {} шагов",
            steps.len()
        );
    }

    #[test]
    fn listen_failure_is_exposed_via_listen_error() {
        let backend = MockBackend::new();
        backend.fail_listen("wayland session");
        let recorder = Recorder::new(backend.clone());

        // Ошибку выставляет фоновый поток листенера — она появляется через
        // микрозадержку после создания Recorder, поэтому ждём её, а не
        // проверяем немедленно.
        let deadline = Instant::now() + Duration::from_secs(2);
        let err = loop {
            if let Some(err) = recorder.listen_error() {
                break err;
            }
            assert!(
                Instant::now() < deadline,
                "listen_error так и не появился за 2 секунды"
            );
            thread::sleep(Duration::from_millis(10));
        };

        assert!(
            err.contains("wayland"),
            "в сообщении должна быть причина из fail_listen, а было: {err}"
        );

        // И повторно: геттер идемпотентен, ошибка не «съедается» чтением.
        assert!(recorder.listen_error().is_some());
    }
}
