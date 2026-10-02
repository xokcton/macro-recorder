//! Глобальные хоткеи на чистом Wayland через xdg-desktop-portal
//! (интерфейс `org.freedesktop.portal.GlobalShortcuts`, крейт `ashpd`).
//!
//! `global-hotkey` на Linux работает поверх X11, поэтому на чистом Wayland
//! не получает ни одного нажатия. Портал — официальный обходной путь:
//! приложение регистрирует ярлыки, комбинации ловит сам композитор и шлёт
//! нам события `Activated`. Что важно знать:
//!
//! - нужен рабочий `xdg-desktop-portal` и поддержка интерфейса композитором
//!   (GNOME ≥ 48, KDE Plasma ≥ 6, свежие wlroots-оболочки); иначе привязка
//!   упадёт — мы пишем предупреждение в лог и продолжаем работать кнопками;
//! - при первой привязке композитор показывает пользователю диалог согласия;
//! - предложенные нами комбинации — лишь `preferred_trigger`: пользователь
//!   может переназначить их в системных настройках, идентификаторы ярлыков
//!   от этого не меняются.
//!
//! Поток запускается ТОЛЬКО когда сессия похожа на Wayland И обычные хоткеи
//! недоступны (условие в `app.rs`) — иначе одно действие срабатывало бы
//! дважды: портал + X11-хук.

use std::sync::mpsc;

use egui::Context;
use futures_lite::StreamExt;
use mrcore::{HotkeyAction, HotkeyBinding};

use crate::events::AppEvent;
use crate::settings_panel::action_label;

/// Стабильные строковые идентификаторы ярлыков: портал присылает их обратно
/// в каждом событии — по ним восстанавливаем действие. Переименовывать нельзя
/// без миграции настроек композитора у пользователей. Используется только
/// обратным поиском в `id_action` (и тестом ниже) — прямое направление
/// (`action_id`) сознательно продублировано `match`'ем, см. комментарий там.
const ACTION_IDS: [(HotkeyAction, &str); 4] = [
    (HotkeyAction::ToggleRecording, "toggle_recording"),
    (HotkeyAction::TogglePause, "toggle_pause"),
    (HotkeyAction::TogglePlayback, "toggle_playback"),
    (HotkeyAction::StopAll, "stop_all"),
];

// Раньше здесь был линейный поиск по ACTION_IDS с `.expect("все HotkeyAction
// покрыты ACTION_IDS")` — компилятор никак не проверял, что это утверждение
// вообще правда: добавь пятый вариант HotkeyAction и забудь дописать строку в
// массив — код спокойно соберётся, пройдёт весь cargo test и упадёт только в
// рантайме на реальном Wayland, в потоке portal-hotkeys, при попытке
// зарегистрировать именно этот хоткей. `match` без `_ =>` заставляет
// компилятор отказаться собираться, если варианта не хватает, — та же
// гарантия, но проверяемая на этапе сборки. `action_ids_roundtrip_through_
// portal_strings` ниже тестирует содержимое ACTION_IDS, а не то, что оно
// покрывает весь HotkeyAction, так что сам по себе тест эту дыру не ловит.
fn action_id(action: HotkeyAction) -> &'static str {
    match action {
        HotkeyAction::ToggleRecording => "toggle_recording",
        HotkeyAction::TogglePause => "toggle_pause",
        HotkeyAction::TogglePlayback => "toggle_playback",
        HotkeyAction::StopAll => "stop_all",
    }
}

fn id_action(id: &str) -> Option<HotkeyAction> {
    ACTION_IDS
        .iter()
        .find(|(_, known)| *known == id)
        .map(|(action, _)| *action)
}

