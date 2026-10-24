//! Errors retain their origin until the presentation boundary.
use std::{error::Error, fmt, time::Duration};

#[derive(Debug)]
pub enum AppError {
    Io(std::io::Error),
    Database(rusqlite::Error),
    Json(serde_json::Error),
    Image(image::ImageError),
    Domain(DomainError),
    FileConflict(std::path::PathBuf),
    Rollback {
        database: rusqlite::Error,
        restore: std::io::Error,
        retained: std::path::PathBuf,
    },
    OcrReadInterrupted,
    OcrTimeout(Duration),
    OcrLanguage(&'static str),
    OcrExit {
        code: Option<i32>,
        detail: String,
    },
    Context {
        operation: String,
        source: Box<dyn Error + Send + Sync>,
    },
}
impl AppError {
    pub fn context(
        operation: impl Into<String>,
        source: impl Into<Box<dyn Error + Send + Sync>>,
    ) -> Self {
        Self::Context {
            operation: operation.into(),
            source: source.into(),
        }
    }
}
impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => e.fmt(f),
            Self::Database(e) => e.fmt(f),
            Self::Json(e) => e.fmt(f),
            Self::Image(e) => e.fmt(f),
            Self::Domain(error) => f.write_str(error.message()),
            Self::FileConflict(path) => write!(f, "Conflit de nom de fichier : {}", path.display()),
            Self::Rollback { database, restore, retained } => write!(f, "Index : {database}. Image conservée dans {} : {restore}", retained.display()),
            Self::OcrReadInterrupted => f.write_str("Lecture OCR interrompue"),
            Self::OcrTimeout(limit) => write!(f, "Délai maximal OCR dépassé ({:.2} s)", limit.as_secs_f64()),
            Self::OcrLanguage(lang) => write!(f, "Langue {lang} manquante"),
            Self::OcrExit { code, detail } => write!(f, "Tesseract : vérifie le moteur et les langues fra et eng (code {code:?} : {detail})"),
            Self::Context { operation, source } => write!(f, "{operation} : {source}"),
        }
    }
}
impl Error for AppError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Rollback { database, .. } => Some(database),
            Self::Io(e) => Some(e),
            Self::Database(e) => Some(e),
            Self::Json(e) => Some(e),
            Self::Image(e) => Some(e),
            Self::Context { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}
