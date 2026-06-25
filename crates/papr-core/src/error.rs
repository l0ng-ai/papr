//! Unified application error type. All Tauri commands return `Result<T, AppError>`.
//!
//! `AppError` serializes to `{ code, detail }` so the frontend can localise the
//! message: `code` is a stable identifier mapped to a translation key, and
//! `detail` carries any inner error text shown verbatim.

use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("migration error: {0}")]
    Migration(#[from] rusqlite_migration::Error),

    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("feed parse error: {0}")]
    FeedParse(#[from] feed_rs::parser::ParseFeedError),

    #[error("OPML error: {0}")]
    Opml(String),

    /// A known failure identified by a stable code the frontend localises.
    #[error("{0}")]
    Coded(&'static str),

    /// A free-form failure; its text is shown to the user verbatim.
    #[error("{0}")]
    Other(String),
}

impl AppError {
    pub fn other(msg: impl Into<String>) -> Self {
        AppError::Other(msg.into())
    }

    /// A localisable error — `code` maps to the frontend `error.<code>` key.
    pub fn code(code: &'static str) -> Self {
        AppError::Coded(code)
    }

    /// Stable error code consumed by the frontend i18n layer.
    fn code_str(&self) -> &str {
        match self {
            AppError::Db(_) => "db",
            AppError::Migration(_) => "migration",
            AppError::Http(_) => "network",
            AppError::FeedParse(_) => "feedParse",
            AppError::Opml(_) => "opml",
            AppError::Coded(c) => c,
            AppError::Other(_) => "other",
        }
    }

    /// Inner error text, when there is any worth surfacing.
    fn detail(&self) -> Option<String> {
        match self {
            AppError::Db(e) => Some(e.to_string()),
            AppError::Migration(e) => Some(e.to_string()),
            AppError::Http(e) => Some(e.to_string()),
            AppError::FeedParse(e) => Some(e.to_string()),
            AppError::Opml(s) => Some(s.clone()),
            AppError::Other(s) => Some(s.clone()),
            AppError::Coded(_) => None,
        }
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::Other(e.to_string())
    }
}

impl From<url::ParseError> for AppError {
    fn from(e: url::ParseError) -> Self {
        AppError::Other(format!("invalid URL: {e}"))
    }
}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut st = serializer.serialize_struct("AppError", 2)?;
        st.serialize_field("code", self.code_str())?;
        st.serialize_field("detail", &self.detail())?;
        st.end()
    }
}

pub type AppResult<T> = Result<T, AppError>;
