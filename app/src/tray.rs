//! Иконка в трее + её контекстное меню.
//!
//! Разделено на `imp` (реальная реализация, только под Cargo-фичей `tray`) и
//! `stub` (без фичи) — у обоих один и тот же публичный интерфейс
//! (`AppTray::new/poll/set_status`), поэтому `app.rs` вообще не знает, какая
//! из них активна: `AppTray::new()` и так уже может вернуть `None`, если трей
//! недоступен на этой системе — отсутствие фичи просто ещё одна причина того
//! же самого "недоступен", без единого `#[cfg(...)]` за пределами этого файла.
//!
//! Фича включена по умолчанию. Выключить (например, ради совсем лёгкой сборки
//! без GTK на Linux): `cargo build --no-default-features`.

#[cfg(feature = "tray")]
pub use imp::{AppTray, TrayCommand};
#[cfg(not(feature = "tray"))]
pub use stub::{AppTray, TrayCommand};

#[cfg(feature = "tray")]
mod imp {
    //! ## Важный нюанс Linux
    //! `tray-icon` на Linux собран поверх GTK/`libappindicator` и **сам не
    //! крутит GTK event loop** — это должно делать приложение. Наш основной
    //! цикл — это winit-цикл `eframe`, который ничего не знает про GTK,
    //! поэтому здесь мы сами вызываем `gtk::init()` один раз и прокачиваем
    //! очередь GTK-событий на каждый кадр (см. `poll`). Без этого меню трея на
    //! Linux физически не будет реагировать на клики. На Windows/macOS
    //! `tray-icon` работает через нативные API и такого шага не требует — весь
    //! код с `gtk::` ниже под `cfg(target_os = "linux")`.

    use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum TrayCommand {
        ShowWindow,
        PlayLastMacro,
        StopAll,
        Quit,
    }

    /// Единый маппинг «идентификатор пункта меню → команда». Используется из
    /// двух мест: `poll` (Linux) и `spawn_forwarder` (остальные ОС) — поэтому
    /// вынесен сюда, чтобы список пунктов не разъезжался.
    fn map_menu_id(
        id: &MenuId,
        show: &MenuId,
        play: &MenuId,
        stop: &MenuId,
        quit: &MenuId,
    ) -> Option<TrayCommand> {
        if *id == *show {
            Some(TrayCommand::ShowWindow)
        } else if *id == *play {
            Some(TrayCommand::PlayLastMacro)
        } else if *id == *stop {
            Some(TrayCommand::StopAll)
        } else if *id == *quit {
            Some(TrayCommand::Quit)
        } else {
            None
        }
    }

    pub struct AppTray {
        // Держим, чтобы иконка не удалилась при drop (TrayIcon::drop скрывает её).
        _tray_icon: TrayIcon,
        show_id: MenuId,
        play_id: MenuId,
        stop_id: MenuId,
        quit_id: MenuId,
    }

    impl AppTray {
        /// `None`, если трей недоступен на этой системе (нет GTK/D-Bus, нет
        /// панели задач и т.п.) — приложение в этом случае просто работает без
        /// трея, через обычное окно и хоткеи.
        pub fn new() -> Option<Self> {
            #[cfg(target_os = "linux")]
            if let Err(err) = gtk::init() {
                tracing::warn!(%err, "не удалось инициализировать GTK — трей недоступен");
                return None;
            }

            let menu = Menu::new();
            let show_item =
                MenuItem::with_id("show", crate::i18n::t("tray.show_window"), true, None);
            let play_item = MenuItem::with_id("play", crate::i18n::t("tray.play_last"), true, None);
            let stop_item = MenuItem::with_id("stop", crate::i18n::t("tray.stop_all"), true, None);
            let quit_item = MenuItem::with_id("quit", crate::i18n::t("tray.quit"), true, None);

            let appended = menu.append(&show_item).is_ok()
                && menu.append(&play_item).is_ok()
                && menu.append(&stop_item).is_ok()
                && menu.append(&quit_item).is_ok();
            if !appended {
                tracing::warn!("не удалось собрать меню трея");
                return None;
            }

            let tray_icon = match TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_icon(status_icon(false, false))
                .with_tooltip("Macro Recorder")
                .build()
            {
                Ok(t) => t,
                Err(err) => {
                    tracing::warn!(%err, "не удалось создать иконку в трее");
                    return None;
                }
            };

            Some(Self {
                _tray_icon: tray_icon,
                show_id: show_item.id().clone(),
                play_id: play_item.id().clone(),
                stop_id: stop_item.id().clone(),
                quit_id: quit_item.id().clone(),
            })
        }

