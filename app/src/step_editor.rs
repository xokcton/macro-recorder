//! Редактор шагов сохранённого макроса: правка задержек, drag&drop reorder,
//! удаление отдельных шагов, превью экрана у шагов с сохранённой миниатюрой.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use mrcore::{InputEvent, Key, LoopMode, Macro, MouseButton, ScreenThumbnail};

use crate::i18n;

pub struct StepEditorState {
    pub macro_: Macro,
    /// Текстуры превью, загруженные в GPU через `egui::Context::load_texture`.
    /// Ключ — хэш содержимого миниатюры (не индекс шага и не отпечаток
    /// экрана!): индексы меняются при drag&drop reorder, а отпечаток теперь
    /// необязателен — миниатюру можно сохранять и без него (см.
    /// `mrcore::ScreenCaptureConfig`), так что полагаться на его наличие
    /// нельзя. Содержимое миниатюры при этом стабильно для своего шага что
    /// при reorder, что при отсутствующем отпечатке. Каждую картинку грузим в
    /// GPU только один раз за всё время жизни редактора — делать это заново
    /// на каждый кадр было бы и медленно, и течью памяти.
    thumbnail_cache: HashMap<u64, egui::TextureHandle>,
}

impl StepEditorState {
    pub fn new(macro_: Macro) -> Self {
        Self {
            macro_,
            thumbnail_cache: HashMap::new(),
        }
    }
}

/// Хэш содержимого миниатюры — используется как ключ кэша текстур вместо
/// индекса шага или отпечатка экрана (см. `StepEditorState::thumbnail_cache`).
fn thumbnail_cache_key(thumbnail: &ScreenThumbnail) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    thumbnail.width.hash(&mut hasher);
    thumbnail.height.hash(&mut hasher);
    thumbnail.rgba.hash(&mut hasher);
    hasher.finish()
}

/// Возвращает текстуру превью из кэша, создавая её при первом обращении.
/// Свободная функция (не метод `&mut self`) специально: вызывается изнутри
/// цикла, который одновременно держит `&mut` на `state.macro_.steps` — методу
/// пришлось бы занимать весь `&mut StepEditorState`, а тут достаточно только
/// поля `thumbnail_cache` (раздельное заимствование полей одной структуры).
fn thumbnail_texture(
    cache: &mut HashMap<u64, egui::TextureHandle>,
    ctx: &egui::Context,
    thumbnail: &ScreenThumbnail,
) -> Option<egui::TextureHandle> {
    if thumbnail.width == 0 || thumbnail.height == 0 {
        return None;
    }
    let key = thumbnail_cache_key(thumbnail);
    // Сначала пробуем без клонирования содержимого миниатюры — клон нужен
    // только на первой загрузке текстуры (см. `insert` ниже), не на каждый
    // кадр, пока превью уже в кэше.
    if let Some(texture) = cache.get(&key) {
        return Some(texture.clone());
    }
    let image = egui::ColorImage::from_rgba_unmultiplied(
        [thumbnail.width as usize, thumbnail.height as usize],
        &thumbnail.rgba,
    );
    let texture = ctx.load_texture(
        format!("step-thumb-{key:x}"),
        image,
        egui::TextureOptions::NEAREST,
    );
    cache.insert(key, texture.clone());
    Some(texture)
}

pub enum StepEditorAction {
    Save(Macro),
    Cancel,
}

