//! "Сторож экрана": лёгкая, чисто арифметическая (никакого ИИ/нейросетей) защита
//! от того, что интерфейс уехал/поменялся между записью и воспроизведением.
//!
//! Идея: в момент клика при записи запоминаем маленький "отпечаток" области
//! экрана вокруг курсора — перцептивный хэш (average hash, aHash). Перед тем как
//! кликнуть при воспроизведении, считаем такой же отпечаток текущего экрана и
//! сравниваем. Если разница выше порога — область явно другая (сайт
//! перестроился, окно сдвинулось, всплыл баннер и т.п.), и вместо клика "в
//! случайное место" мы ставим воспроизведение на паузу и показываем предупреждение.
//!
//! Почему хэш, а не нейросеть: сравнение через Hamming distance (XOR + подсчёт
//! бит) занимает наносекунды и требует нулевых внешних весов/рантаймов — работает
//! одинаково быстро хоть на Raspberry Pi. Полноценная vision-модель дала бы более
//! "умную" оценку "похожести", но ценой десятков-сотен МБ веса и заметной нагрузки
//! на CPU — несоразмерная плата за задачу уровня "да/нет, тут всё то же самое?".
//!
//! Как и `backend`, этот модуль изолирует единственную точку, которая знает про
//! конкретную библиотеку захвата экрана (`xcap`, см. `xcap_guard`, доступен под
//! Cargo-фичей `screen-guard`) — `Recorder`/`Player` работают только через trait
//! [`ScreenGuard`].

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::CoreError;

#[cfg(feature = "screen-guard")]
pub mod xcap_guard;

#[cfg(test)]
pub mod mock_guard;

/// Сырой прямоугольный снимок области экрана в RGBA8 (4 байта на пиксель).
#[derive(Clone)]
pub struct RegionSnapshot {
    pub width: u32,
    pub height: u32,
    /// Длина ровно `width * height * 4`.
    pub rgba: Vec<u8>,
}

/// Источник снимков экрана. Единственная точка входа для `Recorder`/`Player` —
/// они не знают, чем именно снимок сделан (xcap, мок в тестах, что угодно ещё).
pub trait ScreenGuard: Send + Sync + 'static {
    /// Захватывает квадратную область `size`×`size` пикселей с центром в
    /// глобальных экранных координатах `(x, y)`.
    fn capture_region(&self, x: f64, y: f64, size: u32) -> Result<RegionSnapshot, CoreError>;
}

/// Что именно сохранять для клика, если `ScreenGuard` вообще доступен.
///
/// Раньше отпечаток и миниатюра всегда шли парой: если экран-сторож включён —
/// сохраняются оба. Но отпечаток нужен только `Player`, чтобы сверить экран
/// перед кликом при воспроизведении, а миниатюра — только человеку, для
/// превью в редакторе шагов (см. предупреждение о приватности у
/// [`ScreenThumbnail`] ниже). Раз они нужны разным потребителям и по разным
/// причинам, нет причины требовать их только вместе — независимые флаги
/// позволяют, например, держать защиту от "уехавшего" интерфейса включённой,
/// но не писать в файл макроса ни одного настоящего фрагмента экрана — или
/// наоборот, оставить только удобные превью, не включая автопаузу.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenCaptureConfig {
    /// Сохранять перцептивный хэш — то, с чем `Player` сверяет экран.
    pub fingerprint: bool,
    /// Сохранять уменьшенный фрагмент экрана для превью в редакторе шагов.
    pub thumbnail: bool,
}

impl ScreenCaptureConfig {
    /// Ничего не сохранять — поведение по умолчанию (см. `Default`), но и
    /// именованная константа удобнее на вызывающей стороне, чем `::default()`.
    pub const NONE: Self = Self {
        fingerprint: false,
        thumbnail: false,
    };

    /// Нужно ли вообще делать снимок экрана для этого клика — если оба флага
    /// выключены, `Recorder` не должен даже пытаться обращаться к `ScreenGuard`.
    pub fn wants_capture(self) -> bool {
        self.fingerprint || self.thumbnail
    }
}

