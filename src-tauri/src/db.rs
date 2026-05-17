//! SQLite data layer. One file holds feeds, articles, FTS5 index and settings.
//! All SQL lives here; commands call typed functions, never raw SQL.

use crate::error::{AppError, AppResult};
use crate::models::*;
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension};
use rusqlite_migration::{Migrations, M};
use std::path::Path;
use std::sync::LazyLock;

/// Append-only schema migrations. Never edit a shipped migration — add a new one.
static MIGRATIONS: LazyLock<Migrations> = LazyLock::new(|| {
    Migrations::new(vec![M::up(
        r#"
        CREATE TABLE folders (
            id        INTEGER PRIMARY KEY,
            name      TEXT NOT NULL,
            position  INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE feeds (
            id              INTEGER PRIMARY KEY,
            feed_url        TEXT NOT NULL UNIQUE,
            site_url        TEXT,
            title           TEXT NOT NULL,
            description     TEXT,
            favicon_url     TEXT,
            folder_id       INTEGER REFERENCES folders(id) ON DELETE SET NULL,
            source_type     TEXT NOT NULL DEFAULT 'rss',
            etag            TEXT,
            last_modified   TEXT,
            last_fetched_at TEXT,
            fetch_error     TEXT,
            created_at      TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE articles (
            id            INTEGER PRIMARY KEY,
            feed_id       INTEGER NOT NULL REFERENCES feeds(id) ON DELETE CASCADE,
            guid          TEXT NOT NULL,
            url           TEXT,
            title         TEXT NOT NULL,
            author        TEXT,
            summary       TEXT,
            content_html  TEXT,
            extracted_html TEXT,
            body_text     TEXT NOT NULL DEFAULT '',
            image_url     TEXT,
            ai_summary    TEXT,
            published_at  TEXT,
            fetched_at    TEXT NOT NULL DEFAULT (datetime('now')),
            is_read       INTEGER NOT NULL DEFAULT 0,
            is_starred    INTEGER NOT NULL DEFAULT 0,
            read_later    INTEGER NOT NULL DEFAULT 0,
            UNIQUE(feed_id, guid)
        );

        CREATE INDEX idx_articles_feed      ON articles(feed_id);
        CREATE INDEX idx_articles_published ON articles(published_at DESC);
        CREATE INDEX idx_articles_unread    ON articles(is_read) WHERE is_read = 0;

        CREATE TABLE enclosures (
            id         INTEGER PRIMARY KEY,
            article_id INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
            url        TEXT NOT NULL,
            mime_type  TEXT,
            length     INTEGER
        );
        CREATE INDEX idx_enclosures_article ON enclosures(article_id);

        CREATE VIRTUAL TABLE articles_fts USING fts5(
            title, body, tokenize = 'porter unicode61'
        );

        -- Keep the FTS index in sync on delete; inserts are handled in code so
        -- that read-state updates do not trigger needless re-indexing.
        CREATE TRIGGER articles_fts_ad AFTER DELETE ON articles BEGIN
            DELETE FROM articles_fts WHERE rowid = old.id;
        END;

        CREATE TABLE settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        "#,
        ),
        // v2 — placeholder. An earlier sqlite-vec semantic-search schema was
        // removed; this keeps the version count aligned for databases that
        // already applied it. Search is keyword-only (FTS5).
        M::up("-- semantic search removed; search is FTS5 keyword-only"),
        // v3 — sync support: a remote item id per article plus a small queue
        // of local read/starred changes still to push to the sync server.
        M::up(
            r#"
            ALTER TABLE articles ADD COLUMN remote_id TEXT;
            CREATE TABLE sync_queue (
                article_id INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
                field      TEXT NOT NULL,
                value      INTEGER NOT NULL,
                PRIMARY KEY (article_id, field)
            );
            "#,
        ),
        // v4 — article tags: a flat label set plus an article↔tag join table.
        M::up(
            r#"
            CREATE TABLE tags (
                id        INTEGER PRIMARY KEY,
                name      TEXT NOT NULL UNIQUE,
                color     TEXT NOT NULL DEFAULT 'clay',
                position  INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE article_tags (
                article_id INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
                tag_id     INTEGER NOT NULL REFERENCES tags(id)     ON DELETE CASCADE,
                PRIMARY KEY (article_id, tag_id)
            );
            CREATE INDEX idx_article_tags_tag ON article_tags(tag_id);
            "#,
        ),
        // v5 — filter rules: keyword matches applied to incoming articles to
        // auto-skip noise, or auto mark-read / star them, at ingestion time.
        M::up(
            r#"
            CREATE TABLE rules (
                id         INTEGER PRIMARY KEY,
                name       TEXT NOT NULL,
                enabled    INTEGER NOT NULL DEFAULT 1,
                feed_id    INTEGER REFERENCES feeds(id) ON DELETE CASCADE,
                field      TEXT NOT NULL DEFAULT 'title',
                query      TEXT NOT NULL,
                action     TEXT NOT NULL DEFAULT 'skip',
                position   INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            "#,
        ),
        // v6 — index over the effective article date the list sorts by,
        // COALESCE(published_at, fetched_at), so a dateless entry sorts by
        // when it was fetched instead of sinking below every dated article.
        M::up(
            "CREATE INDEX idx_articles_sort
             ON articles(COALESCE(published_at, fetched_at) DESC, id DESC);",
        ),
    ])
});

/// Open the writer connection: run migrations and set the write-side pragmas.
/// WAL mode is persisted in the database header, so reader connections opened
/// afterwards inherit it automatically.
pub fn open(path: &Path) -> AppResult<Connection> {
    let mut conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    MIGRATIONS.to_latest(&mut conn)?;
    Ok(conn)
}

/// Open a read-only connection for the UI query pool. Under WAL these run
/// concurrently with the writer, so interface reads never block on a
/// background refresh. `query_only` is a safety net against an accidental
/// write on a pooled reader. Must be called after `open` has migrated.
pub fn open_reader(path: &Path) -> AppResult<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "query_only", true)?;
    Ok(conn)
}

// ─────────────────────────── folders ───────────────────────────

pub fn list_folders(conn: &Connection) -> AppResult<Vec<Folder>> {
    let mut stmt =
        conn.prepare("SELECT id, name, position FROM folders ORDER BY position, name")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Folder {
                id: r.get(0)?,
                name: r.get(1)?,
                position: r.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn create_folder(conn: &Connection, name: &str) -> AppResult<i64> {
    conn.execute(
        "INSERT INTO folders(name, position) VALUES (?1, (SELECT COALESCE(MAX(position),0)+1 FROM folders))",
        params![name],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn rename_folder(conn: &Connection, id: i64, name: &str) -> AppResult<()> {
    conn.execute("UPDATE folders SET name = ?2 WHERE id = ?1", params![id, name])?;
    Ok(())
}

pub fn delete_folder(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM folders WHERE id = ?1", params![id])?;
    Ok(())
}

// ─────────────────────────── feeds ───────────────────────────

pub fn find_feed_by_url(conn: &Connection, url: &str) -> AppResult<Option<i64>> {
    Ok(conn
        .query_row("SELECT id FROM feeds WHERE feed_url = ?1", params![url], |r| {
            r.get(0)
        })
        .optional()?)
}

pub fn insert_feed(
    conn: &Connection,
    feed_url: &str,
    site_url: Option<&str>,
    title: &str,
    description: Option<&str>,
    source_type: SourceType,
    folder_id: Option<i64>,
) -> AppResult<i64> {
    conn.execute(
        "INSERT INTO feeds(feed_url, site_url, title, description, source_type, folder_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![feed_url, site_url, title, description, source_type.as_str(), folder_id],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn list_feeds(conn: &Connection) -> AppResult<Vec<Feed>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.feed_url, f.site_url, f.title, f.description, f.favicon_url,
                f.folder_id, f.source_type, f.last_fetched_at, f.fetch_error,
                (SELECT COUNT(*) FROM articles a WHERE a.feed_id = f.id AND a.is_read = 0)
         FROM feeds f ORDER BY f.title COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Feed {
                id: r.get(0)?,
                feed_url: r.get(1)?,
                site_url: r.get(2)?,
                title: r.get(3)?,
                description: r.get(4)?,
                favicon_url: r.get(5)?,
                folder_id: r.get(6)?,
                source_type: r.get(7)?,
                last_fetched_at: r.get(8)?,
                fetch_error: r.get(9)?,
                unread_count: r.get(10)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// All feeds that need fetching: (id, feed_url, etag, last_modified).
pub fn feeds_to_refresh(conn: &Connection) -> AppResult<Vec<(i64, String, Option<String>, Option<String>)>> {
    let mut stmt = conn.prepare("SELECT id, feed_url, etag, last_modified FROM feeds")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn update_feed_meta(
    conn: &Connection,
    id: i64,
    title: Option<&str>,
    site_url: Option<&str>,
    description: Option<&str>,
    favicon_url: Option<&str>,
) -> AppResult<()> {
    conn.execute(
        "UPDATE feeds SET
            title       = COALESCE(?2, title),
            site_url    = COALESCE(?3, site_url),
            description = COALESCE(?4, description),
            favicon_url = COALESCE(?5, favicon_url)
         WHERE id = ?1",
        params![id, title, site_url, description, favicon_url],
    )?;
    Ok(())
}

pub fn set_feed_fetch_state(
    conn: &Connection,
    id: i64,
    etag: Option<&str>,
    last_modified: Option<&str>,
    error: Option<&str>,
) -> AppResult<()> {
    conn.execute(
        "UPDATE feeds SET etag = ?2, last_modified = ?3, fetch_error = ?4,
                          last_fetched_at = datetime('now')
         WHERE id = ?1",
        params![id, etag, last_modified, error],
    )?;
    Ok(())
}

/// Record a successful fetch that produced no changes (304 Not Modified).
pub fn touch_feed(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute(
        "UPDATE feeds SET last_fetched_at = datetime('now'), fetch_error = NULL WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

/// Record a failed fetch, keeping the previous content untouched.
pub fn set_feed_error(conn: &Connection, id: i64, error: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE feeds SET last_fetched_at = datetime('now'), fetch_error = ?2 WHERE id = ?1",
        params![id, error],
    )?;
    Ok(())
}

pub fn delete_feed(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM feeds WHERE id = ?1", params![id])?;
    Ok(())
}

/// (title, feed_url, folder_name) for every feed — used to build OPML exports.
pub fn feeds_for_export(conn: &Connection) -> AppResult<Vec<(String, String, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT f.title, f.feed_url, fo.name
         FROM feeds f LEFT JOIN folders fo ON fo.id = f.folder_id
         ORDER BY fo.name, f.title",
    )?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Find a folder by name, creating it if absent. Used during OPML import.
pub fn folder_id_by_name(conn: &Connection, name: &str) -> AppResult<i64> {
    if let Some(id) = conn
        .query_row("SELECT id FROM folders WHERE name = ?1", params![name], |r| {
            r.get(0)
        })
        .optional()?
    {
        Ok(id)
    } else {
        create_folder(conn, name)
    }
}

pub fn move_feed(conn: &Connection, id: i64, folder_id: Option<i64>) -> AppResult<()> {
    conn.execute("UPDATE feeds SET folder_id = ?2 WHERE id = ?1", params![id, folder_id])?;
    Ok(())
}

pub fn rename_feed(conn: &Connection, id: i64, title: &str) -> AppResult<()> {
    conn.execute("UPDATE feeds SET title = ?2 WHERE id = ?1", params![id, title])?;
    Ok(())
}

// ─────────────────────────── first-run seed ───────────────────────────

/// Built-in subscriptions for a fresh install — feeds programmers tend to like.
const DEFAULT_FEEDS: &[(&str, &str)] = &[
    ("Hacker News", "https://hnrss.org/frontpage"),
    ("Lobsters", "https://lobste.rs/rss"),
    ("Rust Blog", "https://blog.rust-lang.org/feed.xml"),
    ("The GitHub Blog", "https://github.blog/feed/"),
    ("Julia Evans", "https://jvns.ca/atom.xml"),
    ("Simon Willison", "https://simonwillison.net/atom/everything/"),
    ("Overreacted — Dan Abramov", "https://overreacted.io/rss.xml"),
    ("Martin Fowler", "https://martinfowler.com/feed.atom"),
];

/// Seed the default feeds on first launch. Returns true if seeding ran (so the
/// caller can trigger an immediate refresh); a no-op on every later launch.
pub fn seed_default_feeds(conn: &Connection) -> AppResult<bool> {
    if get_setting(conn, "seeded")?.is_some() {
        return Ok(false);
    }
    let folder = create_folder(conn, "Tech")?;
    for (title, url) in DEFAULT_FEEDS {
        // All defaults are plain RSS/Atom; classification refines after fetch.
        let _ = insert_feed(conn, url, None, title, None, SourceType::Rss, Some(folder));
    }
    set_setting(conn, "seeded", "1")?;
    Ok(true)
}

// ─────────────────────────── articles ───────────────────────────

/// A parsed article ready for insertion.
pub struct NewArticle {
    pub guid: String,
    pub url: Option<String>,
    pub title: String,
    pub author: Option<String>,
    pub summary: Option<String>,
    pub content_html: Option<String>,
    pub body_text: String,
    pub image_url: Option<String>,
    pub published_at: Option<String>,
    pub enclosures: Vec<Enclosure>,
}

/// True if `rule` (scoped to `feed_id`) matches the incoming article `a`.
/// The query is a comma-separated keyword list; any substring hit fires it.
fn rule_matches(rule: &Rule, feed_id: i64, a: &NewArticle) -> bool {
    if rule.feed_id.is_some_and(|fid| fid != feed_id) {
        return false;
    }
    let author = a.author.as_deref().unwrap_or("");
    let haystack = match rule.field.as_str() {
        "author" => author.to_lowercase(),
        "content" => a.body_text.to_lowercase(),
        "any" => format!("{} {} {}", a.title, author, a.body_text).to_lowercase(),
        _ => a.title.to_lowercase(),
    };
    rule.query
        .split(',')
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .any(|term| haystack.contains(&term))
}

/// Insert an article if it is new (by feed_id + guid). Returns true if inserted.
/// When `dedup` is on, an article whose URL already exists (in any feed) is
/// skipped — collapsing the same story pushed by multiple feeds. Enabled
/// `rules` are evaluated first: a `skip` match drops the article entirely,
/// while `read` / `star` matches pre-set the article's state on insert.
pub fn upsert_article(
    conn: &Connection,
    feed_id: i64,
    a: &NewArticle,
    dedup: bool,
    rules: &[Rule],
) -> AppResult<bool> {
    if dedup {
        if let Some(url) = a.url.as_deref().filter(|u| !u.is_empty()) {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM articles WHERE url = ?1)",
                params![url],
                |r| r.get(0),
            )?;
            if exists {
                return Ok(false);
            }
        }
    }
    // Apply filter rules: skip wins outright; read / star tint the new row.
    let (mut start_read, mut start_starred) = (false, false);
    for rule in rules {
        if !rule_matches(rule, feed_id, a) {
            continue;
        }
        match rule.action.as_str() {
            "skip" => return Ok(false),
            "read" => start_read = true,
            "star" => start_starred = true,
            _ => {}
        }
    }
    // The article row, its FTS index entry, and its enclosures must land
    // together — a partial insert leaves an unsearchable or enclosure-less
    // article. Wrap them in a transaction so a mid-loop failure rolls back.
    let tx = conn.unchecked_transaction()?;
    let n = tx.execute(
        "INSERT INTO articles
            (feed_id, guid, url, title, author, summary, content_html, body_text,
             image_url, published_at, is_read, is_starred)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(feed_id, guid) DO NOTHING",
        params![
            feed_id, a.guid, a.url, a.title, a.author, a.summary,
            a.content_html, a.body_text, a.image_url, a.published_at,
            start_read, start_starred
        ],
    )?;
    if n == 0 {
        return Ok(false);
    }
    let id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO articles_fts(rowid, title, body) VALUES (?1, ?2, ?3)",
        params![id, a.title, a.body_text],
    )?;
    for e in &a.enclosures {
        tx.execute(
            "INSERT INTO enclosures(article_id, url, mime_type, length) VALUES (?1,?2,?3,?4)",
            params![id, e.url, e.mime_type, e.length],
        )?;
    }
    tx.commit()?;
    Ok(true)
}

/// Build and run the article-list query for the given sidebar selection.
pub fn list_articles(
    conn: &Connection,
    query: &ArticleQuery,
    unread_only: bool,
    search: Option<&str>,
    oldest_first: bool,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<ArticleSummary>> {
    let mut where_clauses: Vec<String> = vec!["1=1".into()];
    let mut binds: Vec<Value> = Vec::new();

    match query {
        ArticleQuery::All => {}
        ArticleQuery::Unread => where_clauses.push("a.is_read = 0".into()),
        ArticleQuery::Starred => where_clauses.push("a.is_starred = 1".into()),
        ArticleQuery::ReadLater => where_clauses.push("a.read_later = 1".into()),
        ArticleQuery::Feed(id) => {
            where_clauses.push("a.feed_id = ?".into());
            binds.push(Value::Integer(*id));
        }
        ArticleQuery::Folder(id) => {
            where_clauses.push("f.folder_id = ?".into());
            binds.push(Value::Integer(*id));
        }
        ArticleQuery::Tag(id) => {
            where_clauses.push(
                "a.id IN (SELECT article_id FROM article_tags WHERE tag_id = ?)".into(),
            );
            binds.push(Value::Integer(*id));
        }
    }
    if unread_only && !matches!(query, ArticleQuery::Unread) {
        where_clauses.push("a.is_read = 0".into());
    }

    let searching = search.map(|s| !s.trim().is_empty()).unwrap_or(false);
    let mut sql = String::from(
        "SELECT a.id, a.feed_id, f.title, f.source_type, a.title, a.author,
                substr(a.body_text,1,280), a.image_url, a.url, a.published_at,
                a.is_read, a.is_starred, a.read_later
         FROM articles a JOIN feeds f ON f.id = a.feed_id ",
    );
    if searching {
        sql.push_str("JOIN articles_fts fts ON fts.rowid = a.id ");
        where_clauses.push("articles_fts MATCH ?".into());
        binds.push(Value::Text(fts_query(search.unwrap())));
    }
    sql.push_str("WHERE ");
    sql.push_str(&where_clauses.join(" AND "));
    // Sort by the effective date — COALESCE(published_at, fetched_at) — so
    // an article with no feed-supplied date orders by when it arrived rather
    // than sinking to the bottom. Backed by `idx_articles_sort`.
    sql.push_str(if searching {
        " ORDER BY fts.rank "
    } else if oldest_first {
        " ORDER BY COALESCE(a.published_at, a.fetched_at) ASC, a.id ASC "
    } else {
        " ORDER BY COALESCE(a.published_at, a.fetched_at) DESC, a.id DESC "
    });
    sql.push_str("LIMIT ? OFFSET ?");
    binds.push(Value::Integer(limit));
    binds.push(Value::Integer(offset));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(binds), |r| {
            Ok(ArticleSummary {
                id: r.get(0)?,
                feed_id: r.get(1)?,
                feed_title: r.get(2)?,
                source_type: r.get(3)?,
                title: r.get(4)?,
                author: r.get(5)?,
                snippet: r.get(6)?,
                image_url: r.get(7)?,
                url: r.get(8)?,
                published_at: r.get(9)?,
                is_read: r.get(10)?,
                is_starred: r.get(11)?,
                read_later: r.get(12)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Turn raw user text into a safe FTS5 MATCH expression (each term prefix-matched).
fn fts_query(input: &str) -> String {
    let terms: Vec<String> = input
        .split_whitespace()
        .map(|t| t.chars().filter(|c| c.is_alphanumeric()).collect::<String>())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\"*"))
        .collect();
    if terms.is_empty() {
        "\"\"".into()
    } else {
        terms.join(" ")
    }
}

/// Recent articles as `(title, feed_title, text)` for building an AI digest.
pub fn digest_source(conn: &Connection, limit: i64) -> AppResult<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT a.title, f.title, substr(a.body_text, 1, 600)
         FROM articles a JOIN feeds f ON f.id = a.feed_id
         ORDER BY a.published_at DESC, a.id DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_article(conn: &Connection, id: i64) -> AppResult<ArticleDetail> {
    let mut detail = conn.query_row(
        "SELECT a.id, a.feed_id, f.title, f.source_type, a.title, a.author, a.url,
                a.content_html, a.extracted_html, a.image_url, a.published_at,
                a.is_read, a.is_starred, a.read_later, a.ai_summary
         FROM articles a JOIN feeds f ON f.id = a.feed_id WHERE a.id = ?1",
        params![id],
        |r| {
            Ok(ArticleDetail {
                id: r.get(0)?,
                feed_id: r.get(1)?,
                feed_title: r.get(2)?,
                source_type: r.get(3)?,
                title: r.get(4)?,
                author: r.get(5)?,
                url: r.get(6)?,
                content_html: r.get(7)?,
                extracted_html: r.get(8)?,
                image_url: r.get(9)?,
                published_at: r.get(10)?,
                is_read: r.get(11)?,
                is_starred: r.get(12)?,
                read_later: r.get(13)?,
                ai_summary: r.get(14)?,
                enclosures: Vec::new(),
                tags: Vec::new(),
            })
        },
    )?;
    let mut stmt =
        conn.prepare("SELECT url, mime_type, length FROM enclosures WHERE article_id = ?1")?;
    detail.enclosures = stmt
        .query_map(params![id], |r| {
            Ok(Enclosure {
                url: r.get(0)?,
                mime_type: r.get(1)?,
                length: r.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    detail.tags = tags_for_article(conn, id)?;
    Ok(detail)
}

/// Plain article text used to build an AI prompt.
pub fn article_text(conn: &Connection, id: i64) -> AppResult<(String, String)> {
    Ok(conn.query_row(
        "SELECT title, body_text FROM articles WHERE id = ?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

pub fn set_read(conn: &Connection, id: i64, read: bool) -> AppResult<()> {
    conn.execute("UPDATE articles SET is_read = ?2 WHERE id = ?1", params![id, read])?;
    Ok(())
}

pub fn set_starred(conn: &Connection, id: i64, starred: bool) -> AppResult<()> {
    conn.execute("UPDATE articles SET is_starred = ?2 WHERE id = ?1", params![id, starred])?;
    Ok(())
}

pub fn set_read_later(conn: &Connection, id: i64, v: bool) -> AppResult<()> {
    conn.execute("UPDATE articles SET read_later = ?2 WHERE id = ?1", params![id, v])?;
    Ok(())
}

pub fn set_extracted_html(conn: &Connection, id: i64, html: &str) -> AppResult<()> {
    conn.execute("UPDATE articles SET extracted_html = ?2 WHERE id = ?1", params![id, html])?;
    Ok(())
}

pub fn set_ai_summary(conn: &Connection, id: i64, summary: &str) -> AppResult<()> {
    conn.execute("UPDATE articles SET ai_summary = ?2 WHERE id = ?1", params![id, summary])?;
    Ok(())
}

/// Mark every article matching the current sidebar selection as read.
pub fn mark_all_read(conn: &Connection, query: &ArticleQuery) -> AppResult<usize> {
    let n = match query {
        ArticleQuery::All | ArticleQuery::Unread => {
            conn.execute("UPDATE articles SET is_read = 1 WHERE is_read = 0", [])?
        }
        ArticleQuery::Starred => conn
            .execute("UPDATE articles SET is_read = 1 WHERE is_starred = 1 AND is_read = 0", [])?,
        ArticleQuery::ReadLater => conn
            .execute("UPDATE articles SET is_read = 1 WHERE read_later = 1 AND is_read = 0", [])?,
        ArticleQuery::Feed(id) => conn.execute(
            "UPDATE articles SET is_read = 1 WHERE feed_id = ?1 AND is_read = 0",
            params![id],
        )?,
        ArticleQuery::Folder(id) => conn.execute(
            "UPDATE articles SET is_read = 1 WHERE is_read = 0 AND feed_id IN
                (SELECT id FROM feeds WHERE folder_id = ?1)",
            params![id],
        )?,
        ArticleQuery::Tag(id) => conn.execute(
            "UPDATE articles SET is_read = 1 WHERE is_read = 0 AND id IN
                (SELECT article_id FROM article_tags WHERE tag_id = ?1)",
            params![id],
        )?,
    };
    Ok(n)
}

// ─────────────────────────── tags ───────────────────────────

/// Palette keys cycled through as new tags are created.
const TAG_COLORS: &[&str] = &[
    "clay", "amber", "pine", "teal", "indigo", "violet", "rose", "slate",
];

/// Every tag, ordered for the sidebar, with a live article count.
pub fn list_tags(conn: &Connection) -> AppResult<Vec<Tag>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.name, t.color, t.position,
                (SELECT COUNT(*) FROM article_tags at WHERE at.tag_id = t.id)
         FROM tags t ORDER BY t.position, t.name COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Tag {
                id: r.get(0)?,
                name: r.get(1)?,
                color: r.get(2)?,
                position: r.get(3)?,
                article_count: r.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Create a tag, auto-assigning the next palette colour and list position.
pub fn create_tag(conn: &Connection, name: &str) -> AppResult<i64> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0))?;
    let color = TAG_COLORS[(count as usize) % TAG_COLORS.len()];
    conn.execute(
        "INSERT INTO tags(name, color, position) VALUES (?1, ?2, ?3)",
        params![name, color, count],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn rename_tag(conn: &Connection, id: i64, name: &str) -> AppResult<()> {
    conn.execute("UPDATE tags SET name = ?2 WHERE id = ?1", params![id, name])?;
    Ok(())
}

pub fn set_tag_color(conn: &Connection, id: i64, color: &str) -> AppResult<()> {
    conn.execute("UPDATE tags SET color = ?2 WHERE id = ?1", params![id, color])?;
    Ok(())
}

/// Persist a new tag ordering — `ids` listed in the desired display order.
pub fn reorder_tags(conn: &Connection, ids: &[i64]) -> AppResult<()> {
    for (pos, id) in ids.iter().enumerate() {
        conn.execute(
            "UPDATE tags SET position = ?2 WHERE id = ?1",
            params![id, pos as i64],
        )?;
    }
    Ok(())
}

pub fn delete_tag(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM tags WHERE id = ?1", params![id])?;
    Ok(())
}

/// Attach (`on = true`) or detach a tag from one article.
pub fn set_article_tag(conn: &Connection, article_id: i64, tag_id: i64, on: bool) -> AppResult<()> {
    if on {
        conn.execute(
            "INSERT INTO article_tags(article_id, tag_id) VALUES (?1, ?2)
             ON CONFLICT DO NOTHING",
            params![article_id, tag_id],
        )?;
    } else {
        conn.execute(
            "DELETE FROM article_tags WHERE article_id = ?1 AND tag_id = ?2",
            params![article_id, tag_id],
        )?;
    }
    Ok(())
}

/// Tags attached to one article (article_count left at 0 — unused per-article).
pub fn tags_for_article(conn: &Connection, article_id: i64) -> AppResult<Vec<Tag>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.name, t.color, t.position
         FROM tags t JOIN article_tags at ON at.tag_id = t.id
         WHERE at.article_id = ?1 ORDER BY t.position, t.name COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map(params![article_id], |r| {
            Ok(Tag {
                id: r.get(0)?,
                name: r.get(1)?,
                color: r.get(2)?,
                position: r.get(3)?,
                article_count: 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ─────────────────────────── filter rules ───────────────────────────

fn row_to_rule(r: &rusqlite::Row) -> rusqlite::Result<Rule> {
    Ok(Rule {
        id: r.get(0)?,
        name: r.get(1)?,
        enabled: r.get(2)?,
        feed_id: r.get(3)?,
        field: r.get(4)?,
        query: r.get(5)?,
        action: r.get(6)?,
        position: r.get(7)?,
    })
}

const RULE_COLS: &str = "id, name, enabled, feed_id, field, query, action, position";

/// Every rule, enabled or not, ordered for the settings list.
pub fn list_rules(conn: &Connection) -> AppResult<Vec<Rule>> {
    let mut stmt =
        conn.prepare(&format!("SELECT {RULE_COLS} FROM rules ORDER BY position, id"))?;
    let rows = stmt
        .query_map([], row_to_rule)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Only the enabled rules — the set evaluated against incoming articles.
pub fn active_rules(conn: &Connection) -> AppResult<Vec<Rule>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RULE_COLS} FROM rules WHERE enabled = 1 ORDER BY position, id"
    ))?;
    let rows = stmt
        .query_map([], row_to_rule)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn create_rule(
    conn: &Connection,
    name: &str,
    feed_id: Option<i64>,
    field: &str,
    query: &str,
    action: &str,
) -> AppResult<i64> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM rules", [], |r| r.get(0))?;
    conn.execute(
        "INSERT INTO rules(name, feed_id, field, query, action, position)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![name, feed_id, field, query, action, count],
    )?;
    Ok(conn.last_insert_rowid())
}

#[allow(clippy::too_many_arguments)]
pub fn update_rule(
    conn: &Connection,
    id: i64,
    name: &str,
    enabled: bool,
    feed_id: Option<i64>,
    field: &str,
    query: &str,
    action: &str,
) -> AppResult<()> {
    conn.execute(
        "UPDATE rules SET name = ?2, enabled = ?3, feed_id = ?4,
                          field = ?5, query = ?6, action = ?7
         WHERE id = ?1",
        params![id, name, enabled, feed_id, field, query, action],
    )?;
    Ok(())
}

pub fn delete_rule(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM rules WHERE id = ?1", params![id])?;
    Ok(())
}

/// Preview how a draft rule would behave: the number of *already-stored*
/// articles its keywords match, plus a handful of recent sample titles.
/// Lets the user sanity-check a rule before saving it.
pub fn preview_rule(
    conn: &Connection,
    feed_id: Option<i64>,
    field: &str,
    query: &str,
) -> AppResult<(i64, Vec<String>)> {
    let terms: Vec<String> = query
        .split(',')
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    if terms.is_empty() {
        return Ok((0, Vec::new()));
    }
    let cols: &[&str] = match field {
        "author" => &["a.author"],
        "content" => &["a.body_text"],
        "any" => &["a.title", "a.author", "a.body_text"],
        _ => &["a.title"],
    };
    // One LIKE clause per (term × column); LIKE wildcards in the term are
    // escaped so a literal `%` or `_` keyword can't widen the match.
    let mut ors: Vec<String> = Vec::new();
    let mut binds: Vec<Value> = Vec::new();
    for term in &terms {
        let escaped = term
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        for col in cols {
            ors.push(format!("LOWER(COALESCE({col},'')) LIKE ? ESCAPE '\\'"));
            binds.push(Value::Text(format!("%{escaped}%")));
        }
    }
    let mut where_sql = format!("({})", ors.join(" OR "));
    if let Some(fid) = feed_id {
        where_sql.push_str(" AND a.feed_id = ?");
        binds.push(Value::Integer(fid));
    }

    let count: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM articles a WHERE {where_sql}"),
        params_from_iter(binds.iter().cloned()),
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(&format!(
        "SELECT a.title FROM articles a WHERE {where_sql}
         ORDER BY a.published_at DESC, a.id DESC LIMIT 5"
    ))?;
    let samples = stmt
        .query_map(params_from_iter(binds), |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok((count, samples))
}

/// (total unread, starred, read-later) counts for the sidebar smart folders.
pub fn smart_counts(conn: &Connection) -> AppResult<(i64, i64, i64)> {
    Ok(conn.query_row(
        "SELECT
            (SELECT COUNT(*) FROM articles WHERE is_read = 0),
            (SELECT COUNT(*) FROM articles WHERE is_starred = 1),
            (SELECT COUNT(*) FROM articles WHERE read_later = 1)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?)
}

// ─────────────────────────── settings ───────────────────────────

pub fn get_setting(conn: &Connection, key: &str) -> AppResult<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| {
            r.get(0)
        })
        .optional()?)
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> AppResult<()> {
    conn.execute(
        "INSERT INTO settings(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

#[allow(dead_code)]
fn _unused(_: &AppError) {}

// ─────────────────────────── storage ───────────────────────────

/// `(database bytes, article count, feed count)` for the storage panel.
pub fn storage_stats(conn: &Connection) -> AppResult<(i64, i64, i64)> {
    let page_count: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
    let page_size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    let articles: i64 = conn.query_row("SELECT COUNT(*) FROM articles", [], |r| r.get(0))?;
    let feeds: i64 = conn.query_row("SELECT COUNT(*) FROM feeds", [], |r| r.get(0))?;
    Ok((page_count * page_size, articles, feeds))
}

/// Delete read articles older than `days`, keeping starred / read-later ones.
/// Returns the number removed.
pub fn cleanup_old_articles(conn: &Connection, days: i64) -> AppResult<usize> {
    Ok(conn.execute(
        "DELETE FROM articles
         WHERE is_starred = 0 AND read_later = 0 AND is_read = 1
           AND published_at IS NOT NULL
           AND published_at < datetime('now', ?1)",
        params![format!("-{days} days")],
    )?)
}

/// Reclaim free pages — must run outside any transaction.
pub fn vacuum(conn: &Connection) -> AppResult<()> {
    conn.execute_batch("VACUUM")?;
    Ok(())
}

/// Wipe all user content (feeds → articles cascade, folders). Settings are kept.
pub fn clear_all_data(conn: &Connection) -> AppResult<()> {
    conn.execute_batch("DELETE FROM feeds; DELETE FROM folders;")?;
    Ok(())
}

/// Clear every stored setting except the first-run seed marker.
pub fn reset_settings(conn: &Connection) -> AppResult<()> {
    conn.execute("DELETE FROM settings WHERE key != 'seeded'", [])?;
    Ok(())
}

pub fn count_unread(conn: &Connection) -> AppResult<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM articles WHERE is_read = 0", [], |r| r.get(0))?)
}

/// Timestamp of the most recent successful feed fetch, if any.
pub fn latest_fetch(conn: &Connection) -> AppResult<Option<String>> {
    Ok(conn.query_row("SELECT MAX(last_fetched_at) FROM feeds", [], |r| {
        r.get::<_, Option<String>>(0)
    })?)
}

// ─────────────────────────── sync ───────────────────────────

/// Local article id for a given source URL — used to reconcile remote state.
pub fn article_id_by_url(conn: &Connection, url: &str) -> AppResult<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM articles WHERE url = ?1 LIMIT 1",
            params![url],
            |r| r.get(0),
        )
        .optional()?)
}

pub fn set_remote_id(conn: &Connection, article_id: i64, remote_id: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE articles SET remote_id = ?2 WHERE id = ?1",
        params![article_id, remote_id],
    )?;
    Ok(())
}

/// Apply remote read/starred state to a local article.
pub fn set_sync_state(
    conn: &Connection,
    article_id: i64,
    read: bool,
    starred: bool,
) -> AppResult<()> {
    conn.execute(
        "UPDATE articles SET is_read = ?2, is_starred = ?3 WHERE id = ?1",
        params![article_id, read, starred],
    )?;
    Ok(())
}

/// Queue a local read/starred change to push on the next sync.
pub fn enqueue_sync(
    conn: &Connection,
    article_id: i64,
    field: &str,
    value: bool,
) -> AppResult<()> {
    conn.execute(
        "INSERT INTO sync_queue(article_id, field, value) VALUES (?1, ?2, ?3)
         ON CONFLICT(article_id, field) DO UPDATE SET value = excluded.value",
        params![article_id, field, value],
    )?;
    Ok(())
}

/// Article ids that still carry un-pushed local changes — their state must not
/// be overwritten by a pull until the change has been sent.
pub fn pending_sync_article_ids(conn: &Connection) -> AppResult<Vec<i64>> {
    let mut stmt = conn.prepare("SELECT DISTINCT article_id FROM sync_queue")?;
    let ids = stmt
        .query_map([], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ids)
}

/// One pushable change drained from the sync queue.
pub struct SyncEntry {
    pub article_id: i64,
    pub remote_id: String,
    pub field: String,
    pub value: bool,
}

/// Drain pushable queue entries. Only rows whose article already has a remote
/// id are returned and removed; the rest wait for a pull to assign one. The
/// caller MUST re-queue any entry whose push fails (see `requeue_sync`) so a
/// network blip never silently drops a local change.
pub fn take_sync_queue(conn: &Connection) -> AppResult<Vec<SyncEntry>> {
    let mut stmt = conn.prepare(
        "SELECT q.article_id, a.remote_id, q.field, q.value
         FROM sync_queue q JOIN articles a ON a.id = q.article_id
         WHERE a.remote_id IS NOT NULL",
    )?;
    let rows: Vec<SyncEntry> = stmt
        .query_map([], |r| {
            Ok(SyncEntry {
                article_id: r.get(0)?,
                remote_id: r.get(1)?,
                field: r.get(2)?,
                value: r.get::<_, i64>(3)? != 0,
            })
        })?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    conn.execute(
        "DELETE FROM sync_queue WHERE article_id IN
            (SELECT id FROM articles WHERE remote_id IS NOT NULL)",
        [],
    )?;
    Ok(rows)
}

/// Re-insert a queue entry whose push failed. Unlike `enqueue_sync` this does
/// not clobber a newer edit the user made on the same article during the sync.
pub fn requeue_sync(conn: &Connection, article_id: i64, field: &str, value: bool) -> AppResult<()> {
    conn.execute(
        "INSERT INTO sync_queue(article_id, field, value) VALUES (?1, ?2, ?3)
         ON CONFLICT(article_id, field) DO NOTHING",
        params![article_id, field, value],
    )?;
    Ok(())
}
