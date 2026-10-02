//! Локализация. Переводы — обычный JSON (см. `../i18n/*.json`), вшитый в
//! бинарник на этапе компиляции (`include_str!`) — никакой работы с файлами
//! в рантайме, поэтому один exe/бинарник остаётся самодостаточным (важно для
//! инсталляторов из `dist`).
//!
//! ## Как добавить новый язык
//! 1. Скопировать `app/i18n/en.json` в `app/i18n/<code>.json` и перевести значения
//!    (ключи трогать не надо).
//! 2. Добавить один элемент в массив `LANGS` ниже.
//! 3. Пересобрать проект. Всё остальное (переключатель языка в UI, подстановка
//!    аргументов в `{0}`/`{1}`) работает автоматически — конкретные экраны
//!    (`app.rs`, `step_editor.rs`, `tray.rs`) ничего не знают про список языков.
//!
//! Если для текущего языка не хватает какого-то ключа — используется английский,
//! а если и там нет — виден сам ключ (заметно в UI, а не тихая пустая строка).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Ru,
    En,
}

struct LangDef {
    lang: Lang,
    code: &'static str,
    label: &'static str,
    json: &'static str,
}

// Первый элемент — язык по умолчанию.
static LANGS: &[LangDef] = &[
    LangDef {
        lang: Lang::Ru,
        code: "ru",
        label: "Русский",
        json: include_str!("../i18n/ru.json"),
    },
    LangDef {
        lang: Lang::En,
        code: "en",
        label: "English",
        json: include_str!("../i18n/en.json"),
    },
];

impl Lang {
    pub fn all() -> impl Iterator<Item = Lang> {
        LANGS.iter().map(|d| d.lang)
    }

    /// Код языка (`"ru"`, `"en"`, ...) — используется при сохранении выбранного
    /// языка в настройки.
    pub fn code(self) -> &'static str {
        LANGS
            .iter()
            .find(|d| d.lang == self)
            .map_or("?", |d| d.code)
    }

    /// Обратная операция к [`Lang::code`] — разбор значения, прочитанного из
    /// файла настроек. `None`, если код не распознан (например, настройки от
    /// более новой версии с языком, которого здесь ещё нет) — вызывающая
    /// сторона в этом случае просто оставляет язык по умолчанию.
    pub fn from_code(code: &str) -> Option<Lang> {
        LANGS.iter().find(|d| d.code == code).map(|d| d.lang)
    }

    /// Имя языка для показа в UI — специально не переводится (человек должен
    /// узнать название своего языка независимо от того, какой язык сейчас выбран).
    pub fn label(self) -> &'static str {
        LANGS
            .iter()
            .find(|d| d.lang == self)
            .map_or("?", |d| d.label)
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0); // индекс в LANGS; 0 = язык по умолчанию

pub fn set_lang(lang: Lang) {
    if let Some(idx) = LANGS.iter().position(|d| d.lang == lang) {
        CURRENT.store(idx as u8, Ordering::Relaxed);
    }
}

pub fn current_lang() -> Lang {
    let idx = CURRENT.load(Ordering::Relaxed) as usize;
    LANGS.get(idx).map_or(Lang::Ru, |d| d.lang)
}

type Table = HashMap<String, String>;

static TABLES: LazyLock<Vec<Table>> = LazyLock::new(|| {
    LANGS
        .iter()
        .map(|d| {
            serde_json::from_str(d.json)
                .unwrap_or_else(|err| panic!("app/i18n/{}.json: невалидный JSON: {err}", d.code))
        })
        .collect()
});

/// Перевод строки по ключу для текущего языка.
pub fn t(key: &str) -> String {
    let idx = CURRENT.load(Ordering::Relaxed) as usize;
    if let Some(v) = TABLES.get(idx).and_then(|table| table.get(key)) {
        return v.clone();
    }
    // запасной вариант — язык по умолчанию (индекс 0), затем сам ключ
    TABLES
        .first()
        .and_then(|t| t.get(key))
        .cloned()
        .unwrap_or_else(|| key.to_string())
}