/// Отпечаток области экрана: average hash (сетка 8×8 → 64 бита) плюс средний
/// цвет области.
///
/// Зачем цвет: у чистого aHash есть вырожденный случай — идеально однотонная
/// область даёт все 64 бита равными единице независимо от ЦВЕТА (каждая
/// ячейка равна своему среднему). Красная кнопка и синяя неразличимы. Средний
/// цвет стоит ноль при подсчёте и закрывает именно этот случай; для обычных
/// неоднотонных областей основную работу продолжает делать хэш, а цвет —
/// вторая линия защиты.
///
/// Сериализация ручная (см. `FingerprintRepr`): старые макросы хранят
/// отпечаток голым числом — они обязаны продолжать загружаться.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScreenFingerprint {
    bits: u64,
    /// Средний цвет области, RGB по каналу 0..=255. `None` — отпечаток из
    /// старого формата (без цвета): такие сравниваются только по битам.
    avg_rgb: Option<[u8; 3]>,
}

/// Формы хранения в JSON. `untagged` пробует варианты по порядку: число
/// читается как `Legacy` (старый формат), объект — как `Full` (текущий).
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum FingerprintRepr {
    Legacy(u64),
    Full { bits: u64, avg_rgb: Option<[u8; 3]> },
}

impl Serialize for ScreenFingerprint {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        FingerprintRepr::Full {
            bits: self.bits,
            avg_rgb: self.avg_rgb,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ScreenFingerprint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match FingerprintRepr::deserialize(deserializer)? {
            FingerprintRepr::Legacy(bits) => Ok(Self {
                bits,
                avg_rgb: None,
            }),
            FingerprintRepr::Full { bits, avg_rgb } => Ok(Self { bits, avg_rgb }),
        }
    }
}

const GRID: usize = 8;

impl ScreenFingerprint {
    pub fn from_snapshot(snapshot: &RegionSnapshot) -> Self {
        if snapshot.width == 0 || snapshot.height == 0 {
            return Self {
                bits: 0,
                avg_rgb: None,
            };
        }

        let mut gray = [0f32; GRID * GRID];
        // Средний цвет региона копим в этом же проходе, а не отдельным
        // вторым (как было раньше) — сетка ниже и так задевает каждый пиксель
        // области ровно один раз, второй проход по тем же данным был чистым
        // дублированием работы.
        let (mut sr, mut sg, mut sb, mut n) = (0u64, 0u64, 0u64, 0u64);

        for gy in 0..GRID as u32 {
            // Границы ячейки — целочисленным делением (floor), а не через
            // f32 * .ceil()/.min()/.max(x0+1): у той схемы соседние ячейки
            // получали на 1px общую границу всякий раз, когда width/height не
            // делится на GRID нацело — а именно так и выходит для снимка,
            // обрезанного у края монитора (см. `xcap_guard::capture_region`,
            // там `width`/`height` могут быть меньше `DEFAULT_REGION_SIZE` и
            // произвольными). Общий пиксель на границе учитывался в яркости
            // ДВАЖДЫ — сразу в обеих соседних ячейках, слегка искажая биты
            // хэша именно там, где начинается и заканчивается регион. Целые
            // границы `i * dim / GRID` разбивают [0, dim) на пиксель ровно
            // по одному разу, без исключений и без плавающей точки вообще.
            let y0 = (gy * snapshot.height) / GRID as u32;
            let y1 = ((gy + 1) * snapshot.height) / GRID as u32;
            for gx in 0..GRID as u32 {
                let x0 = (gx * snapshot.width) / GRID as u32;
                let x1 = ((gx + 1) * snapshot.width) / GRID as u32;

                let mut sum = 0f32;
                let mut count = 0u32;
                for y in y0..y1 {
                    for x in x0..x1 {
                        let idx = ((y * snapshot.width + x) * 4) as usize;
                        if idx + 2 >= snapshot.rgba.len() {
                            continue;
                        }
                        let r = snapshot.rgba[idx];
                        let g = snapshot.rgba[idx + 1];
                        let b = snapshot.rgba[idx + 2];
                        sum += 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32; // стандартные веса luma
                        count += 1;
                        sr += u64::from(r);
                        sg += u64::from(g);
                        sb += u64::from(b);
                        n += 1;
                    }
                }
                gray[(gy * GRID as u32 + gx) as usize] =
                    if count > 0 { sum / count as f32 } else { 0.0 };
            }
        }

        let mean: f32 = gray.iter().sum::<f32>() / gray.len() as f32;
        let mut bits: u64 = 0;
        for (i, &v) in gray.iter().enumerate() {
            if v >= mean {
                bits |= 1 << i;
            }
        }

        let avg_rgb = if n > 0 {
            // Деление гарантированно безопасно (n > 0), но
            // clippy::manual_checked_ops требует выражать этот факт самим
            // checked_div, поэтому fallback формально недостижим.
            Some([
                sr.checked_div(n).unwrap_or(0) as u8,
                sg.checked_div(n).unwrap_or(0) as u8,
                sb.checked_div(n).unwrap_or(0) as u8,
            ])
        } else {
            None
        };

        Self { bits, avg_rgb }
    }

