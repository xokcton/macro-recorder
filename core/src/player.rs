//! Движок воспроизведения.
//!
//! В отличие от записи, здесь нет проблемы с "неостанавливаемым" вызовом:
//! `simulate()` — обычный синхронный вызов, поэтому у каждого запуска
//! воспроизведения свой поток, который спокойно завершается сам.
//!
//! Два независимых требования к паузе объединены здесь в один механизм:
//! 1. Ручная пауза (интернет тормозит) — пользователь жмёт хоткей/кнопку.
//! 2. Screen-guard пауза — отпечаток экрана перед кликом не совпал с тем, что
//!    был записан, значит интерфейс поменялся, и слепой клик рискован.
//!
//! Оба случая используют один и тот же `Control` и один и тот же
//! `interruptible_sleep`: ожидание перед каждым шагом реализовано короткими
//! отрезками, а не одним `thread::sleep` на всю задержку, поэтому пауза (ручная
//! или автоматическая) и стоп применяются почти мгновенно.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::backend::InputBackend;
use crate::event::InputEvent;
use crate::macro_data::{LoopMode, Macro, MacroStep};
use crate::screen_guard::{
    ScreenFingerprint, ScreenGuard, DEFAULT_MAX_AVG_CHANNEL_DIFF, DEFAULT_MAX_HAMMING_DISTANCE,
    DEFAULT_REGION_SIZE,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Playing,
    Paused,
}

enum SleepOutcome {
    Continue,
    Stop,
}

struct Control {
    phase: Mutex<Phase>,
    cv: Condvar,
}

impl Control {
    fn new() -> Self {
        Self {
            phase: Mutex::new(Phase::Playing),
            cv: Condvar::new(),
        }
    }

    fn pause(&self) {
        let mut p = self.phase.lock().unwrap();
        if *p == Phase::Playing {
            *p = Phase::Paused;
            self.cv.notify_all();
        }
    }

    fn resume(&self) {
        let mut p = self.phase.lock().unwrap();
        if *p == Phase::Paused {
            *p = Phase::Playing;
            self.cv.notify_all();
        }
    }

    fn stop(&self) {
        let mut p = self.phase.lock().unwrap();
        *p = Phase::Idle;
        self.cv.notify_all();
    }

    fn phase(&self) -> Phase {
        *self.phase.lock().unwrap()
    }

    /// Ждёт `duration`, немедленно прерываясь на `Stop`, и блокируясь без
    /// busy-wait, пока не закончится пауза (ручная или screen-guard).
    fn interruptible_sleep(&self, duration: Duration) -> SleepOutcome {
        let deadline = Instant::now() + duration;
        let mut guard = self.phase.lock().unwrap();
        loop {
            match *guard {
                Phase::Idle => return SleepOutcome::Stop,
                Phase::Paused => {
                    guard = self.cv.wait(guard).unwrap();
                    continue;
                }
                Phase::Playing => {}
            }
            drop(guard);
            let now = Instant::now();
            if now >= deadline {
                return SleepOutcome::Continue;
            }
            let chunk = deadline
                .saturating_duration_since(now)
                .min(Duration::from_millis(25));
            thread::sleep(chunk);
            guard = self.phase.lock().unwrap();
        }
    }
}

/// Почему воспроизведение автоматически встало на паузу перед шагом `step_index`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackWarning {
    pub step_index: usize,
    /// Насколько разошёлся отпечаток (0..=64, чем больше — тем сильнее отличие).
    pub hamming_distance: u32,
}

/// "Пульт управления" уже запущенным воспроизведением.
pub struct PlaybackHandle {
    control: Arc<Control>,
    warning: Arc<Mutex<Option<PlaybackWarning>>>,
    /// `None`, если воспроизведение даже не начинали (пустой макрос) —
    /// поток в этом случае не порождается вовсе.
    _handle: Option<JoinHandle<()>>,
}

impl PlaybackHandle {
    pub fn pause(&self) {
        self.control.pause();
    }

    pub fn resume(&self) {
        self.control.resume();
    }

    pub fn stop(&self) {
        self.control.stop();
    }

    pub fn is_paused(&self) -> bool {
        self.control.phase() == Phase::Paused
    }

    /// `true`, пока воспроизведение либо активно играет, либо стоит на паузе
    /// (то есть ещё не завершилось и не было остановлено).
    pub fn is_playing(&self) -> bool {
        matches!(self.control.phase(), Phase::Playing | Phase::Paused)
    }

