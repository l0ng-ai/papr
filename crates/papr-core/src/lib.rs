//! papr-core — the UI-free heart of Papr.
//!
//! Everything here is free of any Tauri/desktop dependency so it can be linked
//! by both the desktop app (`papr_lib`) and the headless agent CLI (`papr-cli`).
//! It owns the SQLite schema + migrations, all typed data-access functions,
//! feed fetching/parsing/discovery, and HTML sanitization.
//!
//! The tauri-coupled refresh scheduler (progress channels) is NOT here — it
//! lives in the desktop app and is composed from the building blocks in
//! [`ingestion`].

pub mod ai;
pub mod db;
pub mod error;
pub mod extraction;
pub mod ingestion;
pub mod models;
pub mod opml;
pub mod sanitize;
pub mod sync;