/// Рисует редактор в переданном `Ui`. Возвращает `Some`, если пользователь
/// нажал "Сохранить" или "Отмена" — вызывающая сторона должна закрыть редактор.
pub fn draw(ui: &mut egui::Ui, state: &mut StepEditorState) -> Option<StepEditorAction> {
    let mut action = None;

    ui.horizontal(|ui| {
        ui.label(i18n::t("ui.name_label"));
        ui.text_edit_singleline(&mut state.macro_.name);
    });
    ui.label(i18n::tf(
        "ui.step_summary",
        &[
            &state.macro_.steps.len().to_string(),
            &format_duration(state.macro_.duration_ms()),
        ],
    ));
    ui.small(i18n::t("step_editor.drag_hint"));

    let mut remove: Option<usize> = None;
    // (откуда, куда) — куда - это индекс строки, над которой отпустили драг.
    let mut drag_from_to: Option<(usize, usize)> = None;

    let ctx = ui.ctx().clone();
    let StepEditorState {
        macro_,
        thumbnail_cache,
    } = state;

    // Повторение: сколько проходов делать за одно воспроизведение. Ряд стоит
    // над списком шагов намеренно — читается как «весь этот список повторить».
    ui.horizontal(|ui| {
        ui.label(i18n::t("step_editor.loop_label"));

        let mode = macro_.loop_mode;
        if ui
            .selectable_label(
                matches!(mode, LoopMode::Once),
                i18n::t("step_editor.loop_once"),
            )
            .clicked()
        {
            macro_.loop_mode = LoopMode::Once;
        }
        if ui
            .selectable_label(
                matches!(mode, LoopMode::Times(_)),
                i18n::t("step_editor.loop_times"),
            )
            .clicked()
            && !matches!(mode, LoopMode::Times(_))
        {
            macro_.loop_mode = LoopMode::Times(3);
        }
        if let LoopMode::Times(n) = &mut macro_.loop_mode {
            ui.add(egui::DragValue::new(n).suffix("x").range(2..=9_999));
        }
        if ui
            .selectable_label(
                matches!(mode, LoopMode::Infinite),
                i18n::t("step_editor.loop_infinite"),
            )
            .clicked()
        {
            macro_.loop_mode = LoopMode::Infinite;
        }
    });

    egui::ScrollArea::vertical()
        .max_height(280.0)
        .show(ui, |ui| {
            for (i, step) in macro_.steps.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    // Превью экрана — если для этого шага сохранена миниатюра
                    // (обычно только у кликов; отпечаток для сверки при
                    // воспроизведении при этом мог и не сохраняться — это
                    // независимая настройка, см. `mrcore::ScreenCaptureConfig`).
                    if let Some(thumb) = step.screen_thumbnail.as_ref() {
                        if let Some(texture) = thumbnail_texture(thumbnail_cache, &ctx, thumb) {
                            ui.add(
                                egui::Image::new((texture.id(), texture.size_vec2()))
                                    .fit_to_exact_size(egui::vec2(28.0, 28.0)),
                            )
                            .on_hover_text(i18n::t("step_editor.screenshot_hover"));
                        }
                    }

                    // Драг-источник и зона сброса — только текстовая часть строки
                    // (не весь ряд): DragValue правее сам реагирует на
                    // перетаскивание мышью для изменения числа, и если бы вся
                    // строка была источником drag&drop, эти два жеста мешали бы
                    // друг другу.
                    let row_id = egui::Id::new("step_editor_row").with(i);
                    let drag_response = ui
                        .dnd_drag_source(row_id, i, |ui| {
                            ui.label(format!("☰ {:>3}. {}", i + 1, describe_event(&step.event)));
                        })
                        .response;

                    ui.add(
                        egui::DragValue::new(&mut step.delay_ms)
                            .suffix(" ms")
                            .speed(5)
                            .range(0..=600_000),
                    );
                    if ui.small_button("🗑").clicked() {
                        remove = Some(i);
                    }

                    if let Some(source_i) = drag_response.dnd_release_payload::<usize>() {
                        drag_from_to = Some((*source_i, i));
                    }
                });
            }
        });

    // Применяем максимум одно структурное изменение за кадр — так и проще
    // рассуждать о корректности индексов, и незаметно для пользователя (клики
    // редко приходят чаще, чем кадры).
    if let Some((from, to)) = drag_from_to {
        move_item(&mut macro_.steps, from, to);
    } else if let Some(i) = remove {
        macro_.steps.remove(i);
    }

    ui.separator();
    ui.horizontal(|ui| {
        if ui.button(i18n::t("ui.save_changes")).clicked() {
            action = Some(StepEditorAction::Save(macro_.clone()));
        }
        if ui.button(i18n::t("ui.cancel")).clicked() {
            action = Some(StepEditorAction::Cancel);
        }
    });

    action
}

/// Перемещает элемент с позиции `from` так, чтобы он оказался ровно на
/// позиции `to` (считая уже после удаления — соседние элементы просто
/// сдвигаются, освобождая место).
fn move_item<T>(items: &mut Vec<T>, from: usize, to: usize) {
    if from == to || from >= items.len() || to >= items.len() {
        return;
    }
    let item = items.remove(from);
    items.insert(to, item);
}

