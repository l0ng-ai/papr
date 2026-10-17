//! Logical WebDAV replication. Only the explicit library columns below leave
//! the machine. AI preferences are explicitly allowlisted; credentials,
//! other local settings, IDs and derived caches never leave the machine.
//! SQLite triggers cover desktop, CLI, bulk edits and background ingestion.
use crate::error::{AppError, AppResult};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::LazyLock;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub kind: String,
    pub key: String,
    pub clock: i64,
    pub nonce: String,
    pub deleted: bool,
    pub data: Value,
}

struct Spec {
    kind: &'static str,
    table: &'static str,
    key: String,
    // (local column, wire expression). Foreign keys are encoded as natural
    // keys, then resolved back to this device's IDs on import.
    columns: Vec<(&'static str, String)>,
    filter: String,
    state: bool,
}

fn specs() -> Vec<Spec> {
    let feed = "(SELECT feed_url FROM feeds WHERE id=t.feed_id)";
    let article = "(SELECT json_array(f.feed_url,a.guid) || '' FROM articles a JOIN feeds f ON f.id=a.feed_id WHERE a.id=t.article_id)";
    let article_key = format!("json_array({feed},t.guid)");
    let mut out = Vec::new();
    let mut add = |kind,
                   table,
                   key: String,
                   names: &[&'static str],
                   refs: Vec<(&'static str, String)>,
                   filter: String,
                   state| {
        let mut columns: Vec<_> = names.iter().map(|c| (*c, format!("t.{c}"))).collect();
        columns.extend(refs);
        out.push(Spec {
            kind,
            table,
            key,
            columns,
            filter,
            state,
        });
    };
    add(
        "folder",
        "folders",
        "lower(t.name)".into(),
        &["name", "position"],
        vec![(
            "parent_id",
            "(SELECT lower(name) FROM folders WHERE id=t.parent_id)".into(),
        )],
        "1".into(),
        false,
    );
    add(
        "feed",
        "feeds",
        "t.feed_url".into(),
        &[
            "feed_url",
            "site_url",
            "title",
            "description",
            "favicon_url",
            "source_type",
            "custom_title",
            "position",
            "refresh_interval_min",
            "auto_translate",
            "open_mode",
        ],
        vec![(
            "folder_id",
            "(SELECT lower(name) FROM folders WHERE id=t.folder_id)".into(),
        )],
        "t.source_type != 'newsletter'".into(),
        false,
    );
    add(
        "rule",
        "rules",
        format!("json_array(t.name,t.field,t.query,t.action,{feed})"),
        &[
            "name",
            "enabled",
            "field",
            "query",
            "action",
            "position",
            "created_at",
        ],
        vec![("feed_id", feed.into())],
        "t.feed_id IS NULL OR (SELECT source_type FROM feeds WHERE id=t.feed_id) != 'newsletter'"
            .into(),
        false,
    );
    let article_filter = "(SELECT source_type FROM feeds WHERE id=t.feed_id) != 'newsletter'";
    add(
        "retention",
        "article_tombstones",
        article_key.clone(),
        &["guid"],
        vec![("feed_id", feed.into())],
        article_filter.into(),
        false,
    );
    add(
        "article",
        "articles",
        article_key.clone(),
        &[
            "guid",
            "url",
            "title",
            "author",
            "summary",
            "content_html",
            "extracted_html",
            "body_text",
            "image_url",
            "published_at",
            "fetched_at",
        ],
        vec![("feed_id", feed.into())],
        article_filter.into(),
        false,
    );
    add(
        "tag",
        "tags",
        "lower(t.name)".into(),
        &["name", "color", "position"],
        vec![(
            "parent_id",
            "(SELECT lower(name) FROM tags WHERE id=t.parent_id)".into(),
        )],
        "1".into(),
        false,
    );
    let child_filter = "(SELECT f.source_type FROM articles a JOIN feeds f ON f.id=a.feed_id WHERE a.id=t.article_id) != 'newsletter'";
    add(
        "enclosure",
        "enclosures",
        format!("json_array({article},t.url)"),
        &["url", "mime_type", "length"],
        vec![("article_id", article.into())],
        child_filter.into(),
        false,
    );
    add(
        "article_tag",
        "article_tags",
        format!("json_array({article},(SELECT lower(name) FROM tags WHERE id=t.tag_id))"),
        &[],
        vec![
            ("article_id", article.into()),
            (
                "tag_id",
                "(SELECT lower(name) FROM tags WHERE id=t.tag_id)".into(),
            ),
        ],
        child_filter.into(),
        false,
    );
    add(
        "highlight",
        "highlights",
        format!("json_array({article},t.quote,t.prefix,t.suffix,t.text_offset)"),
        &[
            "quote",
            "prefix",
            "suffix",
            "text_offset",
            "color",
            "note",
            "created_at",
        ],
        vec![("article_id", article.into())],
        child_filter.into(),
        false,
    );
    for field in ["is_read", "is_starred", "read_later", "reading_progress"] {
        add(
            field,
            "articles",
            article_key.clone(),
            &[field],
            vec![],
            article_filter.into(),
            true,
        );
    }
    out
}

// JSON arrays make paths unambiguous even for legacy names containing slashes.
fn tag_key(alias: &str) -> String {
    format!("(WITH RECURSIVE c(name,parent_id,depth) AS (SELECT lower({alias}.name),{alias}.parent_id,0 UNION ALL SELECT lower(p.name),p.parent_id,c.depth+1 FROM tags p JOIN c ON p.id=c.parent_id) SELECT json_group_array(name) FROM (SELECT name FROM c ORDER BY depth DESC))")
}
fn folder_key(alias: &str) -> String {
    tag_key(alias).replace("FROM tags ", "FROM folders ")
}

fn tag_specs_v25() -> Vec<Spec> {
    let mut all = specs();
    for spec in &mut all {
        if spec.kind == "tag" {
            spec.key = tag_key("t");
            spec.columns
                .iter_mut()
                .find(|(c, _)| *c == "parent_id")
                .unwrap()
                .1 = format!(
                "(SELECT {} || '' FROM tags q WHERE q.id=t.parent_id)",
                tag_key("q")
            );
        }
        if spec.kind == "article_tag" {
            let reference = format!(
                "(SELECT {} || '' FROM tags q WHERE q.id=t.tag_id)",
                tag_key("q")
            );
            spec.key=format!("json_array((SELECT json_array(f.feed_url,a.guid) || '' FROM articles a JOIN feeds f ON f.id=a.feed_id WHERE a.id=t.article_id),{reference})");
            spec.columns
                .iter_mut()
                .find(|(c, _)| *c == "tag_id")
                .unwrap()
                .1 = reference;
            spec.filter
                .push_str(" AND EXISTS(SELECT 1 FROM tags q WHERE q.id=t.tag_id)");
        }
    }
    all
}
fn current_specs() -> Vec<Spec> {
    let mut all = tag_specs_v25();
    for spec in &mut all {
        if spec.kind == "folder" {
            spec.key = folder_key("t");
            spec.columns
                .iter_mut()
                .find(|(c, _)| *c == "parent_id")
                .unwrap()
                .1 = format!(
                "(SELECT {} || '' FROM folders q WHERE q.id=t.parent_id)",
                folder_key("q")
            );
        }
        if spec.kind == "feed" {
            spec.columns.push(("max_articles", "t.max_articles".into()));
            spec.columns
                .iter_mut()
                .find(|(c, _)| *c == "folder_id")
                .unwrap()
                .1 = format!(
                "(SELECT {} || '' FROM folders q WHERE q.id=t.folder_id)",
                folder_key("q")
            );
        }
    }
    for (kind, key, fields) in [
        (
            "ai_config",
            "selection",
            vec!["provider", "model", "effort"],
        ),
        ("ai_prompt", "summary", vec!["prompt"]),
    ] {
        all.push(Spec {
            kind,
            table: "settings",
            key: format!("'{key}'"),
            columns: fields.into_iter().map(|s| (s, String::new())).collect(),
            filter: "1".into(),
            state: false,
        });
    }
    all
}

/// Folder keys are complete paths, like tag keys. Repair descendants after an
/// ancestor move, preserving tombstones for the old paths and feed references.
fn repair_folder_sql() -> String {
    let mut sql=String::from("UPDATE webdav_control SET clock=max(clock+1,CAST(unixepoch('subsec')*1000 AS INTEGER)) WHERE id=1;");
    for spec in current_specs()
        .into_iter()
        .filter(|s| ["folder", "feed"].contains(&s.kind))
    {
        let data = payload(&spec);
        sql.push_str(&format!("UPDATE webdav_records SET deleted=1,clock=(SELECT clock FROM webdav_control WHERE id=1),nonce=lower(hex(randomblob(16))) WHERE kind='{kind}' AND deleted=0 AND key NOT IN (SELECT {key} FROM {table} t WHERE {filter});
         INSERT INTO webdav_records SELECT '{kind}',{key},(SELECT clock FROM webdav_control WHERE id=1),lower(hex(randomblob(16))),0,{data} FROM {table} t WHERE {filter}
         ON CONFLICT(kind,key) DO UPDATE SET clock=excluded.clock,nonce=excluded.nonce,deleted=0,data=excluded.data WHERE webdav_records.deleted!=0 OR webdav_records.data IS NOT excluded.data;",kind=spec.kind,key=spec.key,table=spec.table,filter=spec.filter));
    }
    sql
}

pub(crate) fn legacy_folder_paths(
    records: &[Record],
) -> AppResult<std::collections::BTreeMap<String, String>> {
    let folders: std::collections::BTreeMap<_, _> = records
        .iter()
        .filter(|r| r.kind == "folder")
        .map(|r| {
            (
                r.key.clone(),
                r.data["parent_id"].as_str().map(str::to_owned),
            )
        })
        .collect();
    let mut paths = std::collections::BTreeMap::new();
    for key in folders.keys() {
        let mut names = Vec::new();
        let mut current = Some(key.clone());
        while let Some(name) = current {
            if names.contains(&name) || names.len() > 100 {
                return Err(invalid());
            }
            current = folders.get(&name).cloned().flatten();
            names.push(name);
        }
        names.reverse();
        paths.insert(
            key.clone(),
            serde_json::to_string(&names).map_err(|_| invalid())?,
        );
    }
    Ok(paths)
}
pub(crate) fn convert_legacy_folder(
    r: &mut Record,
    paths: &std::collections::BTreeMap<String, String>,
) {
    let path = |s: &str| {
        paths
            .get(s)
            .cloned()
            .unwrap_or_else(|| serde_json::json!([s]).to_string())
    };
    if r.kind == "folder" {
        r.key = path(&r.key);
        r.data["parent_id"] = r.data["parent_id"]
            .as_str()
            .map(|s| Value::String(path(s)))
            .unwrap_or(Value::Null);
    }
    if r.kind == "feed" {
        r.data["folder_id"] = r.data["folder_id"]
            .as_str()
            .map(|s| Value::String(path(s)))
            .unwrap_or(Value::Null);
    }
}
pub(crate) fn upgrade_legacy_folders(records: &mut [Record]) -> AppResult<()> {
    let paths = legacy_folder_paths(records)?;
    for r in records {
        convert_legacy_folder(r, &paths);
    }
    Ok(())
}

/// v28 metadata-only upgrade. Keep revisions, baselines and both conflict
/// versions; never scan or duplicate article bodies in the 1.3 GB library.
pub(crate) fn upgrade_folder_scope(conn: &mut Connection) -> AppResult<()> {
    let pending: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM webdav_folder_scope_upgrade)",
        [],
        |r| r.get(0),
    )?;
    if !pending {
        return Ok(());
    }
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut local = Vec::new();
    visit(
        &tx,
        &format!("SELECT {RECORD_JSON} FROM webdav_records WHERE kind IN ('folder','feed')"),
        |r| {
            local.push(r);
            Ok(())
        },
    )?;
    let paths = legacy_folder_paths(&local)?;
    // Save the original metadata for diagnosing migrations without article/secret data.
    tx.execute_batch("CREATE TABLE IF NOT EXISTS webdav_folder_migration_backup(kind TEXT,key TEXT,record TEXT,PRIMARY KEY(kind,key));")?;
    for r in &local {
        tx.execute(
            "INSERT OR IGNORE INTO webdav_folder_migration_backup VALUES(?1,?2,?3)",
            params![
                r.kind,
                r.key,
                serde_json::to_string(r).map_err(|_| invalid())?
            ],
        )?;
    }
    for kind in ["folder", "feed"] {
        for event in ["INSERT", "UPDATE", "DELETE"] {
            tx.execute_batch(&format!("DROP TRIGGER webdav_{kind}_{event};"))?;
        }
    }
    for r in &mut local {
        convert_legacy_folder(r, &paths);
    }
    tx.execute(
        "DELETE FROM webdav_records WHERE kind IN ('folder','feed')",
        [],
    )?;
    for r in &local {
        tx.execute(
            "INSERT INTO webdav_records VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                r.kind,
                r.key,
                r.clock,
                r.nonce,
                r.deleted,
                r.data.to_string()
            ],
        )?;
    }
    for table in ["webdav_baseline", "webdav_observed"] {
        let mut history = Vec::new();
        visit(
            &tx,
            &format!("SELECT record FROM {table} WHERE kind IN ('folder','feed')"),
            |r| {
                history.push(r);
                Ok(())
            },
        )?;
        let mut mapping = paths.clone();
        mapping.extend(legacy_folder_paths(&history)?);
        tx.execute(
            &format!("DELETE FROM {table} WHERE kind IN ('folder','feed')"),
            [],
        )?;
        for mut r in history {
            convert_legacy_folder(&mut r, &mapping);
            tx.execute(
                &format!("INSERT INTO {table} VALUES(?1,?2,?3)"),
                params![
                    r.kind,
                    r.key,
                    serde_json::to_string(&r).map_err(|_| invalid())?
                ],
            )?;
        }
    }
    let conflicts = conflicts(&tx)?;
    for c in conflicts
        .into_iter()
        .filter(|c| ["folder", "feed"].contains(&c.kind.as_str()))
    {
        let mut l = c.local;
        let mut r = c.remote;
        convert_legacy_folder(&mut l, &paths);
        convert_legacy_folder(&mut r, &paths);
        if r.kind == "folder" {
            let mut names = r.data["parent_id"]
                .as_str()
                .map(|s| serde_json::from_str::<Vec<String>>(s))
                .transpose()
                .map_err(|_| invalid())?
                .unwrap_or_default();
            names.push(
                r.data["name"]
                    .as_str()
                    .ok_or_else(invalid)?
                    .to_ascii_lowercase(),
            );
            r.key = serde_json::to_string(&names).map_err(|_| invalid())?;
        }
        tx.execute(
            "DELETE FROM webdav_conflicts WHERE kind=?1 AND key=?2",
            params![c.kind, c.key],
        )?;
        store_conflict(&tx, &l, &r, &c.reason)?;
    }
    // A parent moved before upgrading can change descendants' natural keys
    // without changing their old basename-based revisions. Preserve that edit.
    let mut base = Vec::new();
    visit(
        &tx,
        "SELECT record FROM webdav_baseline WHERE kind IN ('folder','feed')",
        |r| {
            base.push(r);
            Ok(())
        },
    )?;
    for r in &local {
        if let Some(old) = base.iter().find(|b| {
            b.kind == r.kind
                && (b.key == r.key || (r.kind == "folder" && b.data["name"] == r.data["name"]))
        }) {
            if !same_value(r, old) || r.key != old.key {
                tx.execute("UPDATE webdav_control SET clock=clock+1 WHERE id=1", [])?;
                let clock: i64 =
                    tx.query_row("SELECT clock FROM webdav_control", [], |r| r.get(0))?;
                tx.execute("UPDATE webdav_records SET clock=?3,nonce=lower(hex(randomblob(16))) WHERE kind=?1 AND key=?2",params![r.kind,r.key,clock])?;
                if r.kind == "folder" && r.key != old.key {
                    tx.execute(
                        "INSERT OR IGNORE INTO webdav_records VALUES(?1,?2,?3,?4,1,?5)",
                        params![
                            old.kind,
                            old.key,
                            clock,
                            probe_id(&tx)?,
                            old.data.to_string()
                        ],
                    )?;
                }
            }
        }
    }
    tx.execute(
        "DELETE FROM webdav_incoming WHERE kind IN ('folder','feed')",
        [],
    )?;
    tx.execute("DELETE FROM webdav_incoming_chunks WHERE CASE WHEN json_valid(descriptor) THEN json_extract(descriptor,'$.kind') END IN ('folder','feed')",[])?;
    tx.execute(
        "DELETE FROM webdav_buckets WHERE kind IN ('folder','feed')",
        [],
    )?;
    tx.execute_batch(&tracking_sql(
        current_specs()
            .into_iter()
            .filter(|s| ["folder", "feed"].contains(&s.kind))
            .collect(),
        true,
    ))?;
    for event in ["UPDATE", "DELETE"] {
        let action = if event == "UPDATE" {
            "UPDATE OF name,parent_id"
        } else {
            "DELETE"
        };
        tx.execute_batch(&format!("CREATE TRIGGER webdav_folder_tree_{event} AFTER {action} ON folders WHEN (SELECT paused FROM webdav_control WHERE id=1)=0 BEGIN {} END;",repair_folder_sql()))?;
    }
    tx.execute("DELETE FROM webdav_folder_scope_upgrade", [])?;
    tx.commit()?;
    Ok(())
}

pub fn feed_retention_schema() -> &'static str {
    static SQL: LazyLock<String> = LazyLock::new(|| {
        let mut sql=String::from("ALTER TABLE feeds ADD COLUMN max_articles INTEGER CHECK(max_articles IS NULL OR max_articles=100); UPDATE webdav_records SET data=json_set(data,'$.max_articles',NULL) WHERE kind='feed';");
        for event in ["INSERT", "UPDATE", "DELETE"] {
            sql.push_str(&format!("DROP TRIGGER webdav_feed_{event};"));
        }
        sql.push_str(&tracking_sql(
            current_specs()
                .into_iter()
                .filter(|s| s.kind == "feed")
                .collect(),
            true,
        ));
        sql
    });
    SQL.as_str()
}

/// v26: two logical preference records. Provider/model/effort travel together;
/// prompt edits can merge independently. Triggers cover CLI and reset/delete.
pub fn ai_preferences_schema() -> &'static str {
    static SQL: LazyLock<String> = LazyLock::new(|| {
        let mut sql = String::new();
        for (kind, key, keys, data) in [
            ("ai_config", "selection", "'ai_provider','ai_model','ai_reasoning_effort_codex'",
             "json_object('provider',coalesce((SELECT value FROM settings WHERE key='ai_provider'),'anthropic'),'model',coalesce((SELECT value FROM settings WHERE key='ai_model'),''),'effort',coalesce((SELECT value FROM settings WHERE key='ai_reasoning_effort_codex'),''))"),
            ("ai_prompt", "summary", "'ai_summary_prompt'",
             "json_object('prompt',coalesce((SELECT value FROM settings WHERE key='ai_summary_prompt'),''))"),
        ] {
            // An untouched installation has revision zero, so its defaults
            // cannot overwrite a configured device or create a first-sync conflict.
            let configured = format!("EXISTS(SELECT 1 FROM settings WHERE key IN ({keys}) AND value!='')");
            sql.push_str(&format!("INSERT INTO webdav_records VALUES('{kind}','{key}',CASE WHEN {configured} THEN CAST(unixepoch('subsec')*1000 AS INTEGER) ELSE 0 END,CASE WHEN {configured} THEN lower(hex(randomblob(16))) ELSE '' END,0,{data});"));
            for (event, row) in [("INSERT", "new"), ("UPDATE", "new"), ("DELETE", "old")] {
                sql.push_str(&format!("CREATE TRIGGER webdav_{kind}_{event} AFTER {event} ON settings
                    WHEN (SELECT paused FROM webdav_control WHERE id=1)=0 AND {row}.key IN ({keys})
                    AND (SELECT data FROM webdav_records WHERE kind='{kind}' AND key='{key}') IS NOT {data}
                    BEGIN UPDATE webdav_control SET clock=max(clock+1,CAST(unixepoch('subsec')*1000 AS INTEGER)) WHERE id=1;
                    INSERT INTO webdav_records VALUES('{kind}','{key}',(SELECT clock FROM webdav_control WHERE id=1),lower(hex(randomblob(16))),0,{data})
                    ON CONFLICT(kind,key) DO UPDATE SET clock=excluded.clock,nonce=excluded.nonce,data=excluded.data; END;"));
            }
        }
        sql
    });
    SQL.as_str()
}
fn repair_tag_sql() -> String {
    let mut sql=String::from("UPDATE webdav_control SET clock=max(clock+1,CAST(unixepoch('subsec')*1000 AS INTEGER)) WHERE id=1;");
    for spec in current_specs()
        .into_iter()
        .filter(|s| ["tag", "article_tag"].contains(&s.kind))
    {
        let data = payload(&spec);
        sql.push_str(&format!("UPDATE webdav_records SET deleted=1,clock=(SELECT clock FROM webdav_control WHERE id=1),nonce=lower(hex(randomblob(16))) WHERE kind='{kind}' AND deleted=0 AND key NOT IN (SELECT {key} FROM {table} t WHERE {filter});
            INSERT INTO webdav_records SELECT '{kind}',{key},(SELECT clock FROM webdav_control WHERE id=1),lower(hex(randomblob(16))),0,{data} FROM {table} t WHERE {filter}
            ON CONFLICT(kind,key) DO UPDATE SET clock=excluded.clock,nonce=excluded.nonce,deleted=0,data=excluded.data WHERE webdav_records.deleted!=0 OR webdav_records.data IS NOT excluded.data;",kind=spec.kind,key=spec.key,table=spec.table,filter=spec.filter));
    }
    sql
}
/// v25: sibling names are unique; identical basenames in other branches are legal.
pub fn scoped_tag_schema() -> &'static str {
    static SQL: LazyLock<String> = LazyLock::new(|| {
        let mut sql = String::from("UPDATE webdav_control SET paused=1 WHERE id=1;");
        for spec in specs() {
            for event in ["INSERT", "UPDATE", "DELETE"] {
                sql.push_str(&format!("DROP TRIGGER webdav_{}_{event};", spec.kind));
            }
        }
        sql.push_str("CREATE TABLE tag_memberships_upgrade AS SELECT * FROM article_tags;
            DROP TABLE article_tags;
            DROP TRIGGER tags_cycle_insert; DROP TRIGGER tags_cycle_update;
            ALTER TABLE tags RENAME TO tags_legacy;
            CREATE TABLE tags(id INTEGER PRIMARY KEY,name TEXT NOT NULL,color TEXT NOT NULL DEFAULT 'clay',position INTEGER NOT NULL DEFAULT 0,parent_id INTEGER REFERENCES tags(id) ON DELETE SET NULL);
            INSERT INTO tags SELECT id,name,color,position,parent_id FROM tags_legacy;
            DROP TABLE tags_legacy;
            CREATE UNIQUE INDEX idx_tags_sibling_name ON tags(coalesce(parent_id,0),name COLLATE NOCASE);
            CREATE INDEX idx_tags_parent ON tags(parent_id);
            CREATE TRIGGER tags_cycle_insert BEFORE INSERT ON tags WHEN new.parent_id IS NOT NULL AND new.parent_id=new.id BEGIN SELECT RAISE(ABORT,'tagCycle'); END;
            CREATE TRIGGER tags_cycle_update BEFORE UPDATE OF parent_id ON tags WHEN new.parent_id IS NOT NULL AND EXISTS(WITH RECURSIVE subtree(id) AS (SELECT new.id UNION SELECT p.id FROM tags p JOIN subtree c ON p.parent_id=c.id) SELECT 1 FROM subtree WHERE id=new.parent_id) BEGIN SELECT RAISE(ABORT,'tagCycle'); END;
            CREATE TABLE article_tags(article_id INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,tag_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,PRIMARY KEY(article_id,tag_id));
            INSERT INTO article_tags SELECT * FROM tag_memberships_upgrade;
            DROP TABLE tag_memberships_upgrade;
            CREATE INDEX idx_article_tags_tag ON article_tags(tag_id);
            CREATE TABLE webdav_observed(kind TEXT NOT NULL,key TEXT NOT NULL,record TEXT NOT NULL,PRIMARY KEY(kind,key));
            CREATE TABLE tag_key_upgrade(old_key TEXT PRIMARY KEY,new_key TEXT NOT NULL);
            INSERT INTO tag_key_upgrade WITH RECURSIVE c(original,parent_key,names,depth) AS (
                SELECT key,json_extract(data,'$.parent_id'),json_array(key),0 FROM webdav_records WHERE kind='tag'
                UNION ALL SELECT c.original,json_extract(p.data,'$.parent_id'),json_insert(c.names,'$[#]',p.key),c.depth+1 FROM c JOIN webdav_records p ON p.kind='tag' AND p.key=c.parent_key WHERE c.depth<100
            ) SELECT original,(SELECT json_group_array(value) FROM (SELECT value FROM json_each(c.names) ORDER BY key DESC)) FROM c WHERE parent_key IS NULL OR NOT EXISTS(SELECT 1 FROM webdav_records p WHERE p.kind='tag' AND p.key=c.parent_key);
            INSERT OR IGNORE INTO tag_key_upgrade SELECT key,json_array(key) FROM webdav_records WHERE kind='tag';
            CREATE TABLE tag_records_upgrade AS SELECT kind,key,clock,nonce,deleted,data FROM webdav_records WHERE kind IN ('tag','article_tag');
            DELETE FROM webdav_records WHERE kind IN ('tag','article_tag');
            INSERT INTO webdav_records SELECT kind,(SELECT new_key FROM tag_key_upgrade WHERE old_key=r.key),clock,nonce,deleted,json_set(data,'$.parent_id',(SELECT new_key FROM tag_key_upgrade WHERE old_key=json_extract(r.data,'$.parent_id'))) FROM tag_records_upgrade r WHERE kind='tag';
            INSERT INTO webdav_records SELECT kind,json_array(json_extract(data,'$.article_id'),coalesce((SELECT new_key FROM tag_key_upgrade WHERE old_key=json_extract(r.data,'$.tag_id')),json_array(json_extract(data,'$.tag_id')))),clock,nonce,deleted,json_set(data,'$.tag_id',coalesce((SELECT new_key FROM tag_key_upgrade WHERE old_key=json_extract(r.data,'$.tag_id')),json_array(json_extract(data,'$.tag_id')))) FROM tag_records_upgrade r WHERE kind='article_tag';
            DROP TABLE tag_records_upgrade; DROP TABLE tag_key_upgrade;
            DELETE FROM webdav_baseline; DELETE FROM webdav_conflicts;");
        sql.push_str(&tracking_sql(tag_specs_v25(), true));
        for event in ["UPDATE", "DELETE"] {
            let action = if event == "UPDATE" {
                "UPDATE OF name,parent_id"
            } else {
                "DELETE"
            };
            sql.push_str(&format!("CREATE TRIGGER webdav_tag_tree_{event} AFTER {action} ON tags WHEN (SELECT paused FROM webdav_control WHERE id=1)=0 BEGIN {} END;",repair_tag_sql()));
        }
        sql.push_str("UPDATE webdav_control SET paused=0 WHERE id=1;");
        sql
    });
    SQL.as_str()
}
/// Convert legacy globally named tag records before three-way comparison.
pub fn upgrade_legacy_tags(records: &mut [Record]) -> AppResult<()> {
    let tags: std::collections::BTreeMap<_, _> = records
        .iter()
        .filter(|r| r.kind == "tag")
        .map(|r| {
            (
                r.key.clone(),
                r.data["parent_id"].as_str().map(str::to_owned),
            )
        })
        .collect();
    let path = |name: &str| -> AppResult<String> {
        let mut names = Vec::new();
        let mut current = Some(name.to_owned());
        while let Some(name) = current {
            if names.contains(&name) || names.len() > 100 {
                return Err(invalid());
            }
            current = tags.get(&name).cloned().flatten();
            names.push(name);
        }
        names.reverse();
        serde_json::to_string(&names).map_err(|_| invalid())
    };
    for r in records {
        if r.kind == "tag" {
            r.key = path(&r.key)?;
            r.data["parent_id"] = r.data["parent_id"]
                .as_str()
                .map(|p| path(p).map(Value::String))
                .transpose()?
                .unwrap_or(Value::Null);
        }
        if r.kind == "article_tag" {
            let tag = path(r.data["tag_id"].as_str().ok_or_else(invalid)?)?;
            r.key = serde_json::json!([r.data["article_id"], tag]).to_string();
            r.data["tag_id"] = Value::String(tag);
        }
    }
    Ok(())
}

fn payload(spec: &Spec) -> String {
    format!(
        "json_object({})",
        spec.columns
            .iter()
            .map(|(col, expr)| format!("'{col}',{expr}"))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// One append-only migration, including the initial snapshot of existing data.
pub fn schema() -> &'static str {
    static SQL: LazyLock<String> = LazyLock::new(|| {
        let mut sql = String::from(
            "ALTER TABLE articles ADD COLUMN reading_progress REAL NOT NULL DEFAULT 0;
             CREATE TABLE webdav_control(id INTEGER PRIMARY KEY CHECK(id=1), paused INTEGER NOT NULL, clock INTEGER NOT NULL);
             INSERT INTO webdav_control VALUES(1,0,0);
             CREATE TABLE webdav_records(kind TEXT NOT NULL,key TEXT NOT NULL,clock INTEGER NOT NULL,nonce TEXT NOT NULL,deleted INTEGER NOT NULL,data TEXT NOT NULL,PRIMARY KEY(kind,key));"
        );
        // v22 predates folder hierarchy. Keep its generated schema stable.
        let legacy = specs()
            .into_iter()
            .map(|mut spec| {
                if ["folder", "tag"].contains(&spec.kind) {
                    spec.columns.retain(|(c, _)| *c != "parent_id");
                }
                spec
            })
            .collect();
        sql.push_str(&tracking_sql(legacy, false));
        sql
    });
    SQL.as_str()
}

fn tracking_sql(tracking_specs: Vec<Spec>, modern: bool) -> String {
    // Explicit UPSERT remains effective inside SQLite foreign-key actions,
    // whose implicit UPDATE policy can override INSERT OR REPLACE in triggers.
    let insert = if modern {
        "INSERT"
    } else {
        "INSERT OR REPLACE"
    };
    let upsert = if modern {
        " ON CONFLICT(kind,key) DO UPDATE SET clock=excluded.clock,nonce=excluded.nonce,deleted=excluded.deleted,data=excluded.data"
    } else {
        ""
    };
    let seed_conflict = if modern {
        " ON CONFLICT(kind,key) DO NOTHING"
    } else {
        ""
    };
    let mut sql = String::new();
    for spec in tracking_specs {
        let data = payload(&spec);
        sql.push_str(&format!("{insert} INTO webdav_records SELECT '{kind}',{key},CAST(unixepoch('subsec')*1000 AS INTEGER),'',0,{data} FROM {table} t WHERE {filter}{seed_conflict};",
                kind=spec.kind,key=spec.key,table=spec.table,filter=spec.filter));
        for (event, row) in [("INSERT", "new"), ("UPDATE", "new"), ("DELETE", "old")] {
            let key = spec.key.replace("t.", &format!("{row}."));
            let data = data.replace("t.", &format!("{row}."));
            let filter = spec.filter.replace("t.", &format!("{row}."));
            let changed = spec
                .columns
                .iter()
                .map(|(c, _)| format!("old.{c} IS NOT new.{c}"))
                .collect::<Vec<_>>()
                .join(" OR ");
            let when = if event == "UPDATE" {
                format!(" AND ({changed})")
            } else {
                String::new()
            };
            // Defaults on a newly fetched article must never undo an older
            // explicit read/star/later/progress edit from another device.
            let baseline = spec.state && event == "INSERT";
            let clock = if baseline {
                "0"
            } else {
                "(SELECT clock FROM webdav_control WHERE id=1)"
            };
            let nonce = if baseline {
                "''"
            } else {
                "lower(hex(randomblob(16)))"
            };
            let deleted = i32::from(event == "DELETE");
            sql.push_str(&format!(
                    "CREATE TRIGGER webdav_{kind}_{event} {timing} {event} ON {table}
                     WHEN (SELECT paused FROM webdav_control WHERE id=1)=0 AND ({filter}){when} BEGIN
                     UPDATE webdav_control SET clock=max(clock+1,CAST(unixepoch('subsec')*1000 AS INTEGER)) WHERE id=1;
                     {insert} INTO webdav_records VALUES('{kind}',{key},{clock},{nonce},{deleted},{data}){upsert};",
                    kind=spec.kind,table=spec.table,timing=if event == "DELETE" { "BEFORE" } else { "AFTER" }));
            if event == "UPDATE" {
                let old_key = spec.key.replace("t.", "old.");
                let old_data = payload(&spec).replace("t.", "old.");
                sql.push_str(&format!(
                        "{insert} INTO webdav_records SELECT '{kind}',{old_key},{clock},{nonce},1,{old_data} WHERE {old_key} IS NOT {key}{upsert};",kind=spec.kind));
            }
            // Parent rows disappear before SQLite runs cascaded child
            // triggers. Tombstone their records while natural keys still
            // resolve; otherwise a later sync could resurrect the children.
            if event == "DELETE" {
                let condition = match spec.kind {
                        "feed" => Some("(kind='rule' AND json_extract(data,'$.feed_id')=old.feed_url) OR (kind IN ('article','retention','is_read','is_starred','read_later','reading_progress') AND json_extract(key,'$[0]')=old.feed_url) OR (kind IN ('enclosure','article_tag','highlight') AND json_extract(json_extract(data,'$.article_id'),'$[0]')=old.feed_url)".to_string()),
                        "article" => Some(format!("kind IN ('is_read','is_starred','read_later','reading_progress') AND key={key} OR kind IN ('enclosure','article_tag','highlight') AND json_extract(data,'$.article_id')={key}")),
                        "tag" => Some(format!("kind='article_tag' AND json_extract(data,'$.tag_id')={}",spec.key.replace("t.","old."))),
                        _ => None,
                    };
                if let Some(condition) = condition {
                    sql.push_str(&format!("UPDATE webdav_records SET deleted=1,clock={clock},nonce={nonce} WHERE {condition};"));
                }
            }
            // Renaming a natural parent key also changes the references
            // encoded in its children, even though their local IDs stay put.
            if event == "UPDATE" && ["folder", "tag"].contains(&spec.kind) {
                if spec.kind == "folder" && spec.key == "lower(t.name)" {
                    sql.push_str(&format!("UPDATE webdav_records SET data=json_set(data,'$.folder_id',lower(new.name)),clock={clock},nonce={nonce} WHERE kind='feed' AND deleted=0 AND json_extract(data,'$.folder_id')=lower(old.name);"));
                    if spec.columns.iter().any(|(c, _)| *c == "parent_id") {
                        sql.push_str(&format!("UPDATE webdav_records SET data=json_set(data,'$.parent_id',lower(new.name)),clock={clock},nonce={nonce} WHERE kind='folder' AND deleted=0 AND json_extract(data,'$.parent_id')=lower(old.name);"));
                    }
                } else if spec.key == "lower(t.name)" {
                    if spec.columns.iter().any(|(c, _)| *c == "parent_id") {
                        sql.push_str(&format!("UPDATE webdav_records SET data=json_set(data,'$.parent_id',lower(new.name)),clock={clock},nonce={nonce} WHERE kind='tag' AND deleted=0 AND json_extract(data,'$.parent_id')=lower(old.name);"));
                    }
                    sql.push_str(&format!("{insert} INTO webdav_records SELECT kind,json_array(json_extract(data,'$.article_id'),lower(new.name)),{clock},{nonce},0,json_set(data,'$.tag_id',lower(new.name)) FROM webdav_records WHERE kind='article_tag' AND deleted=0 AND json_extract(data,'$.tag_id')=lower(old.name) AND lower(old.name)!=lower(new.name){upsert};"));
                    sql.push_str(&format!("UPDATE webdav_records SET deleted=1,clock={clock},nonce={nonce} WHERE kind='article_tag' AND deleted=0 AND json_extract(data,'$.tag_id')=lower(old.name) AND lower(old.name)!=lower(new.name);"));
                }
            }
            sql.push_str("END;");
        }
    }
    sql
}

/// v23: nested folders, with database-level protection against cycles.
pub fn folder_schema() -> &'static str {
    static SQL: LazyLock<String> = LazyLock::new(|| {
        let mut sql = String::from("ALTER TABLE folders ADD COLUMN parent_id INTEGER REFERENCES folders(id) ON DELETE SET NULL;
            CREATE INDEX idx_folders_parent ON folders(parent_id);
            CREATE TRIGGER folders_cycle_insert BEFORE INSERT ON folders WHEN new.parent_id IS NOT NULL AND new.parent_id=new.id BEGIN SELECT RAISE(ABORT,'folderCycle'); END;
            CREATE TRIGGER folders_cycle_update BEFORE UPDATE OF parent_id ON folders WHEN new.parent_id IS NOT NULL AND EXISTS(WITH RECURSIVE subtree(id) AS (SELECT new.id UNION SELECT f.id FROM folders f JOIN subtree s ON f.parent_id=s.id) SELECT 1 FROM subtree WHERE id=new.parent_id) BEGIN SELECT RAISE(ABORT,'folderCycle'); END;
            UPDATE webdav_records SET data=json_set(data,'$.parent_id',NULL) WHERE kind='folder';");
        for spec in specs() {
            for event in ["INSERT", "UPDATE", "DELETE"] {
                sql.push_str(&format!("DROP TRIGGER webdav_{}_{event};", spec.kind));
            }
        }
        sql.push_str(&tracking_sql(
            specs()
                .into_iter()
                .map(|mut spec| {
                    if spec.kind == "tag" {
                        spec.columns.retain(|(c, _)| *c != "parent_id");
                    }
                    spec
                })
                .collect(),
            true,
        ));
        sql
    });
    SQL.as_str()
}

/// v24: nested tags and local three-way sync conflict tracking.
pub fn tag_schema() -> &'static str {
    static SQL: LazyLock<String> = LazyLock::new(|| {
        let mut sql = String::from("ALTER TABLE tags ADD COLUMN parent_id INTEGER REFERENCES tags(id) ON DELETE SET NULL;
            CREATE INDEX idx_tags_parent ON tags(parent_id);
            CREATE TRIGGER tags_cycle_insert BEFORE INSERT ON tags WHEN new.parent_id IS NOT NULL AND new.parent_id=new.id BEGIN SELECT RAISE(ABORT,'tagCycle'); END;
            CREATE TRIGGER tags_cycle_update BEFORE UPDATE OF parent_id ON tags WHEN new.parent_id IS NOT NULL AND EXISTS(WITH RECURSIVE subtree(id) AS (SELECT new.id UNION SELECT t.id FROM tags t JOIN subtree s ON t.parent_id=s.id) SELECT 1 FROM subtree WHERE id=new.parent_id) BEGIN SELECT RAISE(ABORT,'tagCycle'); END;
            UPDATE webdav_records SET data=json_set(data,'$.parent_id',NULL) WHERE kind='tag';
            CREATE TABLE webdav_baseline(kind TEXT NOT NULL,key TEXT NOT NULL,record TEXT NOT NULL,PRIMARY KEY(kind,key));
            CREATE TABLE webdav_conflicts(kind TEXT NOT NULL,key TEXT NOT NULL,local_record TEXT NOT NULL,remote_record TEXT NOT NULL,reason TEXT NOT NULL,PRIMARY KEY(kind,key));");
        for spec in specs() {
            for event in ["INSERT", "UPDATE", "DELETE"] {
                sql.push_str(&format!("DROP TRIGGER webdav_{}_{event};", spec.kind));
            }
        }
        sql.push_str(&tracking_sql(specs(), true));
        sql
    });
    SQL.as_str()
}

/// Random ID for a disposable connection-test resource.
pub fn probe_id(conn: &Connection) -> AppResult<String> {
    Ok(conn.query_row("SELECT lower(hex(randomblob(16)))", [], |r| r.get(0))?)
}

pub fn records(conn: &Connection) -> AppResult<Vec<Record>> {
    let mut stmt = conn.prepare(
        "SELECT kind,key,clock,nonce,deleted,data FROM webdav_records ORDER BY kind,key",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get::<_, String>(5)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (kind, key, clock, nonce, deleted, data) = row?;
        out.push(Record {
            kind,
            key,
            clock,
            nonce,
            deleted,
            data: serde_json::from_str(&data).map_err(|_| AppError::code("webdavInvalidData"))?,
        });
    }
    Ok(out)
}

fn invalid() -> AppError {
    AppError::code("webdavInvalidData")
}

fn reference(kind: &str, column: &str, value: &str) -> String {
    let (table, key) = match column {
        "parent_id" if kind == "tag" => ("tags", tag_key("t")),
        "parent_id" | "folder_id" => ("folders", folder_key("t")),
        "feed_id" => ("feeds", "t.feed_url".to_string()),
        "tag_id" => ("tags", tag_key("t")),
        "article_id" => (
            "articles",
            "json_array((SELECT feed_url FROM feeds WHERE id=t.feed_id),t.guid)".to_string(),
        ),
        _ => return value.to_string(),
    };
    format!("(SELECT t.id FROM {table} t WHERE {key}={value})")
}

/// Validate before touching SQLite. The column allowlist is also the protocol
/// boundary: a remote payload cannot write arbitrary settings, credentials or SQL.
fn validate(record: &Record, spec: &Spec) -> AppResult<()> {
    let object = record.data.as_object().ok_or_else(invalid)?;
    if record.clock < 0
        || record.clock > i64::MAX - 1_000_000
        || record.nonce.len() > 64
        || !record.nonce.bytes().all(|c| c.is_ascii_hexdigit())
        || record.key.is_empty()
        || object.len() != spec.columns.len()
    {
        return Err(invalid());
    }
    for (col, _) in &spec.columns {
        let value = object.get(*col).ok_or_else(invalid)?;
        if !(value.is_null() || value.is_string() || value.is_number()) {
            return Err(invalid());
        }
    }
    if spec.kind == "ai_config" || spec.kind == "ai_prompt" {
        if record.deleted
            || record.key
                != if spec.kind == "ai_config" {
                    "selection"
                } else {
                    "summary"
                }
            || object.values().any(|v| !v.is_string())
        {
            return Err(invalid());
        }
        if spec.kind == "ai_config" {
            if !matches!(
                object["provider"].as_str(),
                Some("anthropic" | "openai" | "deepseek" | "codex")
            ) || object["model"].as_str().unwrap().len() > 512
                || !matches!(
                    object["effort"].as_str(),
                    Some(
                        "" | "none"
                            | "minimal"
                            | "low"
                            | "medium"
                            | "high"
                            | "xhigh"
                            | "max"
                            | "ultra"
                    )
                )
            {
                return Err(invalid());
            }
        } else if object["prompt"].as_str().unwrap().chars().count() > 12000 {
            return Err(invalid());
        }
    }
    if spec.state {
        let value = &object[spec.columns[0].0];
        if spec.kind == "reading_progress" {
            if !value.as_f64().is_some_and(|v| (0.0..=1.0).contains(&v)) {
                return Err(invalid());
            }
        } else if !matches!(value.as_i64(), Some(0 | 1)) {
            return Err(invalid());
        }
    }
    if spec.kind == "feed" {
        if !(object["max_articles"].is_null() || object["max_articles"].as_i64() == Some(100)) {
            return Err(invalid());
        }
        let url = object["feed_url"].as_str().ok_or_else(invalid)?;
        if !url::Url::parse(url).is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
            || object["source_type"] == "newsletter"
        {
            return Err(invalid());
        }
    }
    Ok(())
}

pub(crate) fn validate_record(record: &Record) -> AppResult<()> {
    let r = normalized(record.clone());
    let specs = current_specs();
    validate(
        &r,
        specs
            .iter()
            .find(|s| s.kind == r.kind)
            .ok_or_else(invalid)?,
    )
}

fn newer(remote: &Record, local: &Record) -> bool {
    (
        remote.clock,
        &remote.nonce,
        remote.deleted,
        remote.data.to_string(),
    ) > (
        local.clock,
        &local.nonce,
        local.deleted,
        local.data.to_string(),
    )
}

/// Merge atomically. Foreign IDs resolve from stable feed URL / GUID keys.
/// Per-field state records let independent read and star edits both survive.
pub fn merge(conn: &mut Connection, incoming: &[Record]) -> AppResult<usize> {
    let tx = conn.transaction()?;
    let changed = merge_rows(&tx, incoming)?;
    tx.commit()?;
    Ok(changed)
}

// Input lives on disk, so a multi-gigabyte library never becomes a Vec<Record>.
const RECORD_JSON: &str = "json_object('kind',kind,'key',key,'clock',clock,'nonce',nonce,'deleted',json(CASE deleted WHEN 1 THEN 'true' ELSE 'false' END),'data',json(data))";

pub fn record_json_sql() -> &'static str {
    RECORD_JSON
}

fn lookup(conn: &Connection, kind: &str, key: &str) -> AppResult<Option<Record>> {
    let json: Option<String> = conn
        .query_row(
            &format!("SELECT {RECORD_JSON} FROM webdav_records WHERE kind=?1 AND key=?2"),
            params![kind, key],
            |r| r.get(0),
        )
        .optional()?;
    json.map(|s| serde_json::from_str(&s).map_err(|_| invalid()))
        .transpose()
}

fn visit(
    conn: &Connection,
    sql: &str,
    mut f: impl FnMut(Record) -> AppResult<()>,
) -> AppResult<()> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let json: String = row.get(0)?;
        f(normalized(
            serde_json::from_str(&json).map_err(|_| invalid())?,
        ))?;
    }
    Ok(())
}

fn stage_slice(conn: &Connection, incoming: &[Record]) -> AppResult<()> {
    conn.execute_batch("CREATE TEMP TABLE IF NOT EXISTS webdav_input(kind TEXT,key TEXT,record TEXT,PRIMARY KEY(kind,key)); DELETE FROM webdav_input;")?;
    for r in incoming {
        validate_record(r)?;
        conn.execute(
            "INSERT INTO webdav_input VALUES(?1,?2,?3)",
            params![
                r.kind,
                r.key,
                serde_json::to_string(r).map_err(|_| invalid())?
            ],
        )?;
    }
    Ok(())
}

fn merge_rows(conn: &Connection, incoming: &[Record]) -> AppResult<usize> {
    stage_slice(conn, incoming)?;
    merge_table(conn, "webdav_input")
}

/// A cleanup on another device must not discard a local collection or annotation.
/// Protection wins automatically; unrelated concurrent edits still use review.
fn preserve_protected_articles(conn: &Connection, table: &str) -> AppResult<()> {
    let sql=format!("SELECT r.record FROM {table} r JOIN feeds f ON f.feed_url=json_extract(r.key,'$[0]') JOIN articles a ON a.feed_id=f.id AND a.guid=json_extract(r.key,'$[1]') LEFT JOIN webdav_records l ON l.kind='retention' AND l.key=r.key WHERE r.kind='retention' AND json_extract(r.record,'$.deleted')=0 AND (l.key IS NULL OR json_extract(r.record,'$.clock')>l.clock OR (json_extract(r.record,'$.clock')=l.clock AND json_extract(r.record,'$.nonce')>l.nonce)) AND (a.is_starred=1 OR a.read_later=1 OR EXISTS(SELECT 1 FROM highlights h WHERE h.article_id=a.id))");
    let mut protected = Vec::new();
    visit(conn, &sql, |r| {
        protected.push(r);
        Ok(())
    })?;
    if protected.is_empty() {
        return Ok(());
    }
    let max_sql = if table == "webdav_incoming" {
        format!("SELECT coalesce(max(clock),0) FROM {table}")
    } else {
        format!("SELECT coalesce(max(json_extract(record,'$.clock')),0) FROM {table}")
    };
    let remote_clock: i64 = conn.query_row(&max_sql, [], |r| r.get(0))?;
    conn.execute(
        "UPDATE webdav_control SET clock=max(clock,?1)+1 WHERE id=1",
        [remote_clock],
    )?;
    let clock: i64 = conn.query_row("SELECT clock FROM webdav_control WHERE id=1", [], |r| {
        r.get(0)
    })?;
    for mut marker in protected {
        marker.deleted = true;
        marker.clock = clock;
        marker.nonce = probe_id(conn)?;
        conn.execute("DELETE FROM article_tombstones WHERE feed_id=(SELECT id FROM feeds WHERE feed_url=json_extract(?1,'$[0]')) AND guid=json_extract(?1,'$[1]')",[&marker.key])?;
        conn.execute(
            "INSERT OR REPLACE INTO webdav_records VALUES('retention',?1,?2,?3,1,?4)",
            params![marker.key, clock, marker.nonce, marker.data.to_string()],
        )?;
        conn.execute("UPDATE webdav_records SET clock=?2,nonce=?3 WHERE key=?1 AND kind IN ('article','is_read','is_starred','read_later','reading_progress') AND deleted=0",params![marker.key,clock,marker.nonce])?;
        conn.execute("UPDATE webdav_records SET clock=?2,nonce=?3 WHERE kind IN ('highlight','article_tag','enclosure') AND json_extract(data,'$.article_id')=?1 AND deleted=0",params![marker.key,clock,marker.nonce])?;
    }
    Ok(())
}

fn merge_table(conn: &Connection, table: &str) -> AppResult<usize> {
    preserve_protected_articles(conn, table)?;
    let specs = current_specs();
    conn.execute_batch("CREATE TEMP TABLE IF NOT EXISTS webdav_changed(kind TEXT,key TEXT,PRIMARY KEY(kind,key)); DELETE FROM webdav_changed;")?;
    let mut changed = 0;
    let filter = if table == "webdav_incoming" {
        " WHERE chunk IN (SELECT hash FROM webdav_incoming_chunks WHERE dirty=1) OR NOT EXISTS(SELECT 1 FROM webdav_records l WHERE l.kind=webdav_incoming.kind AND l.key=webdav_incoming.key AND (l.clock>webdav_incoming.clock OR (l.clock=webdav_incoming.clock AND l.nonce>=webdav_incoming.nonce)))"
    } else {
        ""
    };
    visit(
        conn,
        &format!("SELECT record FROM {table}{filter} ORDER BY kind,key"),
        |r| {
            validate(
                &r,
                specs
                    .iter()
                    .find(|s| s.kind == r.kind)
                    .ok_or_else(invalid)?,
            )?;
            if lookup(conn, &r.kind, &r.key)?
                .as_ref()
                .is_none_or(|old| newer(&r, old))
            {
                conn.execute(
                    "INSERT OR REPLACE INTO webdav_records VALUES(?1,?2,?3,?4,?5,?6)",
                    params![
                        r.kind,
                        r.key,
                        r.clock,
                        r.nonce,
                        r.deleted,
                        r.data.to_string()
                    ],
                )?;
                conn.execute(
                    "UPDATE webdav_control SET clock=max(clock,?1) WHERE id=1",
                    [r.clock],
                )?;
                conn.execute(
                    "INSERT INTO webdav_changed VALUES(?1,?2)",
                    params![r.kind, r.key],
                )?;
                changed += 1;
            }
            Ok(())
        },
    )?;
    if changed == 0 {
        return Ok(0);
    }
    conn.execute("UPDATE webdav_control SET paused=1 WHERE id=1", [])?;
    let structural: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM webdav_changed WHERE kind IN ('feed','retention'))",
        [],
        |r| r.get(0),
    )?;
    // Remove canceled purge markers before restoring the corresponding bodies.
    conn.execute("DELETE FROM article_tombstones AS t WHERE json_array((SELECT feed_url FROM feeds WHERE id=t.feed_id),t.guid) IN (SELECT key FROM webdav_records WHERE kind='retention' AND deleted=1)",[])?;
    for spec in &specs {
        let order = if ["tag", "folder"].contains(&spec.kind) {
            "json_array_length(key),key"
        } else {
            "key"
        };
        // Large, unchanged HTML is never re-imported or re-indexed on a flag edit.
        let filter = if spec.kind == "article" && !structural {
            " AND (key IN (SELECT key FROM webdav_changed WHERE kind='article') OR key IN (SELECT json_array(f.feed_url,a.guid) FROM articles a JOIN feeds f ON f.id=a.feed_id LEFT JOIN articles_fts x ON x.rowid=a.id WHERE x.rowid IS NULL))"
        } else {
            ""
        };
        visit(conn,&format!("SELECT {RECORD_JSON} FROM webdav_records WHERE kind='{}' AND deleted=0{filter} ORDER BY {order}",spec.kind),|r|apply(conn,spec,&r))?;
    }
    for spec in specs.iter().rev().filter(|s| !s.state) {
        let order = if ["tag", "folder"].contains(&spec.kind) {
            "json_array_length(key) DESC,key"
        } else {
            "key"
        };
        // Deletion only needs natural keys, never the retained article bodies.
        let mut stmt = conn.prepare(&format!(
            "SELECT key FROM webdav_records WHERE kind=?1 AND deleted=1 ORDER BY {order}"
        ))?;
        let mut rows = stmt.query([spec.kind])?;
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            if spec.kind == "tag" {
                let id: Option<i64> = conn
                    .query_row(
                        &format!("SELECT t.id FROM tags t WHERE {}=?1", spec.key),
                        [&key],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(id) = id {
                    super::delete_tag_rows(conn, id)?;
                }
            } else {
                conn.execute(
                    &format!("DELETE FROM {} AS t WHERE {}=?1", spec.table, spec.key),
                    [&key],
                )?;
            }
        }
    }
    conn.execute_batch(&repair_tag_sql())?;
    conn.execute_batch(&repair_folder_sql())?;
    conn.execute("UPDATE webdav_control SET paused=0 WHERE id=1", [])?;
    Ok(changed)
}

