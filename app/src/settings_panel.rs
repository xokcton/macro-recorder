//! Панель настроек: язык интерфейса, перепривязка хоткеев (включая разрешение
//! хоткеев без модификатора — `Settings::allow_bare_hotkeys`) и экран-сторож
//! (`Settings::screen_capture`, см. `mrcore::ScreenCaptureConfig`).
//!
//! Захват новой комбинации — процесс на несколько кадров (ждём, пока
//! пользователь нажмёт что-то подходящее), поэтому состояние (`capturing`)
//! живёт в `App`, а не тут — эта функция только рисует и сообщает наружу, что
//! произошло за кадр.

use mrcore::{HotkeyAction, InputBackendChoice, ScreenCaptureConfig};

use crate::hotkeys::{AppHotkeys, RebindError};
use crate::i18n::{self, Lang};

#[derive(Default)]
pub struct SettingsPanelState {
    pub capturing: Option<HotkeyAction>,
    pub last_error: Option<String>,
}

pub enum SettingsPanelEvent {
    Language,
    Hotkey,
    /// Чекбокс "разрешить хоткей без модификатора" изменился — самого
    /// поведения хоткеев это не трогает (только будущий захват комбинаций),
    /// так что для этого события достаточно просто сохранить настройки.
    AllowBareHotkeys,
    /// Один из чекбоксов экран-сторожа (`fingerprint`/`thumbnail`) изменился —
    /// в отличие от `AllowBareHotkeys`, здесь помимо сохранения нужно ещё и
    /// перенастроить уже живые `Recorder`/`Player` (см.
    /// `app::MacroRecorderApp::apply_screen_capture_settings`).
    ScreenCapture,
    /// Чекбокс сжатия авто-повтора клавиш изменился — как `AllowBareHotkeys`,
    /// требует сохранения настроек, плюс применения к живому `Recorder`
    /// (см. обработку в `app.rs`).
    CompressKeyRepeats,
    /// Выбран другой бэкенд ввода — сохраняем настройки; применение после
    /// перезапуска (слушатель записи живёт весь процесс).
    BackendChoice,
}

const ACTIONS: [HotkeyAction; 4] = [
    HotkeyAction::ToggleRecording,
    HotkeyAction::TogglePause,
    HotkeyAction::TogglePlayback,
    HotkeyAction::StopAll,
];