    pub fn hamming_distance(&self, other: &Self) -> u32 {
        (self.bits ^ other.bits).count_ones()
    }

    /// Суммарное расхождение средних цветов по трём каналам (0..=765).
    /// Если у любой из сторон цвет неизвестен (старый формат), проверка
    /// неприменима и возвращается 0 — «цвет не возражает».
    pub fn avg_channel_diff(&self, other: &Self) -> u32 {
        match (self.avg_rgb, other.avg_rgb) {
            (Some(a), Some(b)) => (0..3).map(|i| u32::from(a[i].abs_diff(b[i]))).sum(),
            _ => 0,
        }
    }

    /// `true`, если расхождение не превышает `max_distance` бит из 64.
    pub fn is_similar(&self, other: &Self, max_distance: u32) -> bool {
        self.hamming_distance(other) <= max_distance
    }

    /// Полная проверка «тот же ли это экран»: структура (биты) И общий цвет
    /// в пределах порогов. Её использует `Player`; `is_similar` остаётся
    /// для проверки одной только структуры.
    pub fn matches(&self, other: &Self, max_hamming: u32, max_avg_diff: u32) -> bool {
        self.hamming_distance(other) <= max_hamming && self.avg_channel_diff(other) <= max_avg_diff
    }
}

/// Порог по умолчанию: до ~15% сетки может отличаться (антиалиасинг, курсор,
/// мигающий курсор ввода текста и т.п.) прежде чем считать область "другой".
pub const DEFAULT_MAX_HAMMING_DISTANCE: u32 = 10;

/// Порог по среднему цвету: суммарное расхождение трёх каналов RGB (0..=765).
/// ~90 пропускает лёгкую смену яркости/оттенка темы (~30 на канал), но
/// уверенно отсекает однотонные области принципиально разного цвета
/// (красное против синего даёт ~500+).
pub const DEFAULT_MAX_AVG_CHANNEL_DIFF: u32 = 90;

/// Сторона квадратной области вокруг клика, которую проверяем, в пикселях.
/// Маленькая область — намеренно: это на порядки дешевле, чем сверять весь
/// экран, и для вопроса "то ли это место или нет" этого достаточно.
pub const DEFAULT_REGION_SIZE: u32 = 64;

/// Сторона миниатюры для превью в редакторе шагов. Меньше `DEFAULT_REGION_SIZE`
/// специально: превью нужно только чтобы человек узнал "а, это вон та кнопка",
/// а не пиксель-в-пиксель точная копия — незачем раздувать файл макроса.
pub const DEFAULT_THUMBNAIL_SIZE: u32 = 32;

/// Уменьшенная копия [`RegionSnapshot`] для показа в UI (редактор шагов).
///
/// Хранится в макросе рядом с [`ScreenFingerprint`] — но, в отличие от
/// отпечатка, только для *человека*: движок воспроизведения её не читает и не
/// сравнивает, для сверки экрана достаточно хэша.
///
/// ⚠️ Приватность: это маленький (32×32 по умолчанию), но самый настоящий
/// фрагмент скриншота вокруг места клика — если сохранённым макросом делиться
/// с кем-то или синхронизировать в облако, вместе с ним уедут и эти миниатюры.
/// Это осознанный компромисс ради полезного превью; кто не хочет даже такого —
/// просто не включает фичу `screen-guard`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScreenThumbnail {
    pub width: u32,
    pub height: u32,
    /// RGBA8, `width * height * 4` байт. В JSON — компактной base64-строкой
    /// (см. `base64_bytes`), а не "в лоб" JSON-массивом чисел: для 32×32
    /// изображения это ~5.5KB текста вместо ~16KB.
    #[serde(with = "base64_bytes")]
    pub rgba: Vec<u8>,
}