/// То же самое, но с подстановкой позиционных плейсхолдеров `{0}`, `{1}`, ...
///
/// Замена строго однопроходная, слева направо, по исходному шаблону: то, что
/// пришло из аргументов, повторно не сканируется. Последовательные
/// `str::replace` этим свойством не обладают — имя макроса вида `{1}`
/// «раскрылось» бы значением соседнего аргумента (см. тесты ниже).
pub fn tf(key: &str, args: &[&str]) -> String {
    let template = t(key);
    let mut out = String::with_capacity(template.len());
    let mut rest = template.as_str();

    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];

        // Кандидат в плейсхолдеры: '{', непустая последовательность ASCII-цифр,
        // '}' — и число должно помещаться в usize. Итог — (номер, позиция '}').
        let end = rest.find('}').and_then(|close| {
            let body = &rest[1..close];
            if !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit()) {
                body.parse::<usize>().ok().map(|idx| (idx, close))
            } else {
                None
            }
        });

        match end {
            Some((idx, close)) => match args.get(idx) {
                Some(arg) => out.push_str(arg),
                // Номер валидный, но аргумента с таким номером нет —
                // копируем плейсхолдер как есть.
                None => out.push_str(&rest[..=close]),
            },
            // Не плейсхолдер ("{abc", "{}", гигантское число) либо '}' не
            // найден вовсе: '{' копируется как обычный текст.
            None => out.push('{'),
        }

        // Сдвиг окна: за '}' у распознанного плейсхолдера, иначе за '{' —
        // он уже скопирован в out как обычный символ.
        rest = match end {
            Some((_, close)) => &rest[close + 1..],
            None => &rest[1..],
        };
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_in_default_language_exists_in_all_others() {
        let default_keys: std::collections::HashSet<_> = TABLES[0].keys().collect();
        for (i, def) in LANGS.iter().enumerate() {
            let keys: std::collections::HashSet<_> = TABLES[i].keys().collect();
            let missing: Vec<_> = default_keys.difference(&keys).collect();
            assert!(
                missing.is_empty(),
                "в {}.json не хватает ключей: {missing:?}",
                def.code
            );
        }
    }

    #[test]
    fn tf_replaces_positional_placeholders() {
        set_lang(Lang::En);
        assert_eq!(tf("status.saved", &["test"]), "Saved: test");
        set_lang(Lang::Ru);
        assert_eq!(tf("status.saved", &["тест"]), "Сохранено: тест");
    }

    #[test]
    fn tf_does_not_expand_placeholders_inside_arguments() {
        // Шаблон "[{0}] ({1})": аргумент №0 содержит "{1}". Однопроходная
        // замена обязана вставить его как текст, не дав «съесть» слот №1.
        // Последовательные str::replace выдали бы здесь "[END] (END)".
        assert_eq!(
            tf("test.placeholder_template", &["{1}", "END"]),
            "[{1}] (END)"
        );
    }

    #[test]
    fn tf_keeps_unfilled_and_out_of_range_placeholders() {
        assert_eq!(tf("test.placeholder_template", &[]), "[{0}] ({1})");
        assert_eq!(tf("test.placeholder_template", &["A"]), "[A] ({1})");
        // Лишние аргументы молча игнорируются.
        assert_eq!(tf("test.placeholder_template", &["A", "B", "C"]), "[A] (B)");
    }

    #[test]
    fn tf_treats_non_placeholder_braces_as_text() {
        // Скобки внутри аргументов копируются дословно: сканируется только
        // шаблон, а не результат подстановки.
        assert_eq!(
            tf("test.placeholder_template", &["a{b}c", "x{{y}}z"]),
            "[a{b}c] (x{{y}}z)"
        );
    }

    #[test]
    fn unknown_key_returns_itself() {
        assert_eq!(t("this.key.does.not.exist"), "this.key.does.not.exist");
    }

    #[test]
    fn code_and_from_code_roundtrip() {
        for lang in Lang::all() {
            assert_eq!(Lang::from_code(lang.code()), Some(lang));
        }
        assert_eq!(Lang::from_code("this-code-does-not-exist"), None);
    }
}