pub fn draw(
    ui: &mut egui::Ui,
    state: &mut SettingsPanelState,
    hotkeys: &mut AppHotkeys,
    allow_bare_hotkeys: &mut bool,
    screen_capture: &mut ScreenCaptureConfig,
    compress_key_repeats: &mut bool,
    input_backend: &mut InputBackendChoice,
) -> Option<SettingsPanelEvent> {
    let mut event = None;

    ui.horizontal(|ui| {
        ui.label(i18n::t("ui.language_label"));
        let current = i18n::current_lang();
        for lang in Lang::all() {
            if ui.selectable_label(lang == current, lang.label()).clicked() {
                i18n::set_lang(lang);
                event = Some(SettingsPanelEvent::Language);
            }
        }
    });

    ui.separator();
    ui.heading(i18n::t("settings.backend_heading"));

    let mut backend_changed = false;
    ui.horizontal(|ui| {
        if ui
            .selectable_label(
                *input_backend == InputBackendChoice::Rdevin,
                i18n::t("settings.backend_rdevin"),
            )
            .clicked()
        {
            *input_backend = InputBackendChoice::Rdevin;
            backend_changed = true;
        }
        if ui
            .selectable_label(
                *input_backend == InputBackendChoice::Evdev,
                i18n::t("settings.backend_evdev"),
            )
            .clicked()
        {
            *input_backend = InputBackendChoice::Evdev;
            backend_changed = true;
        }
    });
    ui.small(i18n::t("settings.backend_restart_hint"));
    if crate::input_backend::is_wayland_session() && *input_backend == InputBackendChoice::Rdevin {
        ui.colored_label(
            egui::Color32::from_rgb(200, 120, 0),
            i18n::t("settings.backend_wayland_hint"),
        );
    }
    if !cfg!(feature = "evdev") && *input_backend == InputBackendChoice::Evdev {
        ui.colored_label(
            egui::Color32::from_rgb(210, 70, 70),
            i18n::t("settings.backend_evdev_unavailable"),
        );
    }
    if backend_changed {
        event = Some(SettingsPanelEvent::BackendChoice);
    }

    ui.separator();
    ui.heading(i18n::t("settings.hotkeys_heading"));

    if !hotkeys.is_available() {
        ui.colored_label(
            egui::Color32::from_rgb(200, 120, 0),
            i18n::t("settings.hotkeys_unavailable"),
        );
        #[cfg(all(target_os = "linux", feature = "portal-hotkeys"))]
        if crate::input_backend::is_wayland_session() {
            ui.colored_label(
                egui::Color32::from_rgb(150, 150, 150),
                i18n::t("settings.hotkeys_portal_note"),
            );
        }
        return event;
    }

    if let Some(err) = &state.last_error {
        ui.colored_label(egui::Color32::from_rgb(210, 70, 70), err.clone());
    }

    if ui
        .checkbox(allow_bare_hotkeys, i18n::t("settings.allow_bare_hotkeys"))
        .changed()
    {
        event = Some(SettingsPanelEvent::AllowBareHotkeys);
    }
    ui.small(i18n::t("settings.allow_bare_hotkeys_warning"));

    for action in ACTIONS {
        ui.horizontal(|ui| {
            ui.label(action_label(action));

            if state.capturing == Some(action) {
                ui.colored_label(
                    egui::Color32::from_rgb(90, 165, 220),
                    i18n::t("settings.press_combo"),
                );
                if ui.small_button(i18n::t("ui.cancel")).clicked() {
                    state.capturing = None;
                }
                match capture_combo(ui.ctx(), *allow_bare_hotkeys) {
                    Some(Captured::Cancel) => state.capturing = None,
                    Some(Captured::Combo(new_combo)) => match hotkeys.rebind(action, &new_combo) {
                        Ok(()) => {
                            state.capturing = None;
                            state.last_error = None;
                            event = Some(SettingsPanelEvent::Hotkey);
                        }
                        Err(err) => {
                            state.capturing = None;
                            state.last_error = Some(format_rebind_error(err));
                        }
                    },
                    None => {}
                }
            } else {
                let combo = hotkeys.combo_for(action).unwrap_or("—").to_string();
                ui.monospace(combo);
                if ui.small_button(i18n::t("settings.change")).clicked() {
                    state.capturing = Some(action);
                    state.last_error = None;
                }
            }
        });
    }

    ui.separator();
    ui.heading(i18n::t("settings.recording_heading"));

    if ui
        .checkbox(
            compress_key_repeats,
            i18n::t("settings.compress_key_repeats"),
        )
        .changed()
    {
        event = Some(SettingsPanelEvent::CompressKeyRepeats);
    }
    ui.small(i18n::t("settings.compress_key_repeats_hint"));

    ui.separator();
    ui.heading(i18n::t("settings.screen_guard_heading"));

    if cfg!(feature = "screen-guard") {
        let mut capture_changed = false;
        capture_changed |= ui
            .checkbox(
                &mut screen_capture.fingerprint,
                i18n::t("settings.screen_guard_enabled"),
            )
            .changed();
        ui.small(i18n::t("settings.screen_guard_enabled_hint"));

        capture_changed |= ui
            .checkbox(
                &mut screen_capture.thumbnail,
                i18n::t("settings.save_thumbnails"),
            )
            .changed();
        ui.small(i18n::t("settings.save_thumbnails_hint"));

        if capture_changed {
            event = Some(SettingsPanelEvent::ScreenCapture);
        }
    } else {
        ui.colored_label(
            egui::Color32::from_rgb(150, 150, 150),
            i18n::t("settings.screen_guard_unavailable"),
        );
    }

    event
}

pub(crate) fn action_label(action: HotkeyAction) -> String {
    match action {
        HotkeyAction::ToggleRecording => i18n::t("settings.action.toggle_recording"),
        HotkeyAction::TogglePause => i18n::t("settings.action.toggle_pause"),
        HotkeyAction::TogglePlayback => i18n::t("settings.action.toggle_playback"),
        HotkeyAction::StopAll => i18n::t("settings.action.stop_all"),
    }
}