/// Спавнит вечный поток портальных хоткеев. Вызывать один раз за жизнь
/// приложения (в первом кадре, рядом со `spawn_hotkey_forwarder`).
pub fn spawn(ctx: Context, bindings: Vec<HotkeyBinding>, tx: mpsc::Sender<AppEvent>) {
    std::thread::Builder::new()
        .name("macro-recorder-portal-hotkeys".into())
        .spawn(move || {
            if let Err(err) = futures_lite::future::block_on(run(&ctx, &bindings, &tx)) {
                tracing::warn!(
                    %err,
                    "портал GlobalShortcuts недоступен — глобальные хоткеи \
                     на этой системе работать не будут"
                );
            }
        })
        .expect("не удалось запустить поток portal-hotkeys");
}

async fn run(
    ctx: &Context,
    bindings: &[HotkeyBinding],
    tx: &mpsc::Sender<AppEvent>,
) -> Result<(), ashpd::Error> {
    use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut};

    let proxy = GlobalShortcuts::new().await?;
    let session = proxy
        .create_session(ashpd::desktop::CreateSessionOptions::default())
        .await?;

    let shortcuts: Vec<NewShortcut> = bindings
        .iter()
        .map(|binding| {
            NewShortcut::new(action_id(binding.action), action_label(binding.action))
                .preferred_trigger(portal_trigger(&binding.combo).as_str())
        })
        .collect();

    let bound = proxy
        .bind_shortcuts(&session, &shortcuts, None, BindShortcutsOptions::default())
        .await?
        .response()?;
    for shortcut in bound.shortcuts() {
        tracing::info!(
            id = shortcut.id(),
            trigger = shortcut.trigger_description(),
            "портальный хоткей привязан"
        );
    }

    let stream = proxy.receive_activated().await?;
    futures_lite::pin!(stream);
    while let Some(activated) = stream.next().await {
        let Some(action) = id_action(activated.shortcut_id()) else {
            continue;
        };
        if tx.send(AppEvent::PortalHotkey(action)).is_err() {
            break; // приложение завершается
        }
        ctx.request_repaint();
    }
    Ok(())
}

/// Наша запись комбинации (`"Ctrl+Alt+R"`; токены те же, что выдаёт
/// `settings_panel::capture_combo`) → акселератор в формате GTK/XDG
/// (`"<Control><Alt>R"`), который ждёт портал.
fn portal_trigger(combo: &str) -> String {
    let mut prefix = String::new();
    let mut key = String::new();
    for part in combo.split('+') {
        match part {
            "Ctrl" => prefix.push_str("<Control>"),
            "Alt" => prefix.push_str("<Alt>"),
            "Shift" => prefix.push_str("<Shift>"),
            "Cmd" => prefix.push_str("<Super>"),
            token => key = key_name(token).to_string(),
        }
    }
    format!("{prefix}{key}")
}

/// Токены нашего захвата → имена клавиш GDK. Буквы, цифры и F1–F12 совпадают
/// как есть; служебные клавиши в GDK называются иначе.
fn key_name(token: &str) -> &str {
    match token {
        "UP" => "Up",
        "DOWN" => "Down",
        "LEFT" => "Left",
        "RIGHT" => "Right",
        "ENTER" => "Return",
        "SPACE" => "space",
        "TAB" => "Tab",
        "BACKSPACE" => "BackSpace",
        "DELETE" => "Delete",
        "HOME" => "Home",
        "END" => "End",
        "PAGEUP" => "Page_Up",
        "PAGEDOWN" => "Page_Down",
        "INSERT" => "Insert",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_trigger_maps_mods_and_special_keys() {
        assert_eq!(portal_trigger("Ctrl+Alt+R"), "<Control><Alt>R");
        assert_eq!(portal_trigger("Shift+ENTER"), "<Shift>Return");
        assert_eq!(portal_trigger("Cmd+SPACE"), "<Super>space");
        assert_eq!(portal_trigger("F5"), "F5");
        assert_eq!(portal_trigger("Ctrl+PAGEUP"), "<Control>Page_Up");
    }

    #[test]
    fn action_ids_roundtrip_through_portal_strings() {
        for (action, _) in ACTION_IDS {
            assert_eq!(id_action(action_id(action)), Some(action));
        }
    }
}
