//! Главный экран приложения.
//!
//! Действия (старт записи, пауза, стоп, играть...) вынесены в отдельные методы
//! и вызываются как из кнопок, так и из опроса хоткеев в `update()` — одна
//! логика, два способа её вызвать.

use std::sync::{mpsc, Arc};

use mrcore::{
    HotkeyAction, InputBackendChoice, Macro, MacroSummary, PlaybackHandle, PlaybackWarning, Player,
    Recorder, ScreenCaptureConfig, ScreenGuard, Settings, SettingsStore, Storage,
};

use crate::hotkeys::AppHotkeys;
use crate::i18n::{self, Lang};
use crate::settings_panel::{self, SettingsPanelEvent, SettingsPanelState};
use crate::step_editor::{self, format_duration, StepEditorAction, StepEditorState};
use crate::tray::{AppTray, TrayCommand};

pub struct MacroRecorderApp {
    recorder: Arc<Recorder>,
    player: Player,
    storage: Storage,
    settings_store: SettingsStore,
    hotkeys: AppHotkeys,
    tray: Option<AppTray>,
    /// Последнее состояние (запись?, воспроизведение?), которое реально
    /// уходило в `tray.set_status`. На Linux `update()` тикает раз в 100 мс
    /// даже в покое (см. `needs_gtk_pump` ниже — без этого не работали бы
    /// клики по меню трея), и без этого кэша `set_status` — а с ним лишняя
    /// аллокация 32×32-иконки в `tray::status_icon` и системный вызов на
    /// смену иконки — дёргался бы 10 раз в секунду вечно, даже когда
    /// состояние не менялось вообще. `None` — трея нет либо `set_status`
    /// ещё ни разу не звали.
    last_tray_status: Option<(bool, bool)>,
    /// События от фоновых пересыльщиков (глобальные хоткеи; меню трея на
    /// Windows/macOS) — см. `crate::events`. Заменяет опрос системных каналов
    /// из `update`: UI спит, пока событий нет, и мгновенно просыпается по ним.
    events_tx: mpsc::Sender<crate::events::AppEvent>,
    events_rx: mpsc::Receiver<crate::events::AppEvent>,
    /// Пересыльщики стартуются один раз — в первом кадре, когда уже известен
    /// `egui::Context`, который нужен им, чтобы будить GUI.
    forwarders_started: bool,

    /// Возможность вообще делать снимки экрана в этой сборке — `None`, если
    /// собрано без Cargo-фичи `screen-guard` (см. `crate::screen_guard`). Сама
    /// по себе ничего не включает — реально используемое решают
    /// `screen_capture` ниже и чекбоксы в настройках.
    screen_guard_capability: Option<Arc<dyn ScreenGuard>>,
    /// Что именно сохранять при клике — независимые флаги: отпечаток (для
    /// автопаузы при воспроизведении) и миниатюра (для превью в редакторе
    /// шагов) раньше всегда шли парой, теперь нет. Персистентно, см.
    /// `Settings::screen_capture`.
    screen_capture: ScreenCaptureConfig,
    /// Разрешить привязку хоткея совсем без модификатора — см.
    /// `Settings::allow_bare_hotkeys` и `settings_panel::capture_combo`.
    allow_bare_hotkeys: bool,
    compress_key_repeats: bool,
    /// Выбранный бэкенд ввода — применяется при следующем запуске
    /// (см. `crate::input_backend`).
    input_backend: InputBackendChoice,

    macros: Vec<MacroSummary>,
    playback: Option<PlaybackHandle>,
    last_warning: Option<PlaybackWarning>,
    /// Явно выбранный пользователем макрос (клик по строке в списке) — им же
    /// управляет хоткей/трей "играть". Обновляется и при обычном клике "▶",
    /// чтобы поведение оставалось предсказуемым (последнее, что играли или
    /// выбрали, — то и есть текущий выбор).
    selected_macro_id: Option<uuid::Uuid>,
    editing: Option<StepEditorState>,
    show_settings: bool,
    settings_panel: SettingsPanelState,

    pending_name: String,
    last_recorded_steps: Option<Macro>,
    speed: f32,
    status: String,
}

