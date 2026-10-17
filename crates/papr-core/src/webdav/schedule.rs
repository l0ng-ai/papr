//! Device-local automatic sync cadence, independent of subscription refresh.
use super::*;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Options {
    /// Zero disables automatic runs. Manual sync always remains available.
    pub interval_minutes: u32,
}
impl Options {
    pub fn read(conn: &Connection) -> AppResult<Self> {
        Ok(Self { interval_minutes: db::setting_parsed::<u32>(conn, "webdav_interval_minutes", 60).min(1440) })
    }
    pub fn save(&self, conn: &Connection) -> AppResult<()> {
        if self.interval_minutes > 1440 { return Err(AppError::code("webdavInvalidSchedule")); }
        db::set_setting(conn, "webdav_interval_minutes", &self.interval_minutes.to_string())
    }
    pub(super) fn due(&self, conn: &Connection, now: chrono::DateTime<chrono::Utc>) -> AppResult<bool> {
        if self.interval_minutes == 0 || !replication::conflicts(conn)?.is_empty() { return Ok(false); }
        let last = db::get_setting(conn, "webdav_last_attempt")?
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok());
        Ok(last.is_none_or(|last| {
            let elapsed = now.signed_duration_since(last).num_seconds();
            // Recover from a device clock that was moved backwards.
            elapsed < 0 || elapsed >= i64::from(self.interval_minutes) * 60
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cadence_includes_failed_attempts_manual_mode_and_clock_changes() {
        let c = db::open(std::path::Path::new(":memory:")).unwrap();
        let now = chrono::Utc::now();
        let mut options = Options::read(&c).unwrap();
        assert_eq!(options.interval_minutes, 60);
        assert!(options.due(&c, now).unwrap());
        db::set_setting(&c, "webdav_last_attempt", &now.to_rfc3339()).unwrap();
        assert!(!options.due(&c, now + chrono::Duration::minutes(59)).unwrap());
        assert!(options.due(&c, now + chrono::Duration::minutes(60)).unwrap());
        assert!(options.due(&c, now - chrono::Duration::minutes(1)).unwrap());
        options.interval_minutes = 0;
        options.save(&c).unwrap();
        assert!(!Options::read(&c).unwrap().due(&c, now + chrono::Duration::days(2)).unwrap());
        options.interval_minutes = 1441;
        assert!(options.save(&c).is_err());
    }
}