impl ScreenThumbnail {
    /// Даунсемплит снимок до `DEFAULT_THUMBNAIL_SIZE`×`DEFAULT_THUMBNAIL_SIZE`
    /// простым усреднением по ячейкам (тот же приём, что и в `ScreenFingerprint`,
    /// только тут сохраняется цвет каждой ячейки целиком, а не один яркостный бит).
    pub fn from_snapshot(snapshot: &RegionSnapshot) -> Self {
        Self::from_snapshot_sized(snapshot, DEFAULT_THUMBNAIL_SIZE)
    }

    fn from_snapshot_sized(snapshot: &RegionSnapshot, target: u32) -> Self {
        if snapshot.width == 0 || snapshot.height == 0 || target == 0 {
            return Self {
                width: 0,
                height: 0,
                rgba: Vec::new(),
            };
        }

        let mut out = vec![0u8; (target * target * 4) as usize];

        for gy in 0..target {
            // Целые границы ячейки — та же схема и та же причина, что в
            // `ScreenFingerprint::from_snapshot` (см. комментарий там): f32
            // `.ceil()`/`.max(x0+1)` давал соседним ячейкам общий пиксель на
            // границе, если размер снимка не делится на `target` нацело —
            // а это как раз случай снимка, обрезанного у края монитора. Тут
            // цена ошибки ниже (едва заметный шов в превью для человека, а
            // не искажённый отпечаток для сравнения), но раз чиним схему в
            // одном месте — чиним и здесь, чтобы она не разъезжалась.
            let y0 = (gy * snapshot.height) / target;
            let y1 = ((gy + 1) * snapshot.height) / target;
            for gx in 0..target {
                let x0 = (gx * snapshot.width) / target;
                let x1 = ((gx + 1) * snapshot.width) / target;

                let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32);
                let mut count = 0u32;
                for y in y0..y1 {
                    for x in x0..x1 {
                        let idx = ((y * snapshot.width + x) * 4) as usize;
                        if idx + 3 >= snapshot.rgba.len() {
                            continue;
                        }
                        r += snapshot.rgba[idx] as u32;
                        g += snapshot.rgba[idx + 1] as u32;
                        b += snapshot.rgba[idx + 2] as u32;
                        a += snapshot.rgba[idx + 3] as u32;
                        count += 1;
                    }
                }

                let out_idx = ((gy * target + gx) * 4) as usize;
                if let (Some(r), Some(g), Some(b), Some(a)) = (
                    r.checked_div(count),
                    g.checked_div(count),
                    b.checked_div(count),
                    a.checked_div(count),
                ) {
                    out[out_idx] = r as u8;
                    out[out_idx + 1] = g as u8;
                    out[out_idx + 2] = b as u8;
                    out[out_idx + 3] = a as u8;
                }
            }
        }

        Self {
            width: target,
            height: target,
            rgba: out,
        }
    }
}

/// `serde(with = ...)` модуль: (де)сериализует `Vec<u8>` как base64-строку.
mod base64_bytes {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(deserializer)?;
        STANDARD.decode(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_snapshot(width: u32, height: u32, rgb: (u8, u8, u8)) -> RegionSnapshot {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            rgba.extend_from_slice(&[rgb.0, rgb.1, rgb.2, 255]);
        }
        RegionSnapshot {
            width,
            height,
            rgba,
        }
    }

    /// Левая половина светлая, правая — тёмная.
    fn split_snapshot(width: u32, height: u32) -> RegionSnapshot {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let v = if x < width / 2 { 240 } else { 10 };
                let _ = y;
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        RegionSnapshot {
            width,
            height,
            rgba,
        }
    }

    #[test]
    fn identical_snapshots_have_zero_distance() {
        let a = ScreenFingerprint::from_snapshot(&split_snapshot(64, 64));
        let b = ScreenFingerprint::from_snapshot(&split_snapshot(64, 64));
        assert_eq!(a.hamming_distance(&b), 0);
        assert!(a.is_similar(&b, 0));
    }

