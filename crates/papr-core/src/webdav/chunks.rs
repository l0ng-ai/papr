//! Version 7 (reads v6): immutable, content-addressed gzip chunks and a conditional root.
//! Cache/staging tables are disk-backed SQLite tables, never whole-library Vecs.
use super::*;
use flate2::{read::MultiGzDecoder, write::GzEncoder, Compression};
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
};

const TARGET: usize = 4 * 1024 * 1024;
pub(super) const MAX_CHUNK: usize = 64 * 1024 * 1024;
const MAX_PARTS: usize = 200_000;

pub fn schema() -> &'static str {
    "CREATE TABLE webdav_objects(hash TEXT PRIMARY KEY,body BLOB NOT NULL);
     CREATE TABLE webdav_buckets(kind TEXT,bucket INTEGER,fingerprint TEXT NOT NULL,chunks TEXT NOT NULL,PRIMARY KEY(kind,bucket));
     CREATE TABLE webdav_incoming(kind TEXT,key TEXT,chunk TEXT NOT NULL,clock INTEGER NOT NULL,nonce TEXT NOT NULL,record TEXT NOT NULL,PRIMARY KEY(kind,key));
     CREATE INDEX webdav_incoming_chunk ON webdav_incoming(chunk);
     CREATE TABLE webdav_incoming_chunks(hash TEXT PRIMARY KEY,descriptor TEXT NOT NULL,dirty INTEGER NOT NULL);"
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Part {
    pub hash: String,
    pub kind: String,
    pub bucket: u8,
    pub bytes: usize,
    pub raw_bytes: usize,
    pub records: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    pub version: u32,
    pub format: String,
    pub chunks: Vec<Part>,
}
impl Manifest {
    pub fn empty() -> Self {
        Self {
            version: 7,
            format: "papr-gzip-jsonl-sha256".into(),
            chunks: vec![],
        }
    }
    pub fn validate(&self) -> AppResult<()> {
        if !matches!(self.version, 6 | 7) || self.format != "papr-gzip-jsonl-sha256" {
            return Err(AppError::code("webdavVersion"));
        }
        let mut seen = BTreeSet::new();
        if self.chunks.len() > MAX_PARTS {
            return Err(invalid());
        }
        for p in &self.chunks {
            if p.hash.len() != 64
                || !p
                    .hash
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                || p.kind.is_empty()
                || p.kind.len() > 32
                || !p.kind.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                || p.bytes == 0
                || p.bytes > MAX_CHUNK + 1024 * 1024
                || p.raw_bytes == 0
                || p.raw_bytes > MAX_CHUNK
                || p.records == 0
                || p.records > MAX_CHUNK
                || !seen.insert(&p.hash)
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
}
fn invalid() -> AppError {
    AppError::code("webdavInvalidData")
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn bucket(key: &str) -> u8 {
    Sha256::digest(key.as_bytes())[0]
}
pub(super) fn path(p: &Part) -> String {
    format!("papr-chunk-v6-{}.jsonl.gz", p.hash)
}

pub(super) fn available(conn: &Connection, p: &Part) -> AppResult<bool> {
    let descriptor = serde_json::to_string(p).map_err(|_| invalid())?;
    let known: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM webdav_incoming_chunks WHERE hash=?1 AND descriptor=?2)",
        params![p.hash, descriptor],
        |r| r.get(0),
    )?;
    Ok(known || cached(conn, p)?.is_some())
}
pub(super) fn cached(conn: &Connection, p: &Part) -> AppResult<Option<Vec<u8>>> {
    let body: Option<Vec<u8>> = conn
        .query_row(
            "SELECT body FROM webdav_objects WHERE hash=?1",
            [&p.hash],
            |r| r.get(0),
        )
        .optional()?;
    // Validate cached bytes too; evict damage so a retry can regenerate it.
    if let Some(body) = body {
        if body.len() == p.bytes && hash(&body) == p.hash {
            return Ok(Some(body));
        }
        conn.execute("DELETE FROM webdav_objects WHERE hash=?1", [&p.hash])?;
    }
    Ok(None)
}
pub(super) fn cache(conn: &Connection, p: &Part, body: &[u8]) -> AppResult<()> {
    decode(p, body)?;
    conn.execute(
        "INSERT OR REPLACE INTO webdav_objects VALUES(?1,?2)",
        params![p.hash, body],
    )?;
    Ok(())
}
fn decode(p: &Part, body: &[u8]) -> AppResult<Vec<Record>> {
    if body.len() != p.bytes || hash(body) != p.hash {
        return Err(invalid());
    }
    let mut raw = Vec::with_capacity(p.raw_bytes.min(TARGET));
    MultiGzDecoder::new(body)
        .take((p.raw_bytes + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|_| invalid())?;
    if raw.len() != p.raw_bytes || raw.last() != Some(&b'\n') {
        return Err(invalid());
    }
    let mut records = Vec::new();
    for line in raw.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let r: Record = serde_json::from_slice(line).map_err(|_| invalid())?;
        if r.kind != p.kind || bucket(&r.key) != p.bucket {
            return Err(invalid());
        }
        replication::validate_record(&r)?;
        records.push(r);
    }
    if records.len() != p.records {
        return Err(invalid());
    }
    Ok(records)
}
fn flush(
    conn: &Connection,
    kind: &str,
    bucket: u8,
    raw: &mut Vec<u8>,
    count: &mut usize,
    parts: &mut Vec<Part>,
) -> AppResult<()> {
    if raw.is_empty() {
        return Ok(());
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(raw).map_err(|_| invalid())?;
    let body = encoder.finish().map_err(|_| invalid())?;
    let p = Part {
        hash: hash(&body),
        kind: kind.into(),
        bucket,
        bytes: body.len(),
        raw_bytes: raw.len(),
        records: *count,
    };
    conn.execute(
        "INSERT OR REPLACE INTO webdav_objects VALUES(?1,?2)",
        params![p.hash, body],
    )?;
    parts.push(p);
    raw.clear();
    *count = 0;
    Ok(())
}

/// Header fingerprints avoid parsing/compressing unchanged article content.
/// Only one changed bucket and one chunk are held in memory at a time.
pub(super) fn export(conn: &mut Connection) -> AppResult<Manifest> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS webdav_export_keys(kind TEXT,key TEXT,bucket INTEGER,PRIMARY KEY(kind,key));
        CREATE INDEX IF NOT EXISTS temp.webdav_export_bucket ON webdav_export_keys(kind,bucket,key);
        DELETE FROM webdav_export_keys;
        CREATE TEMP TABLE IF NOT EXISTS webdav_upload_baseline(kind TEXT,key TEXT,record TEXT,PRIMARY KEY(kind,key));
        DELETE FROM webdav_upload_baseline;")?;
    let mut fingerprints: BTreeMap<(String, u8), Sha256> = BTreeMap::new();
    {
        let mut stmt = tx
            .prepare("SELECT kind,key,clock,nonce,deleted FROM webdav_records ORDER BY kind,key")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let kind: String = r.get(0)?;
            let key: String = r.get(1)?;
            let b = bucket(&key);
            let clock: i64 = r.get(2)?;
            let nonce: String = r.get(3)?;
            let deleted: i64 = r.get(4)?;
            let h = fingerprints.entry((kind.clone(), b)).or_default();
            // JSON tuple is length-delimited and unambiguous.
            h.update(serde_json::to_vec(&(&key, clock, nonce, deleted)).map_err(|_| invalid())?);
            tx.execute(
                "INSERT INTO webdav_export_keys VALUES(?1,?2,?3)",
                params![kind, key, b],
            )?;
        }
    }
    let mut manifest = Manifest::empty();
    for ((kind, b), h) in fingerprints {
        let fp = format!("{:x}", h.finalize());
        let cached: Option<String> = tx
            .query_row(
                "SELECT chunks FROM webdav_buckets WHERE kind=?1 AND bucket=?2 AND fingerprint=?3",
                params![kind, b, fp],
                |r| r.get(0),
            )
            .optional()?;
        let mut parts: Vec<Part> = Vec::new();
        if let Some(json) = cached {
            parts = serde_json::from_str(&json).map_err(|_| invalid())?;
        }
        let intact= !parts.is_empty() && parts.iter().map(|p|tx.query_row("SELECT EXISTS(SELECT 1 FROM webdav_objects WHERE hash=?1 AND length(body)=?2)",params![p.hash,p.bytes],|r|r.get::<_,bool>(0)).map_err(AppError::from)).collect::<AppResult<Vec<_>>>()?.iter().all(|b|*b);
        if !intact {
            parts.clear();
            let sql=format!("SELECT {} FROM webdav_records JOIN webdav_export_keys USING(kind,key) WHERE webdav_export_keys.kind=?1 AND bucket=?2 ORDER BY key",replication::record_json_sql());
            let mut stmt = tx.prepare(&sql)?;
            let mut rows = stmt.query(params![kind, b])?;
            let mut raw = Vec::with_capacity(TARGET);
            let mut count = 0;
            while let Some(row) = rows.next()? {
                let json: String = row.get(0)?;
                if json.len() + 1 > MAX_CHUNK {
                    return Err(AppError::code("webdavTooLarge"));
                }
                if raw.len() + json.len() + 1 > TARGET {
                    flush(&tx, &kind, b, &mut raw, &mut count, &mut parts)?;
                }
                raw.extend_from_slice(json.as_bytes());
                raw.push(b'\n');
                count += 1;
            }
            flush(&tx, &kind, b, &mut raw, &mut count, &mut parts)?;
            tx.execute(
                "INSERT OR REPLACE INTO webdav_buckets VALUES(?1,?2,?3,?4)",
                params![
                    kind,
                    b,
                    fp,
                    serde_json::to_string(&parts).map_err(|_| invalid())?
                ],
            )?;
        }
        manifest.chunks.extend(parts);
    }
    manifest.validate()?;
    tx.execute(&format!("INSERT INTO webdav_upload_baseline SELECT kind,key,{} FROM webdav_records WHERE kind NOT IN ('article','enclosure','retention')",replication::record_json_sql()),[])?;
    tx.commit()?;
    Ok(manifest)
}