fn apply(conn: &Connection, spec: &Spec, record: &Record) -> AppResult<()> {
    if spec.kind == "ai_prompt" {
        return super::set_setting(
            conn,
            "ai_summary_prompt",
            record.data["prompt"].as_str().ok_or_else(invalid)?,
        );
    }
    if spec.kind == "ai_config" {
        let provider = record.data["provider"].as_str().ok_or_else(invalid)?;
        let model = record.data["model"].as_str().ok_or_else(invalid)?;
        let previous =
            super::get_setting(conn, "ai_provider")?.unwrap_or_else(|| "anthropic".into());
        // Endpoint profiles stay local, including URLs containing credentials.
        // On a remote provider switch, restore this device's matching endpoint.
        if previous != provider {
            for key in ["ai_model", "ai_base_url"] {
                let value = super::get_setting(conn, key)?.unwrap_or_default();
                super::set_setting(conn, &format!("{key}_{previous}"), &value)?;
            }
            let endpoint =
                super::get_setting(conn, &format!("ai_base_url_{provider}"))?.unwrap_or_default();
            super::set_setting(conn, "ai_base_url", &endpoint)?;
        }
        super::set_setting(conn, "ai_provider", provider)?;
        super::set_setting(conn, "ai_model", model)?;
        super::set_setting(conn, &format!("ai_model_{provider}"), model)?;
        return super::set_setting(
            conn,
            "ai_reasoning_effort_codex",
            record.data["effort"].as_str().ok_or_else(invalid)?,
        );
    }
    let mut data = record.data.clone();
    if spec.kind == "article" {
        let base = data["url"].as_str().map(str::to_string);
        for col in ["content_html", "extracted_html", "summary"] {
            if let Some(html) = data[col].as_str() {
                data[col] = Value::String(crate::sanitize::sanitize(html, base.as_deref()));
            }
        }
    }
    let json = data.to_string();
    let expressions: Vec<_> = spec
        .columns
        .iter()
        .map(|(c, _)| reference(spec.kind, c, &format!("json_extract(?2,'$.{c}')")))
        .collect();
    // Mandatory parents may have been concurrently deleted. Keep the record
    // for later reconciliation, without resurrecting its deleted parent.
    for ((col, _), expr) in spec.columns.iter().zip(&expressions) {
        if (["feed_id", "article_id", "tag_id"].contains(col)
            || (["tag", "folder"].contains(&spec.kind)
                && *col == "parent_id"
                && !data[*col].is_null()))
            && !(spec.kind == "rule" && *col == "feed_id" && data[*col].is_null())
        {
            let exists: bool = conn.query_row(
                &format!("SELECT {expr} IS NOT NULL, ?1"),
                params![record.key, json],
                |r| r.get(0),
            )?;
            if !exists {
                return Ok(());
            }
        }
    }
    // Reject mismatched keys, including payloads trying to alter another row.
    // Evaluate the key over a synthetic row with the resolved local IDs.
    if !spec.state {
        let fields = spec
            .columns
            .iter()
            .zip(&expressions)
            .map(|((c, _), e)| format!("{e} AS {c}"))
            .collect::<Vec<_>>()
            .join(",");
        let key: String = conn.query_row(
            &format!("SELECT {key},?1 FROM (SELECT {fields}) t", key = spec.key),
            params![record.key, json],
            |r| r.get(0),
        )?;
        if key != record.key {
            return Err(invalid());
        }
    }
    let select = format!(
        "SELECT rowid FROM {table} t WHERE {key}=?1",
        table = spec.table,
        key = spec.key
    );
    let id: Option<i64> = conn
        .query_row(&select, [&record.key], |r| r.get(0))
        .optional()?;
    if id.is_none() && spec.state {
        return Ok(());
    }
    if spec.kind == "article" {
        let retained: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM article_tombstones WHERE feed_id=(SELECT id FROM feeds WHERE feed_url=json_extract(?1,'$[0]')) AND guid=json_extract(?1,'$[1]'))",
            [&record.key], |r| r.get(0))?;
        if retained {
            return Ok(());
        }
    }
    if let Some(id) = id {
        let assignments = spec
            .columns
            .iter()
            .zip(&expressions)
            .map(|((c, _), e)| format!("{c}={e}"))
            .collect::<Vec<_>>()
            .join(",");
        conn.execute(
            &format!("UPDATE {} SET {assignments} WHERE rowid=?1", spec.table),
            params![id, json],
        )?;
    } else {
        let names = spec
            .columns
            .iter()
            .map(|(c, _)| *c)
            .collect::<Vec<_>>()
            .join(",");
        conn.execute(
            &format!(
                "INSERT INTO {}({names}) SELECT {} WHERE ?1 IS NOT NULL",
                spec.table,
                expressions.join(",")
            ),
            params![record.key, json],
        )?;
    }
    if spec.kind == "article" {
        let id: i64 = conn.query_row(&select, [&record.key], |r| r.get(0))?;
        conn.execute("DELETE FROM articles_fts WHERE rowid=?1", [id])?;
        conn.execute("INSERT INTO articles_fts(rowid,title,body) SELECT id,title,body_text FROM articles WHERE id=?1", [id])?;
    }
    Ok(())
}