impl MacroRecorderApp {
    pub fn new() -> Self {
        let settings_store = SettingsStore::new().unwrap_or_else(|err| {
            tracing::error!(%err, "не удалось открыть директорию настроек, использую временную");
            SettingsStore::with_path(
                std::env::temp_dir()
                    .join("macro-recorder-fallback")
                    .join("settings.json"),
            )
        });
        let settings = settings_store.load();
        if let Some(lang) = settings.language.as_deref().and_then(Lang::from_code) {
            i18n::set_lang(lang);
        }

        let screen_guard_capability = crate::screen_guard::capability();

        let backend = Arc::new(crate::input_backend::build_backend(settings.input_backend));
        let recorder = Arc::new(Recorder::with_screen_guard(
            backend.clone(),
            screen_guard_capability.clone(),
            settings.screen_capture,
            settings.compress_key_repeats,
        ));
        let player = Player::new(backend);
        let hotkeys = AppHotkeys::new(&settings.hotkeys);
        let tray = AppTray::new();

        let storage = Storage::new().unwrap_or_else(|err| {
            tracing::error!(%err, "не удалось открыть директорию данных, использую временную");
            Storage::with_dir(std::env::temp_dir().join("macro-recorder-fallback"))
        });
        let macros = storage.list().unwrap_or_default();

        let status = if hotkeys.is_available() {
            i18n::tf(
                "status.ready_with_hotkeys",
                &[
                    hotkeys
                        .combo_for(HotkeyAction::ToggleRecording)
                        .unwrap_or("—"),
                    hotkeys
                        .combo_for(HotkeyAction::TogglePlayback)
                        .unwrap_or("—"),
                    hotkeys.combo_for(HotkeyAction::TogglePause).unwrap_or("—"),
                ],
            )
        } else {
            i18n::t("status.ready_no_hotkeys")
        };

        let (events_tx, events_rx) = mpsc::channel::<crate::events::AppEvent>();
        let mut app = Self {
            recorder,
            player,
            storage,
            settings_store,
            hotkeys,
            tray,
            last_tray_status: None,
            events_tx,
            events_rx,
            forwarders_started: false,
            screen_guard_capability,
            screen_capture: settings.screen_capture,
            allow_bare_hotkeys: settings.allow_bare_hotkeys,
            compress_key_repeats: settings.compress_key_repeats,
            input_backend: settings.input_backend,
            macros,
            playback: None,
            last_warning: None,
            selected_macro_id: None,
            editing: None,
            show_settings: false,
            settings_panel: SettingsPanelState::default(),
            pending_name: String::new(),
            last_recorded_steps: None,
            speed: 1.0,
            status,
        };
        // `recorder` уже получил правильный `capture` при создании (см. выше),
        // но `player` (см. `Player::new`) всегда стартует без guard'а — эта
        // строка включает ему проверку экрана, если она включена в настройках,
        // тем же путём, каким это происходит при переключении чекбокса в UI
        // (см. `apply_screen_capture_settings`/`draw_settings`).
        app.apply_screen_capture_settings();
        app
    }

    fn refresh_macros(&mut self) {
        // Список лёгкий: сводки из индекса, без чтения тел макросов с их
        // превью. Полная загрузка — точечно в ▶/✏ (см. draw_playback_section).
        self.macros = self.storage.list().unwrap_or_default();
    }

    /// Собирает текущее состояние (хоткеи + язык + экран-сторож) и сохраняет
    /// на диск. Ошибка сохранения не должна ронять приложение — только
    /// статус-строка.
    fn persist_settings(&mut self) {
        let settings = Settings {
            hotkeys: self.hotkeys.bindings().to_vec(),
            language: Some(i18n::current_lang().code().to_string()),
            allow_bare_hotkeys: self.allow_bare_hotkeys,
            screen_capture: self.screen_capture,
            compress_key_repeats: self.compress_key_repeats,
            input_backend: self.input_backend,
        };
        if let Err(err) = self.settings_store.save(&settings) {
            tracing::warn!(%err, "не удалось сохранить настройки");
            self.status = i18n::tf("status.save_failed", &[&err.to_string()]);
        }
    }

    /// Приводит уже живые `recorder`/`player` в соответствие с текущим
    /// `screen_capture` — вызывается и один раз при старте (см. `new`), и
    /// каждый раз, когда пользователь меняет чекбоксы экран-сторожа в
    /// настройках (см. `draw_settings`). Ни `Recorder`, ни `Player` не нужно
    /// пересоздавать (см. `Recorder::set_capture_config` и
    /// `Player::set_screen_guard`), так что новая настройка действует сразу,
    /// без перезапуска приложения.
    fn apply_screen_capture_settings(&mut self) {
        self.recorder.set_capture_config(self.screen_capture);
        self.recorder
            .set_compress_repeats(self.compress_key_repeats);
        self.player
            .set_screen_guard(if self.screen_capture.fingerprint {
                self.screen_guard_capability.clone()
            } else {
                None
            });
    }

