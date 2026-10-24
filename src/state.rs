//! Domain states are independent of translated labels and legacy SQLite values.
use rusqlite::{
    types::{FromSql, FromSqlResult, ToSqlOutput, ValueRef},
    ToSql,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VisitState {
    Saved,
    NeedsReview,
    Pending,
    Unknown(String),
}
impl VisitState {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Saved => "enregistre",
            Self::NeedsReview => "a_reprendre",
            Self::Pending => "en_attente",
            Self::Unknown(s) => s,
        }
    }
}
impl From<&str> for VisitState {
    fn from(s: &str) -> Self {
        match s {
            "enregistre" => Self::Saved,
            "a_reprendre" => Self::NeedsReview,
            "en_attente" => Self::Pending,
            _ => Self::Unknown(s.into()),
        }
    }
}
impl ToSql for VisitState {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_str()))
    }
}
impl FromSql for VisitState {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        Ok(Self::from(value.as_str()?))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessingPhase {
    Ready,
    Recognizing,
    Finished,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GalleryFilter {
    All,
    Pending,
}

#[derive(Debug)]
pub enum OcrStatus {
    Bundled,
    External,
    Missing,
    Incomplete(crate::error::AppError),
}
impl std::fmt::Display for OcrStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bundled => f.write_str("Tesseract intégré · français + anglais : OK"),
            Self::External => f.write_str("Tesseract externe · français + anglais : OK"),
            Self::Missing => f.write_str("Tesseract introuvable"),
            Self::Incomplete(error) => write!(f, "OCR incomplet : {error}"),
        }
    }
}
impl ProcessingPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ready => "Prêt",
            Self::Recognizing => "Reconnaissance OCR",
            Self::Finished => "Analyse terminée",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_and_future_states_survive_a_database_roundtrip() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        for code in ["enregistre", "a_reprendre", "en_attente", "etat_futur"] {
            let state = VisitState::from(code);
            let read: VisitState = db
                .query_row("SELECT ?1", [&state], |row| row.get(0))
                .unwrap();
            assert_eq!(read, state);
            assert_eq!(read.as_str(), code);
        }
    }
}