/// Conflict payloads are local-only; neither losing version is discarded until
/// the user chooses. Baselines track observed downloads and successful uploads.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
    pub kind: String,
    pub key: String,
    pub local: Record,
    pub remote: Record,
    pub reason: String,
    pub title: String,
}
pub fn conflicts(conn: &Connection) -> AppResult<Vec<Conflict>> {
    let mut stmt = conn.prepare(
        "SELECT kind,key,local_record,remote_record,reason FROM webdav_conflicts ORDER BY kind,key",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (kind, key, l, r, reason) = row?;
        let title = if ["tag", "folder"].contains(&kind.as_str()) {
            serde_json::from_str::<Vec<String>>(&key)
                .map(|p| p.join(" / "))
                .unwrap_or_else(|_| key.clone())
        } else if let Ok(pair) = serde_json::from_str::<Vec<String>>(&key) {
            if pair.len() == 2 {
                conn.query_row("SELECT a.title FROM articles a JOIN feeds f ON f.id=a.feed_id WHERE f.feed_url=?1 AND a.guid=?2",params![pair[0],pair[1]],|r|r.get::<_,Option<String>>(0)).optional()?.flatten().unwrap_or_else(||key.clone())
            } else {
                key.clone()
            }
        } else {
            key.clone()
        };
        out.push(Conflict {
            kind,
            key,
            title,
            local: serde_json::from_str(&l).map_err(|_| invalid())?,
            remote: serde_json::from_str(&r).map_err(|_| invalid())?,
            reason,
        });
    }
    Ok(out)
}
fn same_value(a: &Record, b: &Record) -> bool {
    a.deleted == b.deleted && a.data == b.data
}
fn same_revision(a: &Record, b: &Record) -> bool {
    a.clock == b.clock && a.nonce == b.nonce && same_value(a, b)
}
fn normalized(mut r: Record) -> Record {
    if r.kind == "feed" {
        if let Some(o) = r.data.as_object_mut() {
            o.entry("max_articles").or_insert(Value::Null);
        }
    }
    if ["tag", "folder"].contains(&r.kind.as_str()) {
        if let Some(o) = r.data.as_object_mut() {
            o.entry("parent_id").or_insert(Value::Null);
        }
    }
    r
}
fn store_conflict(
    conn: &Connection,
    local: &Record,
    remote: &Record,
    reason: &str,
) -> AppResult<()> {
    conn.execute("INSERT INTO webdav_conflicts VALUES(?1,?2,?3,?4,?5) ON CONFLICT(kind,key) DO UPDATE SET local_record=excluded.local_record,remote_record=excluded.remote_record,reason=excluded.reason",params![local.kind,local.key,serde_json::to_string(local).map_err(|_|invalid())?,serde_json::to_string(remote).map_err(|_|invalid())?,reason])?;
    Ok(())
}
/// Detect concurrent edits against this directory's last acknowledged snapshot.
/// Independent fields are separate records, so read/star/later edits commute.
/// Derived article fetch metadata is intentionally reconciled automatically.
pub fn detect_conflicts(conn: &Connection, incoming: &[Record]) -> AppResult<usize> {
    let tx = conn.unchecked_transaction()?;
    let n = detect_rows(&tx, incoming)?;
    tx.commit()?;
    Ok(n)
}
/// Hold the SQLite writer lock across detection, import and baseline update,
/// including against another process using the CLI with the same database.
pub fn merge_remote(conn: &mut Connection, incoming: &[Record]) -> AppResult<usize> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if detect_rows(&tx, incoming)? > 0 {
        tx.commit()?;
        return Err(AppError::code("webdavNeedsResolution"));
    }
    let changed = merge_rows(&tx, incoming)?;
    acknowledge_rows(&tx, incoming)?;
    tx.commit()?;
    Ok(changed)
}
/// Atomically review/import the on-disk remote snapshot.
pub fn merge_staged(conn: &mut Connection) -> AppResult<usize> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if detect_table(&tx, "webdav_incoming")? > 0 {
        tx.commit()?;
        return Err(AppError::code("webdavNeedsResolution"));
    }
    let changed = merge_table(&tx, "webdav_incoming")?;
    tx.execute(
        "UPDATE webdav_incoming_chunks SET dirty=0 WHERE dirty=1",
        [],
    )?;
    tx.execute("DELETE FROM webdav_baseline", [])?;
    tx.execute(
        "INSERT INTO webdav_baseline SELECT * FROM webdav_observed",
        [],
    )?;
    tx.commit()?;
    Ok(changed)
}

