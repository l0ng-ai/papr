//! Ingestion: fetching feeds over HTTP and parsing them into articles.
//!
//! The tauri-coupled refresh *scheduler* (progress channels, AppHandle) lives
//! in the desktop app (`papr_lib::scheduler`); this crate exposes only the
//! reusable, UI-free building blocks it is composed from.

pub mod discovery;
pub mod fetch;
pub mod newsletter;
pub mod parse;
pub mod refresh;
pub mod sources;