macro_rules! from_error {
    ($ty:ty, $variant:ident) => {
        impl From<$ty> for AppError {
            fn from(e: $ty) -> Self {
                Self::$variant(e)
            }
        }
    };
}
from_error!(std::io::Error, Io);
from_error!(rusqlite::Error, Database);
from_error!(serde_json::Error, Json);
from_error!(image::ImageError, Image);
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn context_preserves_the_original_error_and_kind() {
        let error = AppError::context(
            "Lecture",
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        );
        let source = error
            .source()
            .unwrap()
            .downcast_ref::<std::io::Error>()
            .unwrap();
        assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(error.to_string().starts_with("Lecture : "));
    }

    #[test]
    fn database_and_io_failures_remain_distinguishable_without_parsing_messages() {
        let database = rusqlite::Connection::open_in_memory().unwrap();
        let failure = database
            .execute("UPDATE missing_table SET value=1", [])
            .unwrap_err();
        let error = AppError::from(failure);
        assert!(matches!(error, AppError::Database(_)));
        assert!(error.source().unwrap().is::<rusqlite::Error>());
        let error = AppError::from(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert!(
            matches!(error, AppError::Io(ref cause) if cause.kind()==std::io::ErrorKind::NotFound)
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomainError {
    TrayUnavailable,
    BatchOutsideExports,
    CaptureOutsideFolder,
    MissingCaptureName,
    DatabaseUnavailable,
    JournalOutsideFolder,
    JournalEscapesExports,
    RecoveryConflict,
    InvalidName,
    EmptyIdentity,
    DuplicateIdentity,
    ExistingFilename,
    InvalidChunkSize,
    MissingPngName,
    UnjournaledMove,
    ExportCollision,
    PathOutsideFolder,
    InvalidPathComponent,
    LinkedWorkingFolder,
    CaptureFilesUnavailable,
    DuplicateIndexUnavailable,
    InvalidDuration,
    CleanupOutsideArchives,
    LinkedArchive,
    WindowUnavailable,
    TrayCreation,
    OcrStdout,
    OcrStderr,
    OcrUnavailable,
    OcrAreaTooSmall,
    HeaderOutsideScreen,
    TitleOutsideScreen,
    ActionInterrupted,
    ScreenNotFound,
    UnexpectedImage,
    InvalidOcrName,
    DiagnosticCrop,
    OcrStopped,
}
impl DomainError {
    pub fn message(self) -> &'static str {
        match self {
            Self::TrayUnavailable => "La zone de notification Windows est indisponible",
            Self::BatchOutsideExports => "Lot hors du dossier des envois",
            Self::CaptureOutsideFolder => "Capture hors du dossier autorisé",
            Self::MissingCaptureName => "Capture sans nom",
            Self::DatabaseUnavailable => "Gestionnaire SQLite indisponible",
            Self::JournalOutsideFolder => "Journal de préparation hors du dossier autorisé",
            Self::JournalEscapesExports => "Le journal contient un lien hors du dossier des envois",
            Self::RecoveryConflict => {
                "Récupération suspendue : les deux copies existent. Aucun fichier n'a été écrasé."
            }
            Self::InvalidName => {
                "Nom invalide : utilise un nom sans caractère interdit, de 90 caractères maximum."
            }
            Self::EmptyIdentity => "Nom sans identité exploitable",
            Self::DuplicateIdentity => {
                "Un objet portant ce nom est déjà présent ou préparé. Aucun fichier n'a été écrasé."
            }
            Self::ExistingFilename => "Le nom de fichier existe déjà",
            Self::InvalidChunkSize => {
                "Le maximum de captures par dossier doit être au moins égal à 1."
            }
            Self::MissingPngName => "PNG sans nom",
            Self::UnjournaledMove => "Déplacement non journalisé",
            Self::ExportCollision => "Un fichier est apparu dans le lot pendant la préparation",
            Self::PathOutsideFolder => "Chemin hors du dossier autorisé",
            Self::InvalidPathComponent => "Composant de chemin non autorisé",
            Self::LinkedWorkingFolder => {
                "Un lien ou une jonction est présent dans le dossier de travail"
            }
            Self::CaptureFilesUnavailable => "Une opération sur les captures s'est interrompue.",
            Self::DuplicateIndexUnavailable => "Index des doublons indisponible",
            Self::InvalidDuration => "Durée invalide",
            Self::CleanupOutsideArchives => "Nettoyage hors des archives autorisées",
            Self::LinkedArchive => "Lien hors des archives",
            Self::WindowUnavailable => "La fenêtre Windows est indisponible",
            Self::TrayCreation => "Création de l'icône Windows impossible",
            Self::OcrStdout => "Sortie OCR indisponible",
            Self::OcrStderr => "Erreur OCR indisponible",
            Self::OcrUnavailable => "Tesseract indisponible",
            Self::OcrAreaTooSmall => "Zone OCR trop petite",
            Self::HeaderOutsideScreen => "En-tête HDV hors écran",
            Self::TitleOutsideScreen => "Titre de fiche hors écran",
            Self::ActionInterrupted => "Opération interrompue",
            Self::ScreenNotFound => "Écran choisi introuvable",
            Self::UnexpectedImage => "Format d'image inattendu",
            Self::InvalidOcrName => "Nom d'objet OCR invalide",
            Self::DiagnosticCrop => "Recadrage diagnostic impossible",
            Self::OcrStopped => "Thread OCR arrêté",
        }
    }
}