    #[test]
    fn solid_color_vs_split_is_clearly_different() {
        let solid = ScreenFingerprint::from_snapshot(&solid_snapshot(64, 64, (128, 128, 128)));
        let split = ScreenFingerprint::from_snapshot(&split_snapshot(64, 64));
        // Ровно половина сетки должна разойтись (светлая половина против тёмной
        // при среднем ровно посередине) — это структурно другое изображение.
        assert!(
            solid.hamming_distance(&split) >= 24,
            "distance = {}",
            solid.hamming_distance(&split)
        );
        assert!(!solid.is_similar(&split, DEFAULT_MAX_HAMMING_DISTANCE));
    }

    #[test]
    fn tiny_noise_stays_within_default_threshold() {
        let base = split_snapshot(64, 64);
        let mut noisy = split_snapshot(64, 64);
        // Портим горстку пикселей в углу — имитация курсора/мигающего каретки.
        for i in 0..8 {
            let idx = i * 4;
            noisy.rgba[idx] = 255 - noisy.rgba[idx];
        }

        let a = ScreenFingerprint::from_snapshot(&base);
        let b = ScreenFingerprint::from_snapshot(&noisy);
        assert!(
            a.is_similar(&b, DEFAULT_MAX_HAMMING_DISTANCE),
            "distance = {}",
            a.hamming_distance(&b)
        );
    }

    #[test]
    fn empty_snapshot_does_not_panic() {
        let empty = RegionSnapshot {
            width: 0,
            height: 0,
            rgba: Vec::new(),
        };
        let fp = ScreenFingerprint::from_snapshot(&empty);
        assert_eq!(fp.hamming_distance(&fp), 0);
    }

    #[test]
    fn non_divisible_region_size_does_not_double_count_pixels() {
        // 37×41 — специально НЕ делится на GRID(8) нацело ни по одной оси.
        // Именно такие размеры получаются у снимка, обрезанного краем
        // монитора (см. `xcap_guard::capture_region` — там `width`/`height`
        // могут быть произвольными и меньше `DEFAULT_REGION_SIZE`). Раньше
        // границы ячеек сетки считались через f32 `.ceil()`/`.max(x0+1)`, и
        // соседние ячейки получали общий пиксель на стыке — тот пиксель
        // учитывался в среднем ДВАЖДЫ. Проверяем это через средний цвет:
        // сравниваем с честным полным проходом по всем пикселям, посчитанным
        // независимо от сетки, — если сетка хоть один пиксель посчитает
        // дважды (или пропустит), суммы разойдутся.
        let width: u32 = 37;
        let height: u32 = 41;
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        let (mut sum_r, mut sum_g, mut sum_b) = (0u64, 0u64, 0u64);
        for y in 0..height {
            for x in 0..width {
                // Неоднотонный узор: у каждого пикселя своё значение, иначе
                // двойной учёт пикселя не изменил бы среднее вообще.
                let r = ((x * 7 + y * 3) % 256) as u8;
                let g = ((x * 5 + y * 11) % 256) as u8;
                let b = ((x * 13 + y) % 256) as u8;
                rgba.extend_from_slice(&[r, g, b, 255]);
                sum_r += u64::from(r);
                sum_g += u64::from(g);
                sum_b += u64::from(b);
            }
        }
        let n = u64::from(width) * u64::from(height);
        let expected_avg = [(sum_r / n) as u8, (sum_g / n) as u8, (sum_b / n) as u8];

        let snapshot = RegionSnapshot {
            width,
            height,
            rgba,
        };
        let fp = ScreenFingerprint::from_snapshot(&snapshot);
        assert_eq!(
            fp.avg_rgb,
            Some(expected_avg),
            "средний цвет региона 37×41 разошёлся с честным полным проходом — \
             похоже, сетка опять считает какой-то пиксель дважды или не считает вовсе"
        );
    }

    #[test]
    fn thumbnail_has_expected_dimensions() {
        let snap = solid_snapshot(64, 64, (10, 20, 30));
        let thumb = ScreenThumbnail::from_snapshot(&snap);
        assert_eq!(thumb.width, DEFAULT_THUMBNAIL_SIZE);
        assert_eq!(thumb.height, DEFAULT_THUMBNAIL_SIZE);
        assert_eq!(
            thumb.rgba.len(),
            (DEFAULT_THUMBNAIL_SIZE * DEFAULT_THUMBNAIL_SIZE * 4) as usize
        );
    }