fn describe_event(event: &InputEvent) -> String {
    match event {
        InputEvent::MouseMove { x, y } => i18n::tf(
            "event.mouse_move",
            &[&format!("{x:.0}"), &format!("{y:.0}")],
        ),
        InputEvent::MouseWheel { delta_x, delta_y } => {
            i18n::tf("event.wheel", &[&delta_x.to_string(), &delta_y.to_string()])
        }
        InputEvent::MouseButton { button, pressed } => {
            let state = if *pressed {
                i18n::t("event.pressed")
            } else {
                i18n::t("event.released")
            };
            format!("{} {}", describe_button(*button), state)
        }
        InputEvent::Key { key, pressed } => {
            let state = if *pressed {
                i18n::t("event.pressed")
            } else {
                i18n::t("event.released")
            };
            i18n::tf("event.key", &[&describe_key(*key), &state])
        }
    }
}

fn describe_button(button: MouseButton) -> String {
    match button {
        MouseButton::Left => i18n::t("button.left"),
        MouseButton::Right => i18n::t("button.right"),
        MouseButton::Middle => i18n::t("button.middle"),
        // Оставляем сырой код — на большинстве систем 8/9 это "назад"/"вперёд",
        // но полагаться на это как на универсальную истину не стоит (см. README).
        MouseButton::Unknown(code) => i18n::tf("button.extra", &[&code.to_string()]),
    }
}

/// Debug-имена варианта `Key` (`KeyA`, `Return`, `F5`, ...) уже достаточно
/// читаемы сами по себе и одинаковы независимо от языка интерфейса — отдельная
/// таблица переводов ради этого списка не стоит своей сложности на этом этапе.
fn describe_key(key: Key) -> String {
    format!("{key:?}")
}

/// Длительность для показа человеку: `125000` мс → `«2:05»`,
/// `3725000` мс → `«1:02:05»`. Секунды округляются вниз: макрос
/// «2:04.9» читается как 2:04 — для ориентировки этого достаточно.
pub fn format_duration(ms: u64) -> String {
    let total_seconds = ms / 1000;
    let h = total_seconds / 3600;
    let m = (total_seconds % 3600) / 60;
    let s = total_seconds % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_item_forward_lands_at_target_index() {
        let mut v = vec!["A", "B", "C", "D", "E"];
        move_item(&mut v, 0, 3); // A -> на позицию D
        assert_eq!(v, vec!["B", "C", "D", "A", "E"]);
    }

    #[test]
    fn move_item_backward_lands_at_target_index() {
        let mut v = vec!["A", "B", "C", "D", "E"];
        move_item(&mut v, 3, 0); // D -> на позицию A
        assert_eq!(v, vec!["D", "A", "B", "C", "E"]);
    }

    #[test]
    fn move_item_to_itself_is_noop() {
        let mut v = vec!["A", "B", "C"];
        move_item(&mut v, 1, 1);
        assert_eq!(v, vec!["A", "B", "C"]);
    }

    #[test]
    fn move_item_adjacent_swap() {
        let mut v = vec!["A", "B", "C"];
        move_item(&mut v, 0, 1);
        assert_eq!(v, vec!["B", "A", "C"]);
    }

    #[test]
    fn move_item_out_of_bounds_is_ignored() {
        let mut v = vec!["A", "B", "C"];
        move_item(&mut v, 0, 10);
        assert_eq!(
            v,
            vec!["A", "B", "C"],
            "индекс за пределами — ничего не меняем"
        );
    }

    /// Ключевая проверка для задачи "миниатюра без отпечатка": кэш текстур
    /// больше не завязан на `ScreenFingerprint`, только на содержимое самой
    /// миниатюры — иначе шаг с миниатюрой, но без отпечатка (см.
    /// `mrcore::ScreenCaptureConfig`), никогда не находил бы себе ключ в кэше.
    #[test]
    fn thumbnail_cache_key_is_stable_for_identical_content() {
        let a = ScreenThumbnail {
            width: 4,
            height: 4,
            rgba: vec![1u8; 4 * 4 * 4],
        };
        let b = a.clone();
        assert_eq!(thumbnail_cache_key(&a), thumbnail_cache_key(&b));
    }

    #[test]
    fn thumbnail_cache_key_differs_for_different_content() {
        let a = ScreenThumbnail {
            width: 4,
            height: 4,
            rgba: vec![1u8; 4 * 4 * 4],
        };
        let b = ScreenThumbnail {
            width: 4,
            height: 4,
            rgba: vec![2u8; 4 * 4 * 4],
        };
        assert_ne!(thumbnail_cache_key(&a), thumbnail_cache_key(&b));
    }

    #[test]
    fn duration_formats_as_m_ss_and_h_mm_ss() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(125_000), "2:05");
        assert_eq!(format_duration(3_725_000), "1:02:05");
    }
}