/// Swap the complete incoming snapshot atomically after every referenced chunk
/// has passed integrity checks. Interrupted downloads reuse verified objects.
pub(super) fn stage(conn: &mut Connection, manifest: &Manifest) -> AppResult<()> {
    manifest.validate()?;
    let mut legacy_folders = Vec::new();
    if manifest.version == 6 {
        for p in manifest.chunks.iter().filter(|p| p.kind == "folder") {
            let body = cached(conn, p)?.ok_or_else(invalid)?;
            legacy_folders.extend(decode(p, &body)?);
        }
    }
    let paths = replication::legacy_folder_paths(&legacy_folders)?;
    let tx = conn.transaction()?;
    let old: BTreeSet<String> = {
        let mut s = tx.prepare("SELECT hash FROM webdav_incoming_chunks")?;
        let rows = s.query_map([], |r| r.get(0))?;
        rows.collect::<Result<_, _>>()?
    };
    let wanted: BTreeSet<_> = manifest.chunks.iter().map(|p| p.hash.clone()).collect();
    if old.is_empty() {
        tx.execute("DELETE FROM webdav_incoming", [])?;
    }
    for h in old.difference(&wanted) {
        tx.execute("DELETE FROM webdav_incoming WHERE chunk=?1", [h])?;
        tx.execute("DELETE FROM webdav_incoming_chunks WHERE hash=?1", [h])?;
    }
    for p in &manifest.chunks {
        let descriptor = serde_json::to_string(p).map_err(|_| invalid())?;
        if old.contains(&p.hash) {
            let previous: String = tx.query_row(
                "SELECT descriptor FROM webdav_incoming_chunks WHERE hash=?1",
                [&p.hash],
                |r| r.get(0),
            )?;
            if previous != descriptor {
                return Err(invalid());
            }
            continue;
        }
        let body = cached(&tx, p)?.ok_or_else(invalid)?;
        for mut r in decode(p, &body)? {
            if manifest.version == 6 {
                replication::convert_legacy_folder(&mut r, &paths);
            }
            tx.execute(
                "INSERT INTO webdav_incoming(kind,key,record,chunk,clock,nonce) VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    r.kind,
                    r.key,
                    serde_json::to_string(&r).map_err(|_| invalid())?,
                    p.hash,
                    r.clock,
                    r.nonce
                ],
            )?;
        }
        tx.execute(
            "INSERT INTO webdav_incoming_chunks VALUES(?1,?2,1)",
            params![p.hash, descriptor],
        )?;
    }
    tx.commit()?;
    Ok(())
}
pub(super) fn stage_legacy(conn: &mut Connection, snapshot: &Snapshot) -> AppResult<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM webdav_incoming", [])?;
    tx.execute("DELETE FROM webdav_incoming_chunks", [])?;
    for r in &snapshot.records {
        replication::validate_record(r)?;
        tx.execute(
            "INSERT INTO webdav_incoming(kind,key,record,chunk,clock,nonce) VALUES(?1,?2,?3,'legacy',?4,?5)",
            params![
                r.kind,
                r.key,
                serde_json::to_string(r).map_err(|_| invalid())?,
                r.clock,
                r.nonce
            ],
        )?;
    }
    tx.execute(
        "INSERT INTO webdav_incoming_chunks VALUES('legacy','',1)",
        [],
    )?;
    tx.commit()?;
    Ok(())
}
pub(super) fn acknowledge(conn: &Connection) -> AppResult<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM webdav_baseline", [])?;
    tx.execute(
        "INSERT INTO webdav_baseline SELECT * FROM webdav_upload_baseline",
        [],
    )?;
    tx.commit()?;
    Ok(())
}
/// Keep only objects referenced by the current incoming snapshot or export.
/// Remote objects are deliberately retained for other/offline clients.
pub(super) fn prune(conn: &Connection) -> AppResult<()> {
    conn.execute("DELETE FROM webdav_objects WHERE hash NOT IN (SELECT hash FROM webdav_incoming_chunks) AND hash NOT IN (SELECT json_extract(value,'$.hash') FROM webdav_buckets,json_each(chunks))",[])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_bombs_truncation_wrong_descriptors_and_duplicate_chunks() {
        let conn = db::open(std::path::Path::new(":memory:")).unwrap();
        let r = replication::records(&conn).unwrap().remove(0);
        let mut raw = serde_json::to_vec(&r).unwrap();
        raw.push(b'\n');
        let mut count = 1;
        let mut parts = vec![];
        flush(
            &conn,
            &r.kind,
            bucket(&r.key),
            &mut raw,
            &mut count,
            &mut parts,
        )
        .unwrap();
        let p = parts.remove(0);
        let body = cached(&conn, &p).unwrap().unwrap();
        assert_eq!(decode(&p, &body).unwrap(), vec![r]);
        let mut bad = p.clone();
        bad.raw_bytes = 1;
        assert!(decode(&bad, &body).is_err());
        bad = p.clone();
        bad.records += 1;
        assert!(decode(&bad, &body).is_err());
        bad = p.clone();
        bad.bucket = bad.bucket.wrapping_add(1);
        assert!(decode(&bad, &body).is_err());
        let mut truncated = body.clone();
        truncated.truncate(truncated.len() - 5);
        bad = p.clone();
        bad.bytes = truncated.len();
        bad.hash = hash(&truncated);
        assert!(decode(&bad, &truncated).is_err());
        let mut m = Manifest::empty();
        m.chunks = vec![p.clone(), p.clone()];
        assert!(m.validate().is_err());
        let damaged = vec![0; body.len()];
        conn.execute("UPDATE webdav_objects SET body=?1", [damaged])
            .unwrap();
        assert!(cached(&conn, &p).unwrap().is_none());
        assert_eq!(
            conn.query_row("SELECT count(*) FROM webdav_objects", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn upload_baseline_describes_export_not_edits_made_during_network_requests() {
        let mut conn = db::open(std::path::Path::new(":memory:")).unwrap();
        db::set_setting(&conn, "ai_summary_prompt", "exported prompt").unwrap();
        let m = export(&mut conn).unwrap();
        db::set_setting(&conn, "ai_summary_prompt", "new local edit").unwrap();
        acknowledge(&conn).unwrap();
        let baseline: String = conn
            .query_row(
                "SELECT record FROM webdav_baseline WHERE kind='ai_prompt'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Record>(&baseline).unwrap().data["prompt"],
            "exported prompt"
        );
        // Incoming cache descriptor tampering is rejected even when hash stays unchanged.
        stage(&mut conn, &m).unwrap();
        let mut bad = m;
        bad.chunks[0].records += 1;
        assert!(stage(&mut conn, &bad).is_err());
    }
}