        /// Вызывать раз в кадр из UI-цикла. На Linux прокачивает GTK-очередь и
        /// вычитывает клики меню — события доходят до канала только пока очередь
        /// качается, а качать её можно только отсюда, из GUI-потока. На остальных
        /// ОС клики обслуживает фоновый поток (`spawn_forwarder`), и чтение общего
        /// канала здесь отключено: иначе события делились бы между двумя
        /// читателями случайным образом.
        pub fn poll(&self) -> Vec<TrayCommand> {
            #[cfg(target_os = "linux")]
            {
                while gtk::events_pending() {
                    gtk::main_iteration();
                }

                let mut commands = Vec::new();
                while let Ok(event) = MenuEvent::receiver().try_recv() {
                    if let Some(command) = map_menu_id(
                        event.id(),
                        &self.show_id,
                        &self.play_id,
                        &self.stop_id,
                        &self.quit_id,
                    ) {
                        commands.push(command);
                    }
                }
                commands
            }

            #[cfg(not(target_os = "linux"))]
            {
                Vec::new()
            }
        }

        /// true, если клики по меню обслуживает фоновый поток (`spawn_forwarder`);
        /// false — если их обязан вычитывать `poll` в UI-цикле (Linux).
        pub fn uses_background_forwarder(&self) -> bool {
            !cfg!(target_os = "linux")
        }

        /// Спавнит вечный фоновый поток, переводящий клики по меню трея в
        /// `AppEvent::TrayCommand` (Windows/macOS; на Linux — no-op).
        pub fn spawn_forwarder(
            &self,
            ctx: egui::Context,
            tx: std::sync::mpsc::Sender<crate::events::AppEvent>,
        ) {
            if cfg!(target_os = "linux") {
                return;
            }
            let show_id = self.show_id.clone();
            let play_id = self.play_id.clone();
            let stop_id = self.stop_id.clone();
            let quit_id = self.quit_id.clone();
            std::thread::spawn(move || {
                while let Ok(event) = MenuEvent::receiver().recv() {
                    if let Some(command) =
                        map_menu_id(event.id(), &show_id, &play_id, &stop_id, &quit_id)
                    {
                        if tx
                            .send(crate::events::AppEvent::TrayCommand(command))
                            .is_err()
                        {
                            break;
                        }
                        ctx.request_repaint();
                    }
                }
            });
        }

        /// Меняет цвет иконки под текущее состояние (серый — простой, красный —
        /// запись, зелёный — воспроизведение).
        pub fn set_status(&self, recording: bool, playing: bool) {
            if let Err(err) = self
                ._tray_icon
                .set_icon(Some(status_icon(recording, playing)))
            {
                tracing::warn!(%err, "не удалось обновить иконку трея");
            }
        }
    }

    /// Процедурная иконка-кружок 32×32 — без внешних файлов/ассетов, чтобы не
    /// тащить в проект бинарные ресурсы ради MVP. Цвет отражает состояние.
    fn status_icon(recording: bool, playing: bool) -> Icon {
        const SIZE: u32 = 32;
        let (r, g, b) = if recording {
            (220u8, 50, 47) // красный — идёт запись
        } else if playing {
            (38u8, 162, 105) // зелёный — идёт воспроизведение
        } else {
            (130u8, 130, 130) // серый — простой
        };

        let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
        let center = SIZE as f32 / 2.0;
        let radius = center - 2.0;
        for y in 0..SIZE {
            for x in 0..SIZE {
                let dx = x as f32 - center + 0.5;
                let dy = y as f32 - center + 0.5;
                if dx * dx + dy * dy <= radius * radius {
                    let idx = ((y * SIZE + x) * 4) as usize;
                    rgba[idx] = r;
                    rgba[idx + 1] = g;
                    rgba[idx + 2] = b;
                    rgba[idx + 3] = 255;
                }
            }
        }

        Icon::from_rgba(rgba, SIZE, SIZE).expect("иконка трея 32x32 RGBA всегда валидна")
    }
}

#[cfg(not(feature = "tray"))]
mod stub {
    /// Варианты здесь никогда не создаются (`poll()` ниже всегда пуст) — но
    /// тип должен существовать и совпадать по форме с `imp::TrayCommand`,
    /// иначе `app.rs` не смог бы ссылаться на него одинаково в обеих сборках.
    #[allow(dead_code)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum TrayCommand {
        ShowWindow,
        PlayLastMacro,
        StopAll,
        Quit,
    }

    /// Заглушка на случай сборки без фичи `tray`: ведёт себя точно как
    /// "трей недоступен на этой системе" — `app.rs` уже умеет с этим жить.
    pub struct AppTray;

    impl AppTray {
        pub fn new() -> Option<AppTray> {
            None
        }

        pub fn poll(&self) -> Vec<TrayCommand> {
            Vec::new()
        }

        pub fn set_status(&self, _recording: bool, _playing: bool) {}

        pub fn uses_background_forwarder(&self) -> bool {
            false
        }

        pub fn spawn_forwarder(
            &self,
            _ctx: egui::Context,
            _tx: std::sync::mpsc::Sender<crate::events::AppEvent>,
        ) {
        }
    }
}
