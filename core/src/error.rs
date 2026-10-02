use std::path::PathBuf;

/// Ошибки, которые может вернуть ядро приложения.
///
/// Специально не смешиваем сюда ошибки UI — этот тип пересекает границу
/// `core` -> `app`, и `app` решает, как показать каждую из них пользователю.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("не удалось подписаться на глобальные события ввода: {0}")]
    ListenFailed(String),

    #[error("не удалось отправить событие ввода в ОС: {0}")]
    SimulateFailed(String),

    #[error("не удалось сделать снимок экрана: {0}")]
    ScreenCaptureFailed(String),

    #[error("запись уже идёт")]
    AlreadyRecording,

    #[error("запись не запущена")]
    NotRecording,

    #[error("воспроизведение уже идёт")]
    AlreadyPlaying,

    #[error("воспроизведение не запущено")]
    NotPlaying,

    #[error("макрос пустой — нечего сохранять")]
    EmptyMacro,

    #[error("макрос с id {0} не найден")]
    MacroNotFound(uuid::Uuid),

    #[error("ошибка чтения/записи файла {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("ошибка (де)сериализации: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("не удалось определить директорию данных приложения")]
    NoDataDir,
}
