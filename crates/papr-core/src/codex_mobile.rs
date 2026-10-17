//! Mobile cannot spawn the desktop Codex CLI. Keep synced preferences intact
//! and return an actionable error rather than silently changing providers.
use crate::{ai::{ChatOutcome, DeltaSink}, error::{AppError, AppResult}};
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all="camelCase")]
pub struct AccountStatus {
    pub email: Option<String>, pub plan: Option<String>, pub logged_in: bool,
    pub pending: bool, pub error: Option<String>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all="camelCase")]
pub struct Model {
    pub id: String, pub name: String, pub is_default: bool,
    pub reasoning_efforts: Vec<String>, pub default_effort: Option<String>,
}
fn unsupported() -> AppError { AppError::code("codexMobileUnsupported") }
pub async fn status() -> AppResult<AccountStatus> {
    Ok(AccountStatus { email:None, plan:None, logged_in:false, pending:false, error:None })
}
pub async fn login() -> AppResult<String> { Err(unsupported()) }
pub async fn cancel_login() -> AppResult<()> { Ok(()) }
pub async fn logout() -> AppResult<()> { Ok(()) }
pub async fn models() -> AppResult<Vec<Model>> { Err(unsupported()) }
pub async fn stream(_: &str, _: Option<&str>, _: &str, _: &str, _: &mut DeltaSink<'_>, _: u32) -> AppResult<ChatOutcome> {
    Err(unsupported())
}