    /// Забирает (и сбрасывает) предупреждение screen-guard, если оно появилось —
    /// удобно опрашивать раз в кадр из UI и один раз показать пользователю.
    pub fn take_warning(&self) -> Option<PlaybackWarning> {
        self.warning.lock().unwrap().take()
    }

    /// Блокирует вызывающий поток до фактического завершения воспроизведения
    /// (включая случаи «стояло на паузе» и «остановлено через stop()»).
    ///
    /// GUI никогда не должен звать этот метод — он заморозит интерфейс.
    /// Предназначен для headless-потребителей ядра вроде `macro-recorder-cli`,
    /// которым нужно дождаться конца проигрывания перед выходом из процесса.
    /// Для пустого макроса поток не порождается — метод возвращается сразу.
    pub fn wait(mut self) {
        if let Some(handle) = self._handle.take() {
            let _ = handle.join();
        }
    }
}

pub struct Player {
    backend: Arc<dyn InputBackend>,
    screen_guard: Option<Arc<dyn ScreenGuard>>,
    max_hamming_distance: u32,
}

impl Player {
    pub fn new(backend: Arc<dyn InputBackend>) -> Self {
        Self {
            backend,
            screen_guard: None,
            max_hamming_distance: DEFAULT_MAX_HAMMING_DISTANCE,
        }
    }

    /// Включает проверку экрана перед кликами, для которых при записи был
    /// сохранён отпечаток (см. `Recorder::with_screen_guard`).
    pub fn with_screen_guard(mut self, guard: Arc<dyn ScreenGuard>) -> Self {
        self.screen_guard = Some(guard);
        self
    }

    /// То же самое, но как обычный сеттер, а не билдер — для случая, когда
    /// `Player` уже создан и нужно переключить guard в ответ на изменение
    /// настройки (см. `app::MacroRecorderApp::apply_screen_capture_settings`).
    /// В отличие от `Recorder`, здесь не нужна отдельная синхронизация:
    /// `Player` не хранит долгоживущего фонового потока сам по себе — каждый
    /// `play()` поднимает свой и клонирует `screen_guard` в момент запуска
    /// (см. ниже), поэтому изменение применяется к следующему запуску, а не
    /// задним числом к уже идущему воспроизведению.
    pub fn set_screen_guard(&mut self, guard: Option<Arc<dyn ScreenGuard>>) {
        self.screen_guard = guard;
    }

    /// Насколько строго сравнивать отпечатки (0..=64). Меньше — строже.
    pub fn with_max_hamming_distance(mut self, max_distance: u32) -> Self {
        self.max_hamming_distance = max_distance;
        self
    }

    /// Запускает воспроизведение `macro_` в отдельном потоке со скоростью `speed`
    /// (1.0 — как записано, 2.0 — вдвое быстрее). Возвращает пульт управления
    /// немедленно, не дожидаясь завершения.
    ///
    /// Принимает `Macro` **или** `Arc<Macro>` (`impl Into` — стандартный
    /// `From<T> for Arc<T>` даёт оба варианта бесплатно): GUI хранит список
    /// макросов как `Arc<Macro>`, и запуск по ▶ не должен копировать шаги
    /// и превью.
    pub fn play(&self, macro_: impl Into<Arc<Macro>>, speed: f32) -> PlaybackHandle {
        let macro_ = macro_.into();
        let control = Arc::new(Control::new());
        let warning = Arc::new(Mutex::new(None));

        // Пустой макрос с Times(n)/Infinite без этой проверки породил бы
        // поток, чей внешний цикл не делает ни одной итерации внутреннего:
        // ни сна, ни выхода — ядро под 100% до ручного стопа. Считаем такой
        // макрос сразу завершившимся и поток вообще не порождаем.
        if macro_.steps.is_empty() {
            control.stop(); // Idle => is_playing() сразу false
            return PlaybackHandle {
                control,
                warning,
                _handle: None,
            };
        }

        let backend = Arc::clone(&self.backend);
        let control_for_thread = Arc::clone(&control);
        let warning_for_thread = Arc::clone(&warning);
        let screen_guard = self.screen_guard.clone();
        let max_distance = self.max_hamming_distance;
        let speed = speed.max(0.05); // страхуемся от деления на ноль/отрицательной скорости

        let handle = thread::spawn(move || {
            run_playback(
                &backend,
                &macro_, // &Arc<Macro> автоматически приводится к &Macro
                speed,
                &control_for_thread,
                &screen_guard,
                max_distance,
                &warning_for_thread,
            );
        });

        PlaybackHandle {
            control,
            warning,
            _handle: Some(handle),
        }
    }
}