    #[test]
    fn thumbnail_of_solid_color_preserves_that_color() {
        let snap = solid_snapshot(64, 64, (200, 100, 50));
        let thumb = ScreenThumbnail::from_snapshot(&snap);
        // Каждая ячейка усредняет один и тот же цвет — должна получиться она же.
        for chunk in thumb.rgba.chunks(4) {
            assert_eq!(chunk, &[200, 100, 50, 255]);
        }
    }

    #[test]
    fn thumbnail_downsamples_smaller_than_source() {
        let snap = solid_snapshot(64, 64, (1, 2, 3));
        let thumb = ScreenThumbnail::from_snapshot(&snap);
        assert!(thumb.width < snap.width);
        assert!((thumb.rgba.len() as u32) < (snap.rgba.len() as u32));
    }

    #[test]
    fn empty_snapshot_thumbnail_does_not_panic() {
        let empty = RegionSnapshot {
            width: 0,
            height: 0,
            rgba: Vec::new(),
        };
        let thumb = ScreenThumbnail::from_snapshot(&empty);
        assert_eq!(thumb.width, 0);
        assert!(thumb.rgba.is_empty());
    }

    #[test]
    fn capture_config_defaults_to_nothing() {
        assert_eq!(ScreenCaptureConfig::default(), ScreenCaptureConfig::NONE);
        assert!(!ScreenCaptureConfig::default().wants_capture());
    }

    #[test]
    fn capture_config_wants_capture_if_either_flag_is_set() {
        assert!(ScreenCaptureConfig {
            fingerprint: true,
            thumbnail: false,
        }
        .wants_capture());
        assert!(ScreenCaptureConfig {
            fingerprint: false,
            thumbnail: true,
        }
        .wants_capture());
        assert!(!ScreenCaptureConfig::NONE.wants_capture());
    }

    #[test]
    fn thumbnail_roundtrips_through_json_as_compact_base64() {
        let snap = split_snapshot(64, 64);
        let thumb = ScreenThumbnail::from_snapshot(&snap);

        let json = serde_json::to_string(&thumb).unwrap();
        // Строкой, не JSON-массивом чисел — иначе файл макроса раздувается в разы.
        assert!(
            json.contains("\"rgba\":\""),
            "ожидали base64-строку, получили: {json}"
        );

        let restored: ScreenThumbnail = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, thumb);
    }

    #[test]
    fn solid_areas_of_different_color_are_distinguished_by_average() {
        let red = ScreenFingerprint::from_snapshot(&solid_snapshot(64, 64, (220, 40, 40)));
        let blue = ScreenFingerprint::from_snapshot(&solid_snapshot(64, 64, (40, 40, 220)));

        // Вырожденный случай aHash: обе области однотонны — биты совпадают
        // один в один, сам хэш их НЕ различает.
        assert_eq!(red.hamming_distance(&blue), 0);
        // А средний цвет — различает, и полная проверка это ловит.
        assert!(red.avg_channel_diff(&blue) > DEFAULT_MAX_AVG_CHANNEL_DIFF);
        assert!(!red.matches(
            &blue,
            DEFAULT_MAX_HAMMING_DISTANCE,
            DEFAULT_MAX_AVG_CHANNEL_DIFF
        ));
    }

    #[test]
    fn slight_brightness_shift_still_matches() {
        let bright = ScreenFingerprint::from_snapshot(&solid_snapshot(64, 64, (245, 245, 245)));
        let dimmer = ScreenFingerprint::from_snapshot(&solid_snapshot(64, 64, (225, 225, 225)));

        assert!(dimmer.avg_channel_diff(&bright) <= DEFAULT_MAX_AVG_CHANNEL_DIFF);
        assert!(bright.matches(
            &dimmer,
            DEFAULT_MAX_HAMMING_DISTANCE,
            DEFAULT_MAX_AVG_CHANNEL_DIFF
        ));
    }

    #[test]
    fn legacy_number_fingerprint_loads_new_format_roundtrips() {
        // Старый формат: голое число в JSON.
        let legacy: ScreenFingerprint = serde_json::from_str("12345").unwrap();
        assert_eq!(legacy.avg_rgb, None);

        // Новый формат сериализуется объектом и читается обратно равным.
        let rich = ScreenFingerprint::from_snapshot(&solid_snapshot(64, 64, (10, 20, 30)));
        let json = serde_json::to_string(&rich).unwrap();
        assert!(json.contains("\"avg_rgb\""), "новая форма: {json}");
        let restored: ScreenFingerprint = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, rich);
    }
}
