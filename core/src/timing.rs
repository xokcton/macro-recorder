//! Арифметика таймингов записи, вынесенная в отдельный чистый (без потоков, без ОС)
//! компонент специально для того, чтобы её можно было юнит-тестировать без реального
//! `sleep` и без захвата событий ОС.
//!
//! Главное требование: если запись поставили на паузу (например, ждали ответа
//! сайта из-за плохого интернета), время паузы НЕ должно попасть в задержку
//! следующего записанного шага — иначе воспроизведение будет неоправданно ждать,
//! и, что хуже, у пользователя нет причин доверять таймингам вообще.

use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Recording,
    Paused,
}

/// Отслеживает задержки между событиями записи с учётом пауз.
pub struct DelayTracker {
    state: State,
    last_active: Instant,
}

impl DelayTracker {
    /// Начинает отслеживание в момент времени `now` (обычно — старт записи).
    pub fn new(now: Instant) -> Self {
        Self {
            state: State::Recording,
            last_active: now,
        }
    }

    pub fn pause(&mut self) {
        self.state = State::Paused;
    }

    /// Возобновляет запись. Критично: точка отсчёта задержки сбрасывается на `now`,
    /// поэтому пауза "выпадает" из тайминга следующего шага.
    pub fn resume(&mut self, now: Instant) {
        self.state = State::Recording;
        self.last_active = now;
    }

    pub fn is_paused(&self) -> bool {
        self.state == State::Paused
    }

    /// Регистрирует поступившее в момент `now` событие.
    ///
    /// Возвращает `Some(delay_ms)` — сколько миллисекунд прошло с прошлого
    /// зафиксированного события — если событие нужно записать, либо `None`,
    /// если сейчас пауза и событие должно быть отброшено.
    pub fn record_event(&mut self, now: Instant) -> Option<u64> {
        if self.state == State::Paused {
            return None;
        }
        let delay_ms = now.saturating_duration_since(self.last_active).as_millis() as u64;
        self.last_active = now;
        Some(delay_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn first_event_delay_is_measured_from_start() {
        let t0 = Instant::now();
        let mut tracker = DelayTracker::new(t0);

        let delay = tracker.record_event(t0 + Duration::from_millis(150));
        assert_eq!(delay, Some(150));
    }

    #[test]
    fn consecutive_events_measure_delta_between_each_other() {
        let t0 = Instant::now();
        let mut tracker = DelayTracker::new(t0);

        assert_eq!(
            tracker.record_event(t0 + Duration::from_millis(100)),
            Some(100)
        );
        assert_eq!(
            tracker.record_event(t0 + Duration::from_millis(250)),
            Some(150)
        );
    }

    #[test]
    fn events_during_pause_are_dropped() {
        let t0 = Instant::now();
        let mut tracker = DelayTracker::new(t0);
        tracker.pause();

        assert_eq!(tracker.record_event(t0 + Duration::from_secs(30)), None);
        assert!(tracker.is_paused());
    }

    #[test]
    fn pause_duration_is_excluded_from_next_delay() {
        // Сценарий из требования: кликнули, встали на паузу (например, ждём
        // загрузку страницы 10 секунд), сняли с паузы, почти сразу кликнули снова.
        // Пауза НЕ должна попасть в итоговую задержку второго клика.
        let t0 = Instant::now();
        let mut tracker = DelayTracker::new(t0);

        let first_click_at = t0 + Duration::from_millis(200);
        assert_eq!(tracker.record_event(first_click_at), Some(200));

        tracker.pause();
        let resume_at = first_click_at + Duration::from_secs(10);
        tracker.resume(resume_at);

        let second_click_at = resume_at + Duration::from_millis(80);
        let delay = tracker.record_event(second_click_at);

        // 80 мс, а НЕ 10080 мс.
        assert_eq!(delay, Some(80));
    }

    #[test]
    fn resuming_without_events_does_not_leak_paused_time_later() {
        let t0 = Instant::now();
        let mut tracker = DelayTracker::new(t0);

        tracker.pause();
        tracker.resume(t0 + Duration::from_secs(5));
        // Ничего не происходило 300мс после resume — это уже реальная задержка,
        // её как раз нужно учитывать.
        let delay = tracker.record_event(t0 + Duration::from_secs(5) + Duration::from_millis(300));
        assert_eq!(delay, Some(300));
    }
}