fn format_rebind_error(err: RebindError) -> String {
    match err {
        RebindError::Unavailable => i18n::t("settings.error.unavailable"),
        RebindError::ParseFailed => i18n::t("settings.error.parse_failed"),
        RebindError::Conflict(action) => {
            i18n::tf("settings.error.conflict", &[&action_label(action)])
        }
        RebindError::OsRejected(reason) => i18n::tf("settings.error.os_rejected", &[&reason]),
    }
}

enum Captured {
    Combo(String),
    Cancel,
}

/// Ловит следующую "валидную" для глобального хоткея комбинацию из ввода
/// текущего кадра. По умолчанию требует хотя бы один модификатор + одну
/// обычную клавишу; если `allow_bare` (см. `Settings::allow_bare_hotkeys`)
/// включён, модификатор больше не обязателен. `Escape` без модификаторов —
/// ВСЕГДА сигнал отмены, а не попытка стать частью комбинации, даже при
/// `allow_bare` — иначе, разреши мы захватить голый `Escape`, отменить сам
/// процесс захвата было бы нечем (соседняя кнопка "Отмена" по-прежнему
/// доступна, так что это не потеря функциональности, а разумная граница).
fn capture_combo(ctx: &egui::Context, allow_bare: bool) -> Option<Captured> {
    ctx.input(|i| {
        if i.key_pressed(egui::Key::Escape) && i.modifiers.is_none() {
            return Some(Captured::Cancel);
        }
        for key in egui::Key::ALL.iter().copied() {
            if !i.key_pressed(key) {
                continue;
            }
            let Some(token) = key_token(key) else {
                continue;
            };
            let mods = i.modifiers;
            let has_modifier = mods.ctrl || mods.alt || mods.shift || mods.mac_cmd;
            if !has_modifier && !allow_bare {
                continue; // без модификатора не считаем валидным глобальным хоткеем
            }
            let mut parts = Vec::new();
            if mods.ctrl {
                parts.push("Ctrl");
            }
            if mods.alt {
                parts.push("Alt");
            }
            if mods.shift {
                parts.push("Shift");
            }
            if mods.mac_cmd {
                parts.push("Cmd");
            }
            parts.push(token);
            return Some(Captured::Combo(parts.join("+")));
        }
        None
    })
}

/// Переводит `egui::Key` в токен, который понимает парсер `global-hotkey`.
/// Не исчерпывающе — редкая пунктуация для глобальных хоткеев не нужна, а
/// непокрытая клавиша просто не даст захватить комбинацию (можно попробовать
/// другую), а не сломает что-то.
fn key_token(key: egui::Key) -> Option<&'static str> {
    use egui::Key::*;
    Some(match key {
        A => "A",
        B => "B",
        C => "C",
        D => "D",
        E => "E",
        F => "F",
        G => "G",
        H => "H",
        I => "I",
        J => "J",
        K => "K",
        L => "L",
        M => "M",
        N => "N",
        O => "O",
        P => "P",
        Q => "Q",
        R => "R",
        S => "S",
        T => "T",
        U => "U",
        V => "V",
        W => "W",
        X => "X",
        Y => "Y",
        Z => "Z",
        Num0 => "0",
        Num1 => "1",
        Num2 => "2",
        Num3 => "3",
        Num4 => "4",
        Num5 => "5",
        Num6 => "6",
        Num7 => "7",
        Num8 => "8",
        Num9 => "9",
        F1 => "F1",
        F2 => "F2",
        F3 => "F3",
        F4 => "F4",
        F5 => "F5",
        F6 => "F6",
        F7 => "F7",
        F8 => "F8",
        F9 => "F9",
        F10 => "F10",
        F11 => "F11",
        F12 => "F12",
        ArrowUp => "UP",
        ArrowDown => "DOWN",
        ArrowLeft => "LEFT",
        ArrowRight => "RIGHT",
        Enter => "ENTER",
        Space => "SPACE",
        Tab => "TAB",
        Backspace => "BACKSPACE",
        Delete => "DELETE",
        Home => "HOME",
        End => "END",
        PageUp => "PAGEUP",
        PageDown => "PAGEDOWN",
        Insert => "INSERT",
        _ => return None,
    })
}