/// Возвращает `Some(hamming_distance)`, если экран у этого клика разошёлся с
/// записанным сильнее допустимого порога; `None`, если всё совпадает или
/// проверка неприменима (не клик, нет сохранённого отпечатка, guard выключен).
fn should_pause_for_guard(
    step: &MacroStep,
    last_mouse_pos: (f64, f64),
    screen_guard: &Option<Arc<dyn ScreenGuard>>,
    max_distance: u32,
) -> Option<u32> {
    let guard = screen_guard.as_ref()?;
    let expected = step.screen_fingerprint?;
    if !matches!(step.event, InputEvent::MouseButton { pressed: true, .. }) {
        return None;
    }

    let (x, y) = last_mouse_pos;
    match guard.capture_region(x, y, DEFAULT_REGION_SIZE) {
        Ok(snapshot) => {
            let actual = ScreenFingerprint::from_snapshot(&snapshot);
            if expected.matches(&actual, max_distance, DEFAULT_MAX_AVG_CHANNEL_DIFF) {
                None
            } else {
                Some(expected.hamming_distance(&actual))
            }
        }
        Err(err) => {
            tracing::warn!(%err, "screen-guard: не удалось сделать снимок, пропускаю проверку шага");
            None
        }
    }
}

fn run_playback(
    backend: &Arc<dyn InputBackend>,
    macro_: &Macro,
    speed: f32,
    control: &Control,
    screen_guard: &Option<Arc<dyn ScreenGuard>>,
    max_distance: u32,
    warning_slot: &Mutex<Option<PlaybackWarning>>,
) {
    let iterations_limit = match macro_.loop_mode {
        LoopMode::Once => Some(1u32),
        LoopMode::Times(n) => Some(n),
        LoopMode::Infinite => None,
    };

    let mut completed = 0u32;
    let mut last_mouse_pos = (0.0f64, 0.0f64);

    'loops: loop {
        if let Some(limit) = iterations_limit {
            if completed >= limit {
                break;
            }
        }

        let mut step_index = 0usize;
        while step_index < macro_.steps.len() {
            let step = &macro_.steps[step_index];

            let scaled_secs = (step.delay_ms as f64 / 1000.0) / speed as f64;
            if matches!(
                control.interruptible_sleep(Duration::from_secs_f64(scaled_secs)),
                SleepOutcome::Stop
            ) {
                break 'loops;
            }

            if let InputEvent::MouseMove { x, y } = step.event {
                last_mouse_pos = (x, y);
            }

            while let Some(distance) =
                should_pause_for_guard(step, last_mouse_pos, screen_guard, max_distance)
            {
                *warning_slot.lock().unwrap() = Some(PlaybackWarning {
                    step_index,
                    hamming_distance: distance,
                });
                control.pause();
                // Ждём именно снятия паузы/стопа — без повторной задержки delay_ms:
                // задержка была "подождать перед этим кликом", а не "подождать
                // после того, как пользователь разберётся с изменившимся экраном".
                if matches!(
                    control.interruptible_sleep(Duration::ZERO),
                    SleepOutcome::Stop
                ) {
                    break 'loops;
                }
                // Пользователь снял паузу — проверяем ещё раз; если экран всё ещё
                // не совпадает, уйдём на паузу заново с новым предупреждением.
            }

            if let Err(err) = backend.simulate(step.event) {
                tracing::error!(%err, "не удалось отправить событие воспроизведения");
            }
            step_index += 1;
        }
        completed += 1;
    }

    control.stop();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::mock_backend::MockBackend;
    use crate::event::{InputEvent, Key, MouseButton};
    use crate::macro_data::MacroStep;
    use crate::screen_guard::mock_guard::MockScreenGuard;
    use crate::screen_guard::RegionSnapshot;

    fn sample_macro(loop_mode: LoopMode) -> Macro {
        let mut m = Macro::new(
            "test",
            vec![
                MacroStep::new(
                    InputEvent::Key {
                        key: Key::KeyA,
                        pressed: true,
                    },
                    20,
                ),
                MacroStep::new(
                    InputEvent::Key {
                        key: Key::KeyA,
                        pressed: false,
                    },
                    20,
                ),
            ],
        );
        m.loop_mode = loop_mode;
        m
    }

    #[test]
    fn plays_all_steps_once() {
        let backend = MockBackend::new();
        let player = Player::new(backend.clone());
        let handle = player.play(sample_macro(LoopMode::Once), 1.0);

        // Ждём фактического завершения, а не «надеемся, что фиксированного
        // сна хватит»: на перегруженном CI-раннере поток воспроизведения
        // может получить процессор на сотни мс позже. Тестируемое свойство —
        // «завершилось само и ровно один раз» — от скорости машины зависеть
        // не должно.
        let deadline = Instant::now() + Duration::from_secs(5);
        while handle.is_playing() {
            assert!(
                Instant::now() < deadline,
                "воспроизведение двух шагов по 20 мс не завершилось за 5 с"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(backend.simulated.lock().unwrap().len(), 2);
    }

    #[test]
    fn loop_times_repeats_macro() {
        let backend = MockBackend::new();
        let player = Player::new(backend.clone());
        // Скорость x10, чтобы тест не был медленным.
        let handle = player.play(sample_macro(LoopMode::Times(3)), 10.0);

        // Та же логика ожидания, что в plays_all_steps_once.
        let deadline = Instant::now() + Duration::from_secs(5);
        while handle.is_playing() {
            assert!(
                Instant::now() < deadline,
                "3 прохода по 2 шага не воспроизвелись за 5 с"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(backend.simulated.lock().unwrap().len(), 6);
    }

    #[test]
    fn stop_interrupts_infinite_loop() {
        let backend = MockBackend::new();
        let player = Player::new(backend.clone());
        let handle = player.play(sample_macro(LoopMode::Infinite), 20.0);

        thread::sleep(Duration::from_millis(50));
        handle.stop();
        thread::sleep(Duration::from_millis(30));
        let count_after_stop = backend.simulated.lock().unwrap().len();

        thread::sleep(Duration::from_millis(100));
        let count_later = backend.simulated.lock().unwrap().len();

        assert_eq!(
            count_after_stop, count_later,
            "после stop() новых событий быть не должно"
        );
        assert!(!handle.is_playing());
    }

    #[test]
    fn pause_blocks_playback_until_resume() {
        let backend = MockBackend::new();
        let player = Player::new(backend.clone());
        let handle = player.play(sample_macro(LoopMode::Once), 1.0);

        thread::sleep(Duration::from_millis(5));
        handle.pause();
        assert!(handle.is_paused());

        thread::sleep(Duration::from_millis(150));
        let mid = backend.simulated.lock().unwrap().len();
        assert!(
            mid <= 1,
            "на паузе воспроизведение не должно продолжаться: {mid}"
        );

        handle.resume();
        let deadline = Instant::now() + Duration::from_secs(5);
        while handle.is_playing() {
            assert!(
                Instant::now() < deadline,
                "после resume воспроизведение не завершилось за 5 с"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(backend.simulated.lock().unwrap().len(), 2);
    }

    #[test]
    fn empty_macro_with_infinite_loop_finishes_immediately() {
        let backend = MockBackend::new();
        let player = Player::new(backend.clone());

        let mut m = Macro::new("empty", vec![]);
        m.loop_mode = LoopMode::Infinite;
        let handle = player.play(m, 1.0);

        thread::sleep(Duration::from_millis(50));
        assert!(
            !handle.is_playing(),
            "пустой макрос не должен «играть» вечно"
        );
        assert!(backend.simulated.lock().unwrap().is_empty());
    }

    fn click_macro_with_fingerprint(fp: ScreenFingerprint) -> Macro {
        Macro::new(
            "click",
            vec![
                MacroStep::new(InputEvent::MouseMove { x: 10.0, y: 10.0 }, 0),
                MacroStep::with_fingerprint(
                    InputEvent::MouseButton {
                        button: MouseButton::Left,
                        pressed: true,
                    },
                    10,
                    fp,
                ),
            ],
        )
    }

    /// Левая половина светлая, правая — тёмная (см. аналогичный хелпер в
    /// `screen_guard::tests` — здесь своя копия, чтобы не тащить `#[cfg(test)]`
    /// между модулями).
    fn split_snapshot(size: u32) -> RegionSnapshot {
        let mut rgba = Vec::with_capacity((size * size * 4) as usize);
        for _y in 0..size {
            for x in 0..size {
                let v = if x < size / 2 { 240 } else { 10 };
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        RegionSnapshot {
            width: size,
            height: size,
            rgba,
        }
    }

    fn solid_snapshot(size: u32, v: u8) -> RegionSnapshot {
        RegionSnapshot {
            width: size,
            height: size,
            rgba: vec![v; (size * size * 4) as usize],
        }
    }

    #[test]
    fn matching_screen_plays_through_without_pausing() {
        let backend = MockBackend::new();
        let snapshot = split_snapshot(8);
        let recorded_fp = ScreenFingerprint::from_snapshot(&snapshot);
        let guard = Arc::new(MockScreenGuard::new(snapshot));

        let player = Player::new(backend.clone()).with_screen_guard(guard);
        let handle = player.play(click_macro_with_fingerprint(recorded_fp), 20.0);

        let deadline = Instant::now() + Duration::from_secs(5);
        while handle.is_playing() {
            assert!(
                Instant::now() < deadline,
                "воспроизведение не завершилось за 5 с"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert!(handle.take_warning().is_none());
        assert_eq!(backend.simulated.lock().unwrap().len(), 2);
    }

    #[test]
    fn mismatched_screen_pauses_and_warns_then_resume_retries_the_click() {
        let backend = MockBackend::new();
        let recorded_snapshot = split_snapshot(8);
        let recorded_fp = ScreenFingerprint::from_snapshot(&recorded_snapshot);

        // На "экране" сейчас совсем другая, однородная картинка — структурно
        // непохоже на записанный чересполосный паттерн.
        let different_snapshot = solid_snapshot(8, 128);
        let guard = Arc::new(MockScreenGuard::new(different_snapshot));

        let player = Player::new(backend.clone()).with_screen_guard(guard.clone());
        let handle = player.play(click_macro_with_fingerprint(recorded_fp), 20.0);

        // Ждём, пока screen-guard фактически поставит паузу: на загруженном
        // раннере поток может даже не успеть дойти до проверки экрана.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !handle.is_paused() && handle.is_playing() {
            assert!(
                Instant::now() < deadline,
                "воспроизведение не дошло ни до паузы, ни до завершения за 5 с"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            handle.is_paused(),
            "должны были встать на паузу из-за несовпадения экрана"
        );
        let warning = handle.take_warning();
        assert!(warning.is_some(), "должно быть предупреждение");
        assert_eq!(warning.unwrap().step_index, 1);
        // Клик не должен был выполниться вслепую.
        assert_eq!(
            backend.simulated.lock().unwrap().len(),
            1,
            "выполнился только MouseMove, не клик"
        );

        // Пользователь "поправил" ситуацию — экран снова совпадает с записанным.
        guard.set(recorded_snapshot);
        handle.resume();

        let deadline = Instant::now() + Duration::from_secs(5);
        while handle.is_playing() {
            assert!(
                Instant::now() < deadline,
                "после resume воспроизведение не завершилось за 5 с"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            backend.simulated.lock().unwrap().len(),
            2,
            "после resume клик должен пройти"
        );
    }

    /// Отключение guard'а через `set_screen_guard(None)` (то, что делает
    /// настройка "экран-сторож" в UI) должно перестать проверять экран даже
    /// для шагов, у которых отпечаток уже записан — а не только для новых
    /// записей. Иначе выключение галочки в настройках не значило бы "выключено".
    #[test]
    fn set_screen_guard_none_disables_verification_for_steps_with_existing_fingerprint() {
        let backend = MockBackend::new();
        let recorded_snapshot = split_snapshot(8);
        let recorded_fp = ScreenFingerprint::from_snapshot(&recorded_snapshot);

        // Экран сейчас не совпадает с записанным — с включённым guard'ом это
        // встало бы на паузу (см. тест выше).
        let different_snapshot = solid_snapshot(8, 128);
        let guard = Arc::new(MockScreenGuard::new(different_snapshot));

        let mut player = Player::new(backend.clone()).with_screen_guard(guard);
        player.set_screen_guard(None);
        let handle = player.play(click_macro_with_fingerprint(recorded_fp), 20.0);

        let deadline = Instant::now() + Duration::from_secs(5);
        while handle.is_playing() {
            assert!(
                Instant::now() < deadline,
                "воспроизведение не завершилось за 5 с"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            handle.take_warning().is_none(),
            "guard выключен — предупреждений быть не должно"
        );
        assert_eq!(
            backend.simulated.lock().unwrap().len(),
            2,
            "оба шага должны выполниться без проверки экрана"
        );
    }
}