    // ---- Действия: запись -------------------------------------------------

    fn start_recording(&mut self) {
        if let Err(err) = self.recorder.start() {
            self.status = i18n::tf("status.error", &[&err.to_string()]);
        }
    }

    fn toggle_recording_pause(&mut self) {
        if !self.recorder.is_recording() {
            return;
        }
        let result = if self.recorder.is_paused() {
            self.recorder.resume()
        } else {
            self.recorder.pause()
        };
        if let Err(err) = result {
            self.status = i18n::tf("status.error", &[&err.to_string()]);
        }
    }

    fn stop_recording(&mut self) {
        match self.recorder.stop() {
            Ok(steps) if steps.is_empty() => {
                self.status = i18n::t("status.recording_empty");
            }
            Ok(steps) => {
                self.pending_name = format!("Macro {}", self.macros.len() + 1);
                self.last_recorded_steps = Some(Macro::new("", steps));
                self.status = i18n::t("status.recording_stopped_name_prompt");
            }
            Err(err) => self.status = i18n::tf("status.error", &[&err.to_string()]),
        }
    }

    /// Хоткей записи — один и тот же переключатель старт/стоп.
    fn toggle_recording(&mut self) {
        if self.recorder.is_recording() {
            self.stop_recording();
        } else {
            self.start_recording();
        }
    }

    // ---- Действия: воспроизведение ----------------------------------------

    fn play_macro(&mut self, m: Arc<Macro>) {
        self.selected_macro_id = Some(m.id);
        self.playback = Some(self.player.play(m, self.speed));
        self.last_warning = None;
        self.status = i18n::t("status.playing");
    }

    /// Хоткей воспроизведения: играет явно выбранный макрос (или первый в
    /// списке, если выбора ещё не было), либо останавливает, если уже
    /// что-то играет.
    fn toggle_playback(&mut self) {
        if let Some(handle) = self.playback.take() {
            handle.stop();
            self.status = i18n::t("status.playback_stopped");
            return;
        }

        // Выбранный макрос, если он ещё существует, иначе первый в списке —
        // прежняя семантика, но тело теперь читается с диска в момент запуска,
        // а не висит в памяти всё время жизни приложения.
        let id = self
            .selected_macro_id
            .filter(|id| self.macros.iter().any(|s| s.id == *id))
            .or_else(|| self.macros.first().map(|s| s.id));

        let Some(id) = id else {
            self.status = i18n::t("status.no_macros_to_play");
            return;
        };
        match self.storage.load(id) {
            Ok(m) => self.play_macro(Arc::new(m)),
            Err(err) => self.status = i18n::tf("status.error", &[&err.to_string()]),
        }
    }

    fn toggle_playback_pause(&mut self) {
        let Some(handle) = &self.playback else { return };
        if handle.is_paused() {
            handle.resume();
            self.last_warning = None;
        } else {
            handle.pause();
        }
    }

    /// Хоткей паузы — один физический хоткей, но ставит на паузу то, что
    /// сейчас реально активно (запись или воспроизведение).
    fn toggle_pause(&mut self) {
        if self.recorder.is_recording() {
            self.toggle_recording_pause();
        } else if self.playback.is_some() {
            self.toggle_playback_pause();
        }
    }

    fn stop_all(&mut self) {
        if self.recorder.is_recording() {
            self.stop_recording();
        }
        if let Some(handle) = self.playback.take() {
            handle.stop();
        }
        self.status = i18n::t("status.all_stopped");
    }

