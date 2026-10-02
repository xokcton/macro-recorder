//! Мост «фоновые потоки → GUI-поток» для внешних событий.
//!
//! Системные каналы `global-hotkey` и `tray-icon` общие: кто первый вызвал
//! `recv()`/`try_recv()`, тот событие и получил. Поэтому события вычитывают
//! ровно в одном месте — здесь, в фоновых потоках (а на Linux меню трея
//! вычитывает UI-поток, см. комментарий в `tray.rs`), и пересылают в
//! собственный канал приложения, попутно будя GUI через
//! `Context::request_repaint()`. В итоге UI-цикл спит, пока событий нет,
//! и просыпается сразу по их приходу — вместо опроса каждые 100 мс.

use std::sync::mpsc;
use std::thread;

use egui::Context;
use global_hotkey::{GlobalHotKeyEvent, HotKeyState};

#[cfg(all(target_os = "linux", feature = "portal-hotkeys"))]
use mrcore::HotkeyAction;

#[cfg(feature = "tray")]
use crate::tray::TrayCommand;

/// Всё, что прилетает в приложение извне GUI-цикла.
#[derive(Debug)]
pub enum AppEvent {
    /// Нажатие глобального хоткея: сырой идентификатор события
    /// `global-hotkey`. В действие его превращает UI-поток через
    /// `AppHotkeys::action_for_id` — тогда перепривязка хоткея действует
    /// сразу, без синхронизации с фоновым потоком.
    HotkeyPressed(u32),
    /// Действие от портальных хоткеев (Wayland): портал присылает сразу
    /// строковый id ярлыка, назначенный нами при привязке, поэтому действие
    /// известно напрямую, без маппинга числовых идентификаторов. Существует
    /// только под фичей `portal-hotkeys` — единственный конструктор
    /// (`portal_hotkeys::spawn`) собирается вместе с ней.
    #[cfg(all(target_os = "linux", feature = "portal-hotkeys"))]
    PortalHotkey(HotkeyAction),
    /// Выбранный пункт меню трея (Windows/macOS; на Linux команды приходят
    /// через `AppTray::poll`, см. `tray.rs`). Существует только вместе с
    /// фичей `tray`: единственный, кто его конструирует, — фоновый
    /// пересыльщик из `AppTray::spawn_forwarder`, которого без фичи нет.
    #[cfg(feature = "tray")]
    TrayCommand(TrayCommand),
}

/// Спавнит вечный поток-пересыльщик нажатий глобальных хоткеев. Вызывать один
/// раз за время жизни приложения (в первом кадре, когда доступен `ctx`).
pub fn spawn_hotkey_forwarder(ctx: Context, tx: mpsc::Sender<AppEvent>) {
    thread::spawn(move || {
        // recv() блокирует до следующего события — нулевой расход CPU между
        // нажатиями. Канал статический и переживает всё приложение, поэтому
        // цикл кончается только вместе с процессом.
        while let Ok(event) = GlobalHotKeyEvent::receiver().recv() {
            if event.state() != HotKeyState::Pressed {
                continue; // реагируем на нажатие, не на отпускание
            }
            if tx.send(AppEvent::HotkeyPressed(event.id())).is_err() {
                break; // приложение завершается
            }
            ctx.request_repaint();
        }
    });
}