fn detect_rows(conn: &Connection, incoming: &[Record]) -> AppResult<usize> {
    stage_slice(conn, incoming)?;
    detect_table(conn, "webdav_input")
}

fn detect_table(conn: &Connection, table: &str) -> AppResult<usize> {
    let specs = current_specs();
    conn.execute("DELETE FROM webdav_observed", [])?;
    conn.execute("DELETE FROM webdav_conflicts", [])?;
    visit(conn,&format!("SELECT record FROM {table} WHERE kind NOT IN ('article','enclosure','retention') ORDER BY kind,key"),|r| {
        validate(&r,specs.iter().find(|s|s.kind==r.kind).ok_or_else(invalid)?)?;
        conn.execute("UPDATE webdav_control SET clock=max(clock,?1) WHERE id=1",[r.clock])?;
        // Article content is automatically reconciled: only revisions needed
        // for user conflict review are retained as a baseline.
        conn.execute("INSERT INTO webdav_observed VALUES(?1,?2,?3)",params![r.kind,r.key,serde_json::to_string(&r).map_err(|_|invalid())?])?;
        let Some(l)=lookup(conn,&r.kind,&r.key)? else { return Ok(()); };
        if same_value(&l,&r) { return Ok(()); }
        let base:Option<String>=conn.query_row("SELECT record FROM webdav_baseline WHERE kind=?1 AND key=?2",params![r.kind,r.key],|row|row.get(0)).optional()?;
        let both_changed=if let Some(base)=base {
            let base=normalized(serde_json::from_str(&base).map_err(|_|invalid())?);
            !same_revision(&l,&base)&&!same_revision(&r,&base)
        } else {l.clock>0&&r.clock>0&&!l.nonce.is_empty()&&!r.nonce.is_empty()};
        // A protected article restored after cleanup carries its original user
        // state. Its canceled purge marker makes this a coherent restoration.
        let restored = if l.deleted && !r.deleted && ["is_read","is_starred","read_later","reading_progress"].contains(&r.kind.as_str()) {
            conn.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {table} p JOIN {table} a ON a.key=p.key AND a.kind='article' WHERE p.kind='retention' AND p.key=?1 AND json_extract(p.record,'$.deleted')=1 AND json_extract(a.record,'$.deleted')=0)"),[&r.key],|row|row.get::<_,bool>(0))?
        } else {false};
        if both_changed && !restored { store_conflict(conn,&l,&r,"concurrent")?; }
        Ok(())
    })?;
    // Only hierarchy metadata needs a combined graph in memory.
    let mut local = std::collections::BTreeMap::new();
    visit(
        conn,
        &format!("SELECT {RECORD_JSON} FROM webdav_records WHERE kind IN ('tag','folder')"),
        |r| {
            local.insert((r.kind.clone(), r.key.clone()), r);
            Ok(())
        },
    )?;
    // Different concurrent moves can form a cycle even when their keys differ.
    // Stop before applying any remote rows and offer moving an edge to root.
    let mut combined = local.clone();
    visit(
        conn,
        &format!("SELECT record FROM {table} WHERE kind IN ('tag','folder')"),
        |r| {
            let k = (r.kind.clone(), r.key.clone());
            if combined.get(&k).is_none_or(|l| newer(&r, l)) {
                combined.insert(k, r);
            }
            Ok(())
        },
    )?;
    for r in combined.values().filter(|r| r.kind == "tag" && !r.deleted) {
        if let Some(parent) = r.data["parent_id"].as_str() {
            if combined
                .get(&("tag".to_owned(), parent.to_owned()))
                .is_none_or(|p| p.deleted)
            {
                let mut root = r.clone();
                root.data["parent_id"] = Value::Null;
                store_conflict(
                    conn,
                    local.get(&(r.kind.clone(), r.key.clone())).unwrap_or(&root),
                    r,
                    "missingParent",
                )?;
            }
        }
    }
    for kind in ["tag", "folder"] {
        for candidate in combined.values().filter(|r| r.kind == kind && !r.deleted) {
            let mut seen = std::collections::BTreeSet::new();
            let mut current = Some(candidate.key.as_str());
            while let Some(k) = current {
                if !seen.insert(k.to_owned()) {
                    for cycle_key in &seen {
                        let key = (kind.to_owned(), cycle_key.clone());
                        if let Some(remote) = combined.get(&key) {
                            let mut root = remote.clone();
                            root.data["parent_id"] = Value::Null;
                            store_conflict(
                                conn,
                                local.get(&key).unwrap_or(&root),
                                remote,
                                "cycle",
                            )?;
                        }
                    }
                    break;
                }
                current = combined
                    .get(&(kind.to_owned(), k.to_owned()))
                    .filter(|r| !r.deleted)
                    .and_then(|r| r.data["parent_id"].as_str());
            }
        }
    }
    let n = conn.query_row("SELECT COUNT(*) FROM webdav_conflicts", [], |r| {
        r.get::<_, usize>(0)
    })?;
    Ok(n)
}
pub fn acknowledge(conn: &Connection, snapshot: &[Record]) -> AppResult<()> {
    let tx = conn.unchecked_transaction()?;
    acknowledge_rows(&tx, snapshot)?;
    tx.commit()?;
    Ok(())
}
fn acknowledge_rows(conn: &Connection, snapshot: &[Record]) -> AppResult<()> {
    conn.execute("DELETE FROM webdav_baseline", [])?;
    for r in snapshot
        .iter()
        .filter(|r| !["article", "enclosure", "retention"].contains(&r.kind.as_str()))
    {
        conn.execute(
            "INSERT INTO webdav_baseline VALUES(?1,?2,?3)",
            params![
                r.kind,
                r.key,
                serde_json::to_string(r).map_err(|_| invalid())?
            ],
        )?;
    }
    Ok(())
}
pub fn resolve_conflict(
    conn: &mut Connection,
    kind: &str,
    key: &str,
    choice: &str,
) -> AppResult<()> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let c = conflicts(&tx)?
        .into_iter()
        .find(|c| c.kind == kind && c.key == key)
        .ok_or_else(|| AppError::code("webdavConflictGone"))?;
    let current = lookup(&tx, kind, key)?;
    if current
        .as_ref()
        .is_some_and(|r| !same_revision(r, &c.local))
        && current.is_some()
    {
        return Err(AppError::code("webdavConflictChanged"));
    }
    let mut chosen = match choice {
        "local" => c.local.clone(),
        "remote" => c.remote.clone(),
        "newer" => {
            if newer(&c.remote, &c.local) {
                c.remote.clone()
            } else {
                c.local.clone()
            }
        }
        "root" if ["tag", "folder"].contains(&kind) => {
            let mut r = c.remote.clone();
            r.data["parent_id"] = Value::Null;
            r.deleted = false;
            r
        }
        _ => return Err(invalid()),
    };
    chosen.clock=tx.query_row("SELECT max(clock+1,?1+1,?2+1,CAST(unixepoch('subsec')*1000 AS INTEGER)) FROM webdav_control WHERE id=1",params![c.local.clock,c.remote.clock],|r|r.get(0))?;
    let mut removed_path = None;
    if ["tag", "folder"].contains(&kind) && chosen.data["parent_id"].is_null() {
        let root_key = serde_json::json!([chosen.data["name"]
            .as_str()
            .ok_or_else(invalid)?
            .to_ascii_lowercase()])
        .to_string();
        if root_key != chosen.key {
            let mut old = chosen.clone();
            old.deleted = true;
            removed_path = Some(old);
            chosen.key = root_key;
        }
    }
    chosen.nonce = probe_id(&tx)?;
    // If hierarchy validation fails, the choice
    // rolls back and the conflict remains available for another resolution.
    let mut changes = vec![chosen];
    if changes[0].key != c.local.key && removed_path.as_ref().is_none_or(|r| r.key != c.local.key) {
        let mut old = c.local.clone();
        old.deleted = true;
        old.clock = changes[0].clock;
        old.nonce = changes[0].nonce.clone();
        changes.push(old);
    }
    if let Some(old) = removed_path {
        changes.push(old);
    }
    merge_rows(&tx, &changes)?;
    tx.execute("INSERT INTO webdav_baseline VALUES(?1,?2,?3) ON CONFLICT(kind,key) DO UPDATE SET record=excluded.record",params![kind,key,serde_json::to_string(&c.remote).map_err(|_|invalid())?])?;
    for change in &changes {
        let observed: Option<String> = tx
            .query_row(
                "SELECT record FROM webdav_observed WHERE kind=?1 AND key=?2",
                params![change.kind, change.key],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(observed) = observed {
            tx.execute("INSERT INTO webdav_baseline VALUES(?1,?2,?3) ON CONFLICT(kind,key) DO UPDATE SET record=excluded.record",params![change.kind,change.key,observed])?;
        }
    }
    tx.execute(
        "DELETE FROM webdav_conflicts WHERE kind=?1 AND key=?2",
        params![kind, key],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn set_progress(conn: &Connection, id: i64, progress: f64) -> AppResult<()> {
    if !progress.is_finite() || !(0.0..=1.0).contains(&progress) {
        return Err(invalid());
    }
    conn.execute(
        "UPDATE articles SET reading_progress=?2 WHERE id=?1",
        params![id, progress],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn database() -> Connection {
        crate::db::open(std::path::Path::new(":memory:")).unwrap()
    }
    fn fixture(conn: &Connection, offset: i64) -> i64 {
        conn.execute(
            "INSERT INTO folders(id,name) VALUES(?1,'News')",
            [offset + 1],
        )
        .unwrap();
        conn.execute("INSERT INTO feeds(id,feed_url,title,folder_id) VALUES(?1,'https://example.com/rss','Feed',?2)",params![offset+2,offset+1]).unwrap();
        conn.execute("INSERT INTO articles(id,feed_id,guid,title,body_text,content_html) VALUES(?1,?2,'guid-1','Article','searchable text','<p>Article</p>')",params![offset+3,offset+2]).unwrap();
        offset + 3
    }
    fn transfer(a: &Connection, b: &mut Connection) {
        merge(b, &records(a).unwrap()).unwrap();
    }
    fn count(c: &Connection, table: &str) -> i64 {
        c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    #[test]
    fn merges_library_with_different_local_ids_and_independent_flags() {
        let mut a = database();
        let mut b = database();
        let aid = fixture(&a, 0);
        let bid = fixture(&b, 100);
        a.execute("INSERT INTO tags(id,name) VALUES(1,'Rust')", [])
            .unwrap();
        a.execute("INSERT INTO article_tags VALUES(?1,1)", [aid])
            .unwrap();
        a.execute(
            "INSERT INTO highlights(article_id,quote,note) VALUES(?1,'Article','note')",
            [aid],
        )
        .unwrap();
        a.execute(
            "INSERT INTO enclosures(article_id,url) VALUES(?1,'https://example.com/audio.mp3')",
            [aid],
        )
        .unwrap();
        crate::db::set_read(&a, aid, true).unwrap();
        crate::db::set_starred(&b, bid, true).unwrap();
        set_progress(&a, aid, 0.625).unwrap();
        transfer(&a, &mut b);
        transfer(&b, &mut a);
        for (conn, id) in [(&a, aid), (&b, bid)] {
            let detail = crate::db::get_article(conn, id).unwrap();
            assert!(detail.is_read && detail.is_starred);
            assert_eq!(detail.reading_progress, 0.625);
            assert_eq!(detail.tags.len(), 1);
            assert_eq!(detail.enclosures.len(), 1);
            assert_eq!(count(conn, "highlights"), 1);
            assert_eq!(count(conn, "feeds"), 1);
            assert_eq!(count(conn, "articles"), 1);
            assert_eq!(count(conn, "articles_fts"), 1);
        }
        assert_eq!(merge(&mut a, &records(&b).unwrap()).unwrap(), 0);
    }
    #[test]
    fn fresh_feed_content_does_not_reset_read_state() {
        let mut a = database();
        let mut b = database();
        let id = fixture(&a, 0);
        crate::db::set_read(&a, id, true).unwrap();
        fixture(&b, 100);
        b.execute("UPDATE articles SET title='New title'", [])
            .unwrap();
        transfer(&b, &mut a);
        transfer(&a, &mut b);
        assert!(crate::db::get_article(&b, 103).unwrap().is_read);
        crate::db::set_read(&b, 103, false).unwrap();
        transfer(&b, &mut a);
        assert!(!crate::db::get_article(&a, id).unwrap().is_read);
    }
    #[test]
    fn deleted_feed_and_children_stay_deleted() {
        let mut a = database();
        let mut b = database();
        let id = fixture(&a, 0);
        a.execute("INSERT INTO tags(name) VALUES('Rust')", [])
            .unwrap();
        a.execute("INSERT INTO article_tags VALUES(?1,1)", [id])
            .unwrap();
        a.execute(
            "INSERT INTO highlights(article_id,quote) VALUES(?1,'Article')",
            [id],
        )
        .unwrap();
        transfer(&a, &mut b);
        a.execute("DELETE FROM feeds", []).unwrap();
        transfer(&a, &mut b);
        transfer(&b, &mut a);
        for c in [&a, &b] {
            assert_eq!(count(c, "feeds"), 0);
            assert_eq!(count(c, "articles"), 0);
            assert_eq!(count(c, "highlights"), 0);
            assert_eq!(count(c, "article_tags"), 0);
        }
    }
    #[test]
    fn renamed_folders_and_tags_preserve_membership() {
        let a = database();
        let mut b = database();
        let id = fixture(&a, 0);
        a.execute("INSERT INTO tags(name) VALUES('Rust')", [])
            .unwrap();
        a.execute("INSERT INTO article_tags VALUES(?1,1)", [id])
            .unwrap();
        transfer(&a, &mut b);
        a.execute("UPDATE folders SET name='Technology'", [])
            .unwrap();
        a.execute("UPDATE tags SET name='Programming'", []).unwrap();
        transfer(&a, &mut b);
        assert_eq!(count(&b, "folders"), 1);
        assert_eq!(count(&b, "tags"), 1);
        let folder: String = b
            .query_row(
                "SELECT name FROM folders JOIN feeds ON feeds.folder_id=folders.id",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(folder, "Technology");
        let imported_id: i64 = b
            .query_row("SELECT id FROM articles", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            crate::db::get_article(&b, imported_id).unwrap().tags[0].name,
            "Programming"
        );
    }
    #[test]
    fn unknown_payload_and_mid_import_failures_roll_back() {
        let a = database();
        let mut b = database();
        let before = records(&b).unwrap();
        fixture(&a, 0);
        let mut incoming = records(&a).unwrap();
        incoming.iter_mut().find(|r| r.kind == "feed").unwrap().data["password"] =
            Value::String("secret".into());
        assert!(merge(&mut b, &incoming).is_err());
        assert_eq!(count(&b, "feeds"), 0);
        let mut incoming = records(&a).unwrap();
        incoming
            .iter_mut()
            .find(|r| r.kind == "article")
            .unwrap()
            .key = "wrong-key".into();
        assert!(merge(&mut b, &incoming).is_err());
        assert_eq!(count(&b, "feeds"), 0);
        assert_eq!(records(&b).unwrap(), before);
        assert_eq!(
            b.query_row("SELECT paused FROM webdav_control", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    #[test]
    fn credentials_and_newsletters_are_never_exported() {
        let a = database();
        fixture(&a, 0);
        crate::db::set_setting(&a, "ai_api_key", "SECRET_API_KEY").unwrap();
        crate::db::set_setting(
            &a,
            "ai_base_url",
            "https://SECRET:SECRET@example.com/v1?token=SECRET",
        )
        .unwrap();
        crate::db::set_setting(&a, "ai_base_url_openai", "SECRET_PROFILE_ENDPOINT").unwrap();
        crate::db::set_setting(&a, "codex_auth", "SECRET_AUTH").unwrap();
        crate::db::set_setting(&a, "webdav_password", "SECRET_DAV_PASSWORD").unwrap();
        a.execute("INSERT INTO feeds(id,feed_url,title,source_type) VALUES(50,'imap://SECRET_IMAP_ADDRESS','Newsletter','newsletter')",[]).unwrap();
        a.execute(
            "INSERT INTO articles(feed_id,guid,title) VALUES(50,'private','SECRET_MAIL')",
            [],
        )
        .unwrap();
        let json = serde_json::to_string(&records(&a).unwrap()).unwrap();
        assert!(!json.contains("SECRET"));
    }
    #[test]
    fn ai_preferences_bootstrap_reset_and_local_endpoints() {
        use crate::db::{configure_ai_selection, get_setting, set_setting};
        let mut a = database();
        let mut b = database();
        configure_ai_selection(&a, "codex", Some("selected-model"), None, Some("high")).unwrap();
        set_setting(&a, "ai_summary_prompt", "写成清晰的短段落，并保留原图。").unwrap();
        set_setting(&b, "ai_api_key", "LOCAL_KEY").unwrap();
        set_setting(&b, "ai_base_url", "https://local-anthropic/v1").unwrap();
        set_setting(&b, "ai_base_url_codex", "https://local-codex/v1").unwrap();
        // A fresh device's defaults must not overwrite a configured one.
        assert_eq!(detect_conflicts(&a, &records(&b).unwrap()).unwrap(), 0);
        merge_remote(&mut a, &records(&b).unwrap()).unwrap();
        assert_eq!(
            get_setting(&a, "ai_model").unwrap().as_deref(),
            Some("selected-model")
        );
        merge_remote(&mut b, &records(&a).unwrap()).unwrap();
        for key in [
            "ai_provider",
            "ai_model",
            "ai_reasoning_effort_codex",
            "ai_summary_prompt",
        ] {
            assert_eq!(get_setting(&a, key).unwrap(), get_setting(&b, key).unwrap());
        }
        assert_eq!(
            get_setting(&b, "ai_api_key").unwrap().as_deref(),
            Some("LOCAL_KEY")
        );
        assert_eq!(
            get_setting(&b, "ai_base_url").unwrap().as_deref(),
            Some("https://local-codex/v1")
        );
        assert_eq!(
            get_setting(&b, "ai_base_url_anthropic").unwrap().as_deref(),
            Some("https://local-anthropic/v1")
        );
        // Default selection and restoring the default prompt are real edits.
        configure_ai_selection(&a, "codex", Some(""), None, Some("")).unwrap();
        set_setting(&a, "ai_summary_prompt", "").unwrap();
        merge_remote(&mut b, &records(&a).unwrap()).unwrap();
        for key in ["ai_model", "ai_reasoning_effort_codex", "ai_summary_prompt"] {
            assert_eq!(get_setting(&b, key).unwrap().as_deref(), Some(""));
        }
    }
    #[test]
    fn ai_independent_edits_merge_and_same_record_conflicts_resolve() {
        use crate::db::{configure_ai_selection, get_setting, set_setting};
        let mut a = database();
        let mut b = database();
        configure_ai_selection(&a, "codex", Some("model-one"), None, Some("low")).unwrap();
        set_setting(&a, "ai_summary_prompt", "Original prompt").unwrap();
        merge_remote(&mut b, &records(&a).unwrap()).unwrap();
        acknowledge(&a, &records(&a).unwrap()).unwrap();
        configure_ai_selection(&a, "codex", Some("model-two"), None, Some("high")).unwrap();
        set_setting(&b, "ai_summary_prompt", "Revised prompt").unwrap();
        merge_remote(&mut a, &records(&b).unwrap()).unwrap();
        merge_remote(&mut b, &records(&a).unwrap()).unwrap();
        assert_eq!(
            get_setting(&b, "ai_model").unwrap().as_deref(),
            Some("model-two")
        );
        assert_eq!(
            get_setting(&a, "ai_summary_prompt").unwrap().as_deref(),
            Some("Revised prompt")
        );
        acknowledge(&a, &records(&a).unwrap()).unwrap();
        configure_ai_selection(&a, "codex", Some("left"), None, Some("low")).unwrap();
        configure_ai_selection(&b, "openai", Some("right"), None, Some("high")).unwrap();
        set_setting(&a, "ai_summary_prompt", "Left prompt").unwrap();
        set_setting(&b, "ai_summary_prompt", "Right prompt").unwrap();
        let remote = records(&b).unwrap();
        acknowledge(&b, &remote).unwrap(); // B successfully uploaded this snapshot.
        assert!(merge_remote(&mut a, &remote).is_err());
        assert_eq!(conflicts(&a).unwrap().len(), 2);
        assert_eq!(
            get_setting(&a, "ai_model").unwrap().as_deref(),
            Some("left")
        );
        resolve_conflict(&mut a, "ai_config", "selection", "remote").unwrap();
        resolve_conflict(&mut a, "ai_prompt", "summary", "local").unwrap();
        merge_remote(&mut a, &remote).unwrap();
        merge_remote(&mut b, &records(&a).unwrap()).unwrap();
        assert_eq!(
            get_setting(&a, "ai_provider").unwrap().as_deref(),
            Some("openai")
        );
        assert_eq!(
            get_setting(&a, "ai_model").unwrap().as_deref(),
            Some("right")
        );
        assert_eq!(
            get_setting(&a, "ai_reasoning_effort_codex")
                .unwrap()
                .as_deref(),
            Some("high")
        );
        assert_eq!(
            get_setting(&b, "ai_summary_prompt").unwrap().as_deref(),
            Some("Left prompt")
        );
        assert_eq!(records(&a).unwrap(), records(&b).unwrap());
    }
    #[test]
    fn ai_payload_allowlist_rejects_secret_injection_and_invalid_values() {
        let a = database();
        let mut b = database();
        let original = records(&b).unwrap();
        let config = records(&a)
            .unwrap()
            .into_iter()
            .find(|r| r.kind == "ai_config")
            .unwrap();
        for (field, value) in [
            ("api_key", serde_json::json!("secret")),
            ("provider", serde_json::json!("unknown")),
            ("effort", serde_json::json!("bad")),
            ("model", Value::Null),
        ] {
            let mut bad = config.clone();
            bad.data[field] = value;
            bad.clock = 100;
            assert!(merge_remote(&mut b, &[bad]).is_err());
        }
        let mut bad = config.clone();
        bad.key = "ai_api_key".into();
        assert!(merge_remote(&mut b, &[bad]).is_err());
        let mut bad = config;
        bad.deleted = true;
        assert!(merge_remote(&mut b, &[bad]).is_err());
        let mut bad = records(&a)
            .unwrap()
            .into_iter()
            .find(|r| r.kind == "ai_prompt")
            .unwrap();
        bad.data["prompt"] = Value::String("字".repeat(12001));
        assert!(merge_remote(&mut b, &[bad]).is_err());
        assert_eq!(records(&b).unwrap(), original);
    }
    #[test]
    fn deleting_preferences_tracks_defaults_and_profile_write_is_atomic() {
        let a = database();
        crate::db::configure_ai_selection(&a, "codex", Some("model"), None, Some("high")).unwrap();
        let config = records(&a)
            .unwrap()
            .into_iter()
            .find(|r| r.kind == "ai_config")
            .unwrap();
        assert_eq!(
            config.data,
            serde_json::json!({"provider":"codex","model":"model","effort":"high"})
        );
        crate::db::set_setting(&a, "ai_summary_prompt", "Custom").unwrap();
        a.execute("DELETE FROM settings", []).unwrap();
        let config = records(&a)
            .unwrap()
            .into_iter()
            .find(|r| r.kind == "ai_config")
            .unwrap();
        assert_eq!(
            config.data,
            serde_json::json!({"provider":"anthropic","model":"","effort":""})
        );
        assert!(!config.deleted);
        let prompt = records(&a)
            .unwrap()
            .into_iter()
            .find(|r| r.kind == "ai_prompt")
            .unwrap();
        assert_eq!(prompt.data["prompt"], "");
        assert!(prompt.clock > 0);
    }
    #[test]
    fn retention_and_deletions_survive_round_trip() {
        let a = database();
        let mut b = database();
        let id = fixture(&a, 0);
        transfer(&a, &mut b);
        a.execute(
            "INSERT INTO article_tombstones(feed_id,guid) SELECT feed_id,guid FROM articles",
            [],
        )
        .unwrap();
        a.execute("DELETE FROM articles WHERE id=?1", [id]).unwrap();
        transfer(&a, &mut b);
        assert_eq!(count(&b, "articles"), 0);
        assert_eq!(count(&b, "article_tombstones"), 1);
    }
    #[test]
    fn global_and_feed_rules_merge_and_follow_feed_deletion() {
        let a = database();
        let mut b = database();
        fixture(&a, 0);
        a.execute(
            "INSERT INTO rules(name,field,query,action) VALUES('Global','title','rust','star')",
            [],
        )
        .unwrap();
        a.execute("INSERT INTO rules(name,feed_id,field,query,action) VALUES('Feed rule',2,'title','go','read')",[]).unwrap();
        transfer(&a, &mut b);
        assert_eq!(count(&b, "rules"), 2);
        a.execute("UPDATE rules SET query='updated' WHERE name='Global'", [])
            .unwrap();
        transfer(&a, &mut b);
        assert_eq!(count(&b, "rules"), 2);
        assert_eq!(
            b.query_row("SELECT query FROM rules WHERE name='Global'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
            "updated"
        );
        a.execute("DELETE FROM feeds", []).unwrap();
        transfer(&a, &mut b);
        assert_eq!(count(&b, "rules"), 1);
    }
    #[test]
    fn imported_html_is_sanitized_before_rendering() {
        let a = database();
        let mut b = database();
        fixture(&a, 0);
        a.execute(
            "UPDATE articles SET content_html='<p onclick=evil()>Safe</p><script>evil()</script>'",
            [],
        )
        .unwrap();
        transfer(&a, &mut b);
        let html: String = b
            .query_row("SELECT content_html FROM articles", [], |r| r.get(0))
            .unwrap();
        assert!(!html.contains("onclick"));
        assert!(!html.contains("script"));
        assert!(html.contains("Safe"));
    }
    #[test]
    fn progress_rejects_invalid_values() {
        let a = database();
        let id = fixture(&a, 0);
        for progress in [-0.1, 1.1, f64::NAN, f64::INFINITY] {
            assert!(set_progress(&a, id, progress).is_err());
        }
    }
    #[test]
    fn same_basename_branches_rename_move_and_delete_without_mixing_articles() {
        let mut a = database();
        let mut b = database();
        let article = fixture(&a, 0);
        let leaves = crate::db::create_tags_in(&a, "agent/code;reading/code;code", None).unwrap();
        crate::db::set_article_tag(&a, article, leaves[0], true).unwrap();
        transfer(&a, &mut b);
        let agent = crate::db::list_tags(&a)
            .unwrap()
            .into_iter()
            .find(|t| t.name == "agent")
            .unwrap()
            .id;
        crate::db::rename_tag(&a, agent, "tools").unwrap();
        transfer(&a, &mut b);
        let imported: i64 = b
            .query_row("SELECT id FROM articles", [], |r| r.get(0))
            .unwrap();
        let tags = crate::db::tags_for_article(&b, imported).unwrap();
        assert_eq!(tags.len(), 1);
        let parent = tags[0].parent_id.unwrap();
        assert_eq!(
            crate::db::list_tags(&b)
                .unwrap()
                .into_iter()
                .find(|t| t.id == parent)
                .unwrap()
                .name,
            "tools"
        );
        crate::db::delete_tag(&a, agent).unwrap();
        transfer(&a, &mut b);
        transfer(&b, &mut a);
        let tags = crate::db::tags_for_article(&b, imported).unwrap();
        assert_eq!(tags.len(), 1);
        assert!(tags[0].parent_id.is_none());
        assert_eq!(
            crate::db::list_tags(&b)
                .unwrap()
                .iter()
                .filter(|t| t.name == "code" && t.parent_id.is_none())
                .count(),
            1
        );
    }

    #[test]
    fn batch_tag_paths_sync_without_flattening_or_parent_assignments() {
        let a = database();
        let mut b = database();
        let article = fixture(&a, 0);
        let leaves = crate::db::create_tags_in(&a, "agent/code;reading/later", None).unwrap();
        for leaf in leaves {
            crate::db::set_article_tag(&a, article, leaf, true).unwrap();
        }
        transfer(&a, &mut b);
        let tags = crate::db::list_tags(&b).unwrap();
        let code = tags.iter().find(|t| t.name == "code").unwrap();
        let agent = tags.iter().find(|t| t.name == "agent").unwrap();
        assert_eq!(code.parent_id, Some(agent.id));
        let id = b
            .query_row("SELECT id FROM articles", [], |r| r.get::<_, i64>(0))
            .unwrap();
        let attached = crate::db::tags_for_article(&b, id).unwrap();
        assert_eq!(attached.len(), 2);
        assert!(attached
            .iter()
            .all(|t| ["code", "later"].contains(&t.name.as_str())));
    }

    #[test]
    fn nested_tags_preserve_membership_counts_moves_and_renames() {
        let a = database();
        let mut b = database();
        let aid = fixture(&a, 0);
        let root = crate::db::create_tag(&a, "Topic").unwrap();
        let child = crate::db::create_tag_in(&a, "Rust", Some(root)).unwrap();
        let leaf = crate::db::create_tag_in(&a, "Async", Some(child)).unwrap();
        crate::db::set_article_tag(&a, aid, leaf, true).unwrap();
        assert!(crate::db::move_tag(&a, root, Some(leaf)).is_err());
        assert!(crate::db::move_tag(&a, root, Some(9999)).is_err());
        assert_eq!(
            crate::db::list_tags(&a)
                .unwrap()
                .iter()
                .find(|t| t.id == root)
                .unwrap()
                .article_count,
            1
        );
        let items = crate::db::list_articles(
            &a,
            &crate::models::ArticleQuery::Tag(root),
            false,
            None,
            false,
            50,
            0,
        )
        .unwrap();
        assert_eq!(items.len(), 1);
        crate::db::rename_tag(&a, root, "Development").unwrap();
        transfer(&a, &mut b);
        let rows = crate::db::list_tags(&b).unwrap();
        let root = rows.iter().find(|t| t.name == "Development").unwrap();
        let child = rows.iter().find(|t| t.name == "Rust").unwrap();
        assert_eq!(child.parent_id, Some(root.id));
        crate::db::delete_tag(
            &a,
            crate::db::list_tags(&a)
                .unwrap()
                .iter()
                .find(|t| t.name == "Rust")
                .unwrap()
                .id,
        )
        .unwrap();
        transfer(&a, &mut b);
        assert_eq!(
            crate::db::list_tags(&b)
                .unwrap()
                .iter()
                .find(|t| t.name == "Async")
                .unwrap()
                .parent_id,
            None
        );
        assert_eq!(count(&b, "article_tags"), 1);
    }
    #[test]
    fn three_way_conflicts_preserve_both_progress_versions_and_resolve() {
        let mut a = database();
        let mut b = database();
        let aid = fixture(&a, 0);
        transfer(&a, &mut b);
        let baseline = records(&a).unwrap();
        acknowledge(&a, &baseline).unwrap();
        acknowledge(&b, &baseline).unwrap();
        let bid = b
            .query_row("SELECT id FROM articles", [], |r| r.get::<_, i64>(0))
            .unwrap();
        set_progress(&a, aid, 0.25).unwrap();
        set_progress(&b, bid, 0.75).unwrap();
        assert_eq!(detect_conflicts(&a, &records(&b).unwrap()).unwrap(), 1);
        let conflict = conflicts(&a).unwrap().remove(0);
        assert_eq!(conflict.kind, "reading_progress");
        assert_eq!(conflict.local.data["reading_progress"], 0.25);
        assert_eq!(conflict.remote.data["reading_progress"], 0.75);
        resolve_conflict(&mut a, &conflict.kind, &conflict.key, "remote").unwrap();
        assert!(conflicts(&a).unwrap().is_empty());
        assert_eq!(
            a.query_row("SELECT reading_progress FROM articles", [], |r| r
                .get::<_, f64>(0))
                .unwrap(),
            0.75
        );
        assert_eq!(detect_conflicts(&a, &records(&b).unwrap()).unwrap(), 0);
        transfer(&a, &mut b);
        assert_eq!(records(&a).unwrap(), records(&b).unwrap());
    }
    #[test]
    fn local_choice_survives_retry_and_changed_item_rejects_stale_choice() {
        let mut a = database();
        let mut b = database();
        let aid = fixture(&a, 0);
        transfer(&a, &mut b);
        let baseline = records(&a).unwrap();
        acknowledge(&a, &baseline).unwrap();
        acknowledge(&b, &baseline).unwrap();
        let bid = b
            .query_row("SELECT id FROM articles", [], |r| r.get::<_, i64>(0))
            .unwrap();
        set_progress(&a, aid, 0.8).unwrap();
        set_progress(&b, bid, 0.4).unwrap();
        detect_conflicts(&a, &records(&b).unwrap()).unwrap();
        let c = conflicts(&a).unwrap().remove(0);
        set_progress(&a, aid, 0.9).unwrap();
        assert!(resolve_conflict(&mut a, &c.kind, &c.key, "local").is_err());
        detect_conflicts(&a, &records(&b).unwrap()).unwrap();
        resolve_conflict(&mut a, &c.kind, &c.key, "local").unwrap();
        assert_eq!(detect_conflicts(&a, &records(&b).unwrap()).unwrap(), 0);
        transfer(&a, &mut b);
        assert_eq!(
            b.query_row("SELECT reading_progress FROM articles", [], |r| r
                .get::<_, f64>(0))
                .unwrap(),
            0.9
        );
    }
    #[test]
    fn independent_flags_and_one_sided_progress_do_not_conflict() {
        let mut a = database();
        let mut b = database();
        let aid = fixture(&a, 0);
        transfer(&a, &mut b);
        let baseline = records(&a).unwrap();
        acknowledge(&a, &baseline).unwrap();
        acknowledge(&b, &baseline).unwrap();
        let bid = b
            .query_row("SELECT id FROM articles", [], |r| r.get::<_, i64>(0))
            .unwrap();
        a.execute(
            "UPDATE articles SET is_read=1,read_later=1,reading_progress=0.8 WHERE id=?1",
            [aid],
        )
        .unwrap();
        b.execute("UPDATE articles SET is_starred=1 WHERE id=?1", [bid])
            .unwrap();
        assert_eq!(detect_conflicts(&a, &records(&b).unwrap()).unwrap(), 0);
        transfer(&b, &mut a);
        transfer(&a, &mut b);
        let row: (i64, i64, i64, f64) = b
            .query_row(
                "SELECT is_read,is_starred,read_later,reading_progress FROM articles",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(row, (1, 1, 1, 0.8));
    }
    #[test]
    fn concurrent_hierarchy_cycle_is_detected_and_can_be_broken() {
        let mut a = database();
        let mut b = database();
        let x = crate::db::create_tag(&a, "A").unwrap();
        let y = crate::db::create_tag(&a, "B").unwrap();
        transfer(&a, &mut b);
        let base = records(&a).unwrap();
        acknowledge(&a, &base).unwrap();
        acknowledge(&b, &base).unwrap();
        crate::db::move_tag(&a, x, Some(y)).unwrap();
        let tags = crate::db::list_tags(&b).unwrap();
        let bx = tags.iter().find(|t| t.name == "A").unwrap().id;
        let by = tags.iter().find(|t| t.name == "B").unwrap().id;
        crate::db::move_tag(&b, by, Some(bx)).unwrap();
        assert!(detect_conflicts(&a, &records(&b).unwrap()).unwrap() > 0);
        let c = conflicts(&a)
            .unwrap()
            .into_iter()
            .find(|c| c.remote.data["name"] == "B")
            .unwrap();
        assert_eq!(c.reason, "missingParent");
        resolve_conflict(&mut a, "tag", &c.key, "root").unwrap();
        assert_eq!(detect_conflicts(&a, &records(&b).unwrap()).unwrap(), 0);
        transfer(&b, &mut a);
        transfer(&a, &mut b);
        assert_eq!(records(&a).unwrap(), records(&b).unwrap());
    }
    #[test]
    fn delete_versus_edit_is_reviewable_and_tombstones_converge() {
        let mut a = database();
        let mut b = database();
        let id = crate::db::create_tag(&a, "Tag").unwrap();
        transfer(&a, &mut b);
        let baseline = records(&a).unwrap();
        acknowledge(&a, &baseline).unwrap();
        acknowledge(&b, &baseline).unwrap();
        crate::db::delete_tag(&a, id).unwrap();
        b.execute("UPDATE tags SET color='blue'", []).unwrap();
        assert_eq!(detect_conflicts(&a, &records(&b).unwrap()).unwrap(), 1);
        resolve_conflict(&mut a, "tag", "[\"tag\"]", "local").unwrap();
        transfer(&a, &mut b);
        assert_eq!(count(&b, "tags"), 0);
    }
}