    fn dispatch_tray(&mut self, ctx: &egui::Context, command: TrayCommand) {
        match command {
            TrayCommand::ShowWindow => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            TrayCommand::PlayLastMacro => {
                if self.playback.is_none() {
                    self.toggle_playback();
                }
            }
            TrayCommand::StopAll => self.stop_all(),
            TrayCommand::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }

    fn dispatch_hotkey(&mut self, action: HotkeyAction) {
        match action {
            HotkeyAction::ToggleRecording => self.toggle_recording(),
            HotkeyAction::TogglePause => self.toggle_pause(),
            HotkeyAction::TogglePlayback => self.toggle_playback(),
            HotkeyAction::StopAll => self.stop_all(),
        }
    }

    // ---- UI -----------------------------------------------------------

    fn draw_top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button(i18n::t("ui.settings_button")).clicked() {
                self.show_settings = !self.show_settings;
            }
            ui.label(&self.status);
        });
    }

    fn draw_settings(&mut self, ui: &mut egui::Ui) {
        if ui.button(i18n::t("ui.back")).clicked() {
            self.show_settings = false;
        }
        ui.separator();

        match settings_panel::draw(
            ui,
            &mut self.settings_panel,
            &mut self.hotkeys,
            &mut self.allow_bare_hotkeys,
            &mut self.screen_capture,
            &mut self.compress_key_repeats,
            &mut self.input_backend,
        ) {
            Some(SettingsPanelEvent::ScreenCapture) => {
                self.apply_screen_capture_settings();
                self.persist_settings();
            }
            Some(
                SettingsPanelEvent::Language
                | SettingsPanelEvent::Hotkey
                | SettingsPanelEvent::AllowBareHotkeys
                | SettingsPanelEvent::BackendChoice,
            ) => {
                self.persist_settings();
            }
            Some(SettingsPanelEvent::CompressKeyRepeats) => {
                // применяем к живому рекордеру сразу, не дожидаясь перезапуска записи
                self.recorder
                    .set_compress_repeats(self.compress_key_repeats);
                // сохранение — ровно как в ветке AllowBareHotkeys
                self.persist_settings();
            }
            None => {}
        }
    }

    fn draw_recording_section(&mut self, ui: &mut egui::Ui) {
        ui.heading(i18n::t("ui.recording_heading"));
        if let Some(err) = self.recorder.listen_error() {
            // Разные подсказки в зависимости от уже выбранного бэкенда: если
            // это rdevin, есть самообслуживаемый выход (переключиться на
            // evdev); если уже evdev — совет "переключитесь на evdev" был бы
            // бессмысленным, там почти всегда дело в правах на /dev/uinput.
            let key = if self.input_backend == InputBackendChoice::Evdev {
                "ui.recording_unavailable_evdev"
            } else {
                "ui.recording_unavailable"
            };
            ui.colored_label(egui::Color32::from_rgb(210, 70, 70), i18n::tf(key, &[&err]));
        }
        ui.horizontal(|ui| {
            if self.recorder.is_recording() {
                let paused = self.recorder.is_paused();
                if paused {
                    if ui.button(i18n::t("ui.resume_recording")).clicked() {
                        self.toggle_recording_pause();
                    }
                } else if ui.button(i18n::t("ui.pause")).clicked() {
                    self.toggle_recording_pause();
                }

                if ui.button(i18n::t("ui.stop")).clicked() {
                    self.stop_recording();
                }

                ui.label(if paused {
                    i18n::t("ui.paused_label")
                } else {
                    i18n::tf(
                        "ui.recording_progress",
                        &[&self.recorder.recorded_step_count().to_string()],
                    )
                });
            } else if ui.button(i18n::t("ui.start_recording")).clicked() {
                self.start_recording();
            }
        });

        if self.last_recorded_steps.is_some() {
            ui.horizontal(|ui| {
                ui.label(i18n::t("ui.macro_name_label"));
                ui.text_edit_singleline(&mut self.pending_name);
                if ui.button(i18n::t("ui.save")).clicked() {
                    if let Some(recorded) = self.last_recorded_steps.take() {
                        let mut m = recorded;
                        m.name = self.pending_name.clone();
                        match self.storage.save(&m) {
                            Ok(()) => {
                                self.status = i18n::tf("status.saved", &[&m.name]);
                                self.refresh_macros();
                            }
                            Err(err) => {
                                self.status = i18n::tf("status.save_failed", &[&err.to_string()]);
                            }
                        }
                    }
                }
            });
        }
    }

    fn draw_playback_section(&mut self, ui: &mut egui::Ui) {
        ui.separator();

        if self.playback.is_some() {
            if let Some(warning) = self
                .playback
                .as_ref()
                .and_then(PlaybackHandle::take_warning)
            {
                self.last_warning = Some(warning);
            }
            if let Some(warning) = self.last_warning {
                ui.colored_label(
                    egui::Color32::from_rgb(200, 120, 0),
                    i18n::tf(
                        "ui.screen_guard_warning",
                        &[
                            &(warning.step_index + 1).to_string(),
                            &warning.hamming_distance.to_string(),
                        ],
                    ),
                );
            }

            let is_paused = self
                .playback
                .as_ref()
                .is_some_and(PlaybackHandle::is_paused);
            ui.horizontal(|ui| {
                if is_paused {
                    if ui.button(i18n::t("ui.resume")).clicked() {
                        self.toggle_playback_pause();
                    }
                } else if ui.button(i18n::t("ui.pause")).clicked() {
                    self.toggle_playback_pause();
                }
                if ui.button(i18n::t("ui.stop")).clicked() {
                    if let Some(handle) = &self.playback {
                        handle.stop();
                    }
                }
            });

            let still_playing = self
                .playback
                .as_ref()
                .is_some_and(PlaybackHandle::is_playing);
            if !still_playing {
                self.playback = None;
                self.last_warning = None;
                self.status = i18n::t("status.playback_finished");
            }
        }

        if self.editing.is_some() {
            self.draw_step_editor(ui);
            return;
        }

        ui.heading(i18n::t("ui.saved_macros_heading"));

        ui.horizontal(|ui| {
            ui.label(i18n::t("ui.speed_label"));
            ui.add(egui::Slider::new(&mut self.speed, 0.25..=3.0).suffix("x"));
        });

        let mut to_delete: Option<uuid::Uuid> = None;
        let mut to_play: Option<uuid::Uuid> = None;
        let mut to_edit: Option<uuid::Uuid> = None;
        let mut to_select: Option<uuid::Uuid> = None;

        egui::ScrollArea::vertical().show(ui, |ui| {
            for m in &self.macros {
                ui.horizontal(|ui| {
                    let is_selected = self.selected_macro_id == Some(m.id);
                    let key = if is_selected {
                        "ui.macro_row_selected"
                    } else {
                        "ui.macro_row"
                    };
                    let label = i18n::tf(
                        key,
                        &[
                            &m.name,
                            &m.step_count.to_string(),
                            &format_duration(m.total_ms),
                        ],
                    );
                    // Клик по строке — явный выбор макроса для хоткея/трея
                    // "играть", без запуска воспроизведения.
                    if ui.selectable_label(is_selected, label).clicked() {
                        to_select = Some(m.id);
                    }

                    // Второе одновременное воспроизведение невозможно (плеер
                    // один), поэтому ▶ блокируется на время любого проигрывания.
                    // А вот ✏ теперь доступен всегда: правка другого макроса
                    // идущему воспроизведению не мешает.
                    let playing = self.playback.is_some();
                    if ui.add_enabled(!playing, egui::Button::new("▶")).clicked() {
                        to_play = Some(m.id);
                    }
                    if ui.button("✏").clicked() {
                        to_edit = Some(m.id);
                    }
                    if ui.button("🗑").clicked() {
                        to_delete = Some(m.id);
                    }
                });
            }
        });

        if let Some(id) = to_select {
            self.selected_macro_id = Some(id);
        }
        if let Some(id) = to_edit {
            match self.storage.load(id) {
                Ok(m) => self.editing = Some(StepEditorState::new(m)),
                Err(err) => self.status = i18n::tf("status.error", &[&err.to_string()]),
            }
        }
        if let Some(id) = to_play {
            match self.storage.load(id) {
                Ok(m) => self.play_macro(Arc::new(m)),
                Err(err) => self.status = i18n::tf("status.error", &[&err.to_string()]),
            }
        }
        if let Some(id) = to_delete {
            if let Err(err) = self.storage.delete(id) {
                self.status = i18n::tf("status.delete_failed", &[&err.to_string()]);
            }
            if self.selected_macro_id == Some(id) {
                self.selected_macro_id = None;
            }
            self.refresh_macros();
        }
    }

    fn draw_step_editor(&mut self, ui: &mut egui::Ui) {
        ui.heading(i18n::t("ui.editing_heading"));
        let Some(state) = self.editing.as_mut() else {
            return;
        };

        match step_editor::draw(ui, state) {
            Some(StepEditorAction::Save(m)) => {
                match self.storage.save(&m) {
                    Ok(()) => {
                        self.status = i18n::tf("status.edit_saved", &[&m.name]);
                        self.refresh_macros();
                    }
                    Err(err) => self.status = i18n::tf("status.save_failed", &[&err.to_string()]),
                }
                self.editing = None;
            }
            Some(StepEditorAction::Cancel) => {
                self.status = i18n::t("status.edit_cancelled");
                self.editing = None;
            }
            None => {}
        }
    }
}

impl Default for MacroRecorderApp {
    fn default() -> Self {
        Self::new()
    }
}

impl eframe::App for MacroRecorderApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Пересыльщики стартуют один раз, в первом кадре: им нужен клон ctx,
        // чтобы будить GUI из фоновых потоков.
        if !self.forwarders_started {
            self.forwarders_started = true;
            crate::events::spawn_hotkey_forwarder(ctx.clone(), self.events_tx.clone());
            if let Some(tray) = &self.tray {
                tray.spawn_forwarder(ctx.clone(), self.events_tx.clone());
            }
            // Портальные хоткеи — только на похожем на Wayland окружении и когда
            // обычные хоткеи не поднялись: иначе одно действие приходило бы дважды
            // (портал + X11-хук).
            #[cfg(all(target_os = "linux", feature = "portal-hotkeys"))]
            if crate::input_backend::is_wayland_session() && !self.hotkeys.is_available() {
                crate::portal_hotkeys::spawn(
                    ctx.clone(),
                    self.hotkeys.bindings().to_vec(),
                    self.events_tx.clone(),
                );
            }
        }

        // Разбираем события от фоновых пересыльщиков.
        //
        // Пока идёт захват новой комбинации в настройках, хоткеи по всему
        // приложению временно отключены; пришедшие за это время нажатия
        // выбрасываем — иначе после захвата внезапно сработало бы то, что
        // пользователь нажал «случайно».
        let capturing = self.settings_panel.capturing.is_some();
        while let Ok(event) = self.events_rx.try_recv() {
            match event {
                #[cfg(all(target_os = "linux", feature = "portal-hotkeys"))]
                crate::events::AppEvent::PortalHotkey(action) => {
                    if capturing {
                        continue;
                    }
                    self.dispatch_hotkey(action);
                }
                crate::events::AppEvent::HotkeyPressed(id) => {
                    if capturing {
                        continue;
                    }
                    if let Some(action) = self.hotkeys.action_for_id(id) {
                        self.dispatch_hotkey(action);
                    }
                }
                #[cfg(feature = "tray")]
                crate::events::AppEvent::TrayCommand(command) => {
                    self.dispatch_tray(ctx, command);
                }
            }
        }

        // Трей: иконка обновляется только когда состояние реально изменилось;
        // на Linux здесь же прокачивается GTK-очередь и вычитываются клики
        // меню (см. `tray::poll`) — это нужно каждый тик независимо от
        // иконки, поэтому poll() зовём безусловно, а set_status — только
        // на изменение (см. `last_tray_status`).
        if let Some(tray) = &self.tray {
            let commands = tray.poll();
            let current = (self.recorder.is_recording(), self.playback.is_some());
            if self.last_tray_status != Some(current) {
                tray.set_status(current.0, current.1);
                self.last_tray_status = Some(current);
            }
            for command in commands {
                self.dispatch_tray(ctx, command);
            }
        }

        // Периодическая перерисовка нужна ТОЛЬКО когда есть живая динамика:
        // таймер записи, статус воспроизведения, процесс захвата комбинации
        // (ввод ловится покадрово), а на Linux — ещё и прокачка GTK-очереди
        // трея: без неё клики по меню не доходят до приложения, пока окно
        // спит. В покое приложение не тратит CPU — будят его только
        // фоновые пересыльщики.
        let animating = self.recorder.is_recording()
            || self.playback.is_some()
            || self.settings_panel.capturing.is_some();
        let needs_gtk_pump = match &self.tray {
            Some(tray) => !tray.uses_background_forwarder(),
            None => false,
        };
        if animating || needs_gtk_pump {
            let period = if animating { 50 } else { 100 };
            ctx.request_repaint_after(std::time::Duration::from_millis(period));
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            self.draw_top_bar(ui);
            ui.separator();
            if self.show_settings {
                self.draw_settings(ui);
            } else {
                self.draw_recording_section(ui);
                self.draw_playback_section(ui);
            }
        });
    }
}
