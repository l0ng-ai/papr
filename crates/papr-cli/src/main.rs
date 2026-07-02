//! papr — an agent-facing CLI over a local Papr RSS database.
//!
//! Reads, searches, triages and refreshes your feeds from the shell, emitting
//! token-efficient TOON on stdout. Designed to be driven by autonomous agents:
//! minimal default schemas, truncated long text with an escape hatch,
//! pre-computed aggregates, definitive empty states, idempotent mutations, and
//! structured errors (also on stdout). Diagnostics go to stderr.

mod setup;
mod toon;

use clap::{Parser, Subcommand};
use papr_core::db;
use papr_core::ingestion::{fetch, parse, refresh};
use papr_core::models::ArticleQuery;
use papr_core::sync;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use toon::Doc;

const APP_IDENTIFIER: &str = "com.thomas.papr";
const DESCRIPTION: &str = "Read, search and triage your Papr RSS feeds from the shell.";
const USER_AGENT: &str = concat!("papr-cli/", env!("CARGO_PKG_VERSION"));

/// Body truncation budget for a single `read`, and the tighter one when reading
/// several articles at once (keeping a batch affordable in tokens).
const READ_TRUNCATE: usize = 1500;
const BATCH_TRUNCATE: usize = 600;
/// Default rows for `list` / `search`.
const LIST_LIMIT: i64 = 30;
const SEARCH_LIMIT: i64 = 20;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            print!(
                "{}",
                render_error(&AxiError::runtime(format!("runtime: {e}")))
            );
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(run(cli)) {
        Ok(body) => {
            print!("{body}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            print!("{}", render_error(&err));
            err.exit_code()
        }
    }
}

// ───────────────────────────── CLI surface ─────────────────────────────

#[derive(Parser)]
#[command(name = "papr", version, about = DESCRIPTION, disable_help_subcommand = true)]
struct Cli {
    /// Path to the Papr SQLite database (defaults to the desktop app's data dir;
    /// override with the PAPR_DB env var).
    #[arg(long, global = true, env = "PAPR_DB", value_name = "PATH")]
    db: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// List subscribed feeds, grouped by folder, with per-feed unread counts.
    Feeds,
    /// List articles. Defaults to unread; combine filters freely.
    List(ListArgs),
    /// Read one or more articles as plain text (truncated unless --full).
    Read(ReadArgs),
    /// Full-text search across every article (FTS5).
    Search {
        /// The search query.
        query: String,
        /// Maximum matches to return.
        #[arg(long, default_value_t = SEARCH_LIMIT)]
        limit: i64,
    },
    /// Change article state: read | unread | star | unstar | later | unlater.
    Mark {
        /// New state to apply.
        #[arg(value_parser = ["read", "unread", "star", "unstar", "later", "unlater"])]
        state: String,
        /// One or more article ids.
        #[arg(required = true, value_name = "ID")]
        ids: Vec<i64>,
    },
    /// List tags with their article counts.
    Tags,
    /// Subscribe to a feed by URL (auto-discovers the feed, then fetches it).
    Subscribe {
        /// A feed URL, or a site URL to auto-discover a feed from.
        url: String,
        /// Place the new feed in this folder id.
        #[arg(long, value_name = "ID")]
        folder: Option<i64>,
    },
    /// Fetch new articles over the network (RSS feeds and newsletter mailboxes).
    Refresh {
        /// Only refresh this feed id (default: all feeds).
        #[arg(long, value_name = "ID")]
        feed: Option<i64>,
        /// Only refresh feeds in this folder id.
        #[arg(long, value_name = "ID")]
        folder: Option<i64>,
    },
    /// Unsubscribe from a feed, deleting it and all its articles.
    Unsubscribe {
        /// The feed id to remove.
        id: i64,
        /// Confirm this destructive action.
        #[arg(long)]
        yes: bool,
    },
    /// Mark every article in a view as read.
    #[command(name = "mark-all")]
    MarkAll(FilterArgs),
    /// Fetch and store the cleaned full text of an article (network).
    Extract {
        /// The article id.
        id: i64,
    },
    /// List folders with their feed counts.
    Folders,
    /// Manage folders (create / rename / delete).
    Folder {
        #[command(subcommand)]
        cmd: FolderCmd,
    },
    /// Manage a feed's settings (rename / move / interval).
    Feed {
        #[command(subcommand)]
        cmd: FeedCmd,
    },
    /// Manage tags (create / rename / color / delete / attach / detach).
    Tag {
        #[command(subcommand)]
        cmd: TagCmd,
    },
    /// List auto-tagging / filter rules.
    Rules,
    /// Manage filter rules (create / delete / enable / disable).
    Rule {
        #[command(subcommand)]
        cmd: RuleCmd,
    },
    /// List highlights (optionally for one article).
    Highlights {
        /// Only highlights on this article id.
        #[arg(long, value_name = "ID")]
        article: Option<i64>,
    },
    /// Manage highlights (create / note / color / delete).
    Highlight {
        #[command(subcommand)]
        cmd: HighlightCmd,
    },
    /// List configured email-newsletter sources.
    Newsletters,
    /// Manage newsletter sources (add / remove).
    Newsletter {
        #[command(subcommand)]
        cmd: NewsletterCmd,
    },
    /// Import or export feeds as OPML.
    Opml {
        #[command(subcommand)]
        cmd: OpmlCmd,
    },
    /// Read or change settings.
    Settings {
        #[command(subcommand)]
        cmd: SettingsCmd,
    },
    /// Show database storage statistics.
    Stats,
    /// Maintenance: retention cleanup, VACUUM, reset settings (destructive).
    Admin {
        #[command(subcommand)]
        cmd: AdminCmd,
    },
    /// Register a SessionStart integration so agents see your unread state at
    /// the start of every conversation (Claude Code / Codex / OpenCode).
    Setup {
        /// Which agent host to wire up.
        #[arg(long, default_value = "all", value_parser = ["all", "claude", "codex", "opencode"])]
        app: String,
    },
    /// FreshRSS / GReader sync (status / connect / disconnect / run).
    Sync {
        #[command(subcommand)]
        cmd: Option<SyncCmd>,
    },
}

#[derive(Subcommand)]
enum SyncCmd {
    /// Show the current sync connection (default).
    Status,
    /// Connect to a FreshRSS / Miniflux server (verifies credentials).
    Connect {
        #[arg(long)]
        url: String,
        #[arg(long)]
        user: String,
        #[arg(long)]
        password: String,
        #[arg(long, value_parser = ["freshrss", "miniflux"])]
        provider: Option<String>,
    },
    /// Forget the stored sync credentials.
    Disconnect {
        #[arg(long)]
        yes: bool,
    },
    /// Run a full sync now (push queued changes, pull subscriptions & state).
    Run,
}

/// A view selector shared by `mark-all` (and reusable by other bulk verbs).
#[derive(clap::Args)]
struct FilterArgs {
    #[arg(long, value_name = "ID")]
    feed: Option<i64>,
    #[arg(long, value_name = "ID")]
    folder: Option<i64>,
    #[arg(long, value_name = "ID")]
    tag: Option<i64>,
    #[arg(long)]
    starred: bool,
    #[arg(long)]
    later: bool,
}

#[derive(Subcommand)]
enum FolderCmd {
    /// Create a folder (idempotent on name).
    Create { name: String },
    /// Rename a folder.
    Rename { id: i64, name: String },
    /// Delete a folder (its feeds become folderless).
    Delete {
        id: i64,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum FeedCmd {
    /// Rename a feed.
    Rename { id: i64, title: String },
    /// Move a feed into a folder (omit --folder to make it folderless).
    Move {
        id: i64,
        #[arg(long, value_name = "ID")]
        folder: Option<i64>,
    },
    /// Set a feed's refresh interval in minutes (omit to follow the global one).
    Interval {
        id: i64,
        #[arg(long)]
        minutes: Option<i64>,
    },
}

#[derive(Subcommand)]
enum TagCmd {
    /// Create a tag.
    Create { name: String },
    /// Rename a tag.
    Rename { id: i64, name: String },
    /// Set a tag's palette colour.
    Color { id: i64, color: String },
    /// Delete a tag.
    Delete {
        id: i64,
        #[arg(long)]
        yes: bool,
    },
    /// Attach a tag to an article.
    Add {
        #[arg(value_name = "TAG_ID")]
        tag_id: i64,
        #[arg(value_name = "ARTICLE_ID")]
        article_id: i64,
    },
    /// Detach a tag from an article.
    Remove {
        #[arg(value_name = "TAG_ID")]
        tag_id: i64,
        #[arg(value_name = "ARTICLE_ID")]
        article_id: i64,
    },
}

#[derive(Subcommand)]
enum RuleCmd {
    /// Create a rule: match `query` keywords in `field`, then take `action`.
    Create {
        name: String,
        /// Comma-separated keywords (a match fires if any one is a substring).
        query: String,
        /// Which text to match.
        #[arg(long, default_value = "title", value_parser = ["title", "author", "content", "any"])]
        field: String,
        /// What to do on a match.
        #[arg(long, default_value = "skip", value_parser = ["skip", "read", "star"])]
        action: String,
        /// Scope the rule to one feed (default: all feeds).
        #[arg(long, value_name = "ID")]
        feed: Option<i64>,
    },
    /// Delete a rule.
    Delete {
        id: i64,
        #[arg(long)]
        yes: bool,
    },
    /// Enable a rule.
    Enable { id: i64 },
    /// Disable a rule.
    Disable { id: i64 },
}

#[derive(Subcommand)]
enum HighlightCmd {
    /// Create a highlight on an article from a quote.
    Create {
        #[arg(value_name = "ARTICLE_ID")]
        article: i64,
        quote: String,
        #[arg(long, default_value = "")]
        note: String,
        #[arg(long, default_value = "yellow")]
        color: String,
    },
    /// Set or replace a highlight's note.
    Note { id: i64, note: String },
    /// Set a highlight's colour.
    Color { id: i64, color: String },
    /// Delete a highlight.
    Delete {
        id: i64,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum NewsletterCmd {
    /// Add a newsletter source polled over IMAP.
    Add {
        #[arg(long)]
        title: String,
        #[arg(long)]
        host: String,
        #[arg(long, default_value_t = 993)]
        port: u16,
        #[arg(long)]
        user: String,
        #[arg(long)]
        password: String,
        #[arg(long, default_value = "INBOX")]
        folder: String,
    },
    /// Remove a newsletter source (deletes the feed and its articles).
    Remove {
        #[arg(value_name = "FEED_ID")]
        feed_id: i64,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum OpmlCmd {
    /// Import feeds from an OPML file.
    Import { file: PathBuf },
    /// Export all feeds as OPML (to stdout, or to --out FILE).
    Export {
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum SettingsCmd {
    /// Print a setting's value.
    Get { key: String },
    /// Set a setting's value.
    Set { key: String, value: String },
}

#[derive(Subcommand)]
enum AdminCmd {
    /// Delete read articles older than N days (keeps starred / read-later).
    Cleanup {
        days: i64,
        #[arg(long)]
        yes: bool,
    },
    /// Compact the database file (VACUUM).
    Vacuum {
        #[arg(long)]
        yes: bool,
    },
    /// Reset all settings to defaults.
    Reset {
        #[arg(long)]
        yes: bool,
    },
}

#[derive(clap::Args)]
struct ListArgs {
    /// Only this feed id.
    #[arg(long, value_name = "ID")]
    feed: Option<i64>,
    /// Only feeds in this folder id.
    #[arg(long, value_name = "ID")]
    folder: Option<i64>,
    /// Only articles carrying this tag id.
    #[arg(long, value_name = "ID")]
    tag: Option<i64>,
    /// Only starred articles.
    #[arg(long)]
    starred: bool,
    /// Only read-later articles.
    #[arg(long)]
    later: bool,
    /// Include already-read articles (default shows unread only).
    #[arg(long)]
    all: bool,
    /// Maximum rows to return.
    #[arg(long, default_value_t = LIST_LIMIT)]
    limit: i64,
    /// Skip this many rows (pagination).
    #[arg(long, default_value_t = 0)]
    offset: i64,
    /// Extra columns beyond the default {id,feed,title,flags,date}, comma-
    /// separated: author, url, snippet, type, feed_id, published.
    #[arg(long, value_name = "F1,F2")]
    fields: Option<String>,
}

#[derive(clap::Args)]
struct ReadArgs {
    /// Article ids to read. Omit to read by filter (--feed/--tag/...).
    ids: Vec<i64>,
    /// Show the complete body instead of a truncated preview.
    #[arg(long)]
    full: bool,
    /// Read the latest articles of this feed id.
    #[arg(long, value_name = "ID")]
    feed: Option<i64>,
    /// Read the latest articles in this folder id.
    #[arg(long, value_name = "ID")]
    folder: Option<i64>,
    /// Read the latest articles carrying this tag id.
    #[arg(long, value_name = "ID")]
    tag: Option<i64>,
    /// With a filter, restrict to unread articles.
    #[arg(long)]
    unread: bool,
    /// With a filter, how many articles to read.
    #[arg(long, default_value_t = 5)]
    limit: i64,
}

// ───────────────────────────── dispatch ─────────────────────────────

async fn run(cli: Cli) -> Result<String, AxiError> {
    // `setup` writes agent config files, never the database — dispatch it
    // before resolving the DB path so it still works in environments without
    // HOME/APPDATA (where `db_path` would otherwise fail first).
    if let Some(Cmd::Setup { app }) = &cli.cmd {
        return setup::run(app);
    }
    let path = db_path(&cli)?;
    match cli.cmd {
        None => cmd_home(&path),
        Some(Cmd::Feeds) => cmd_feeds(&path),
        Some(Cmd::List(args)) => cmd_list(&path, args),
        Some(Cmd::Read(args)) => cmd_read(&path, args),
        Some(Cmd::Search { query, limit }) => cmd_search(&path, &query, limit),
        Some(Cmd::Mark { state, ids }) => cmd_mark(&path, &state, &ids),
        Some(Cmd::Tags) => cmd_tags(&path),
        Some(Cmd::Subscribe { url, folder }) => cmd_subscribe(&path, &url, folder).await,
        Some(Cmd::Refresh { feed, folder }) => cmd_refresh(&path, feed, folder).await,
        Some(Cmd::Unsubscribe { id, yes }) => cmd_unsubscribe(&path, id, yes),
        Some(Cmd::MarkAll(f)) => cmd_mark_all(&path, &f),
        Some(Cmd::Extract { id }) => cmd_extract(&path, id).await,
        Some(Cmd::Folders) => cmd_folders(&path),
        Some(Cmd::Folder { cmd }) => cmd_folder(&path, cmd),
        Some(Cmd::Feed { cmd }) => cmd_feed(&path, cmd),
        Some(Cmd::Tag { cmd }) => cmd_tag(&path, cmd),
        Some(Cmd::Rules) => cmd_rules(&path),
        Some(Cmd::Rule { cmd }) => cmd_rule(&path, cmd),
        Some(Cmd::Highlights { article }) => cmd_highlights(&path, article),
        Some(Cmd::Highlight { cmd }) => cmd_highlight(&path, cmd),
        Some(Cmd::Newsletters) => cmd_newsletters(&path),
        Some(Cmd::Newsletter { cmd }) => cmd_newsletter(&path, cmd),
        Some(Cmd::Opml { cmd }) => cmd_opml(&path, cmd),
        Some(Cmd::Settings { cmd }) => cmd_settings(&path, cmd),
        Some(Cmd::Stats) => cmd_stats(&path),
        Some(Cmd::Admin { cmd }) => cmd_admin(&path, cmd),
        Some(Cmd::Setup { .. }) => unreachable!("setup is dispatched before db_path"),
        Some(Cmd::Sync { cmd }) => cmd_sync(&path, cmd.unwrap_or(SyncCmd::Status)).await,
    }
}

/// Guard a destructive action behind `--yes`, with a clear re-run hint.
fn require_yes(yes: bool, action: &str, rerun: &str) -> Result<(), AxiError> {
    if yes {
        return Ok(());
    }
    Err(AxiError::usage(
        format!("{action} is destructive and needs confirmation"),
        vec![format!("Run `{rerun} --yes` to proceed")],
    ))
}

/// Build an `ArticleQuery` from a bare view selector (no unread coupling).
fn filter_query(f: &FilterArgs) -> ArticleQuery {
    if let Some(id) = f.feed {
        ArticleQuery::Feed(id)
    } else if let Some(id) = f.folder {
        ArticleQuery::Folder(id)
    } else if let Some(id) = f.tag {
        ArticleQuery::Tag(id)
    } else if f.starred {
        ArticleQuery::Starred
    } else if f.later {
        ArticleQuery::ReadLater
    } else {
        ArticleQuery::All
    }
}

/// A SQLite `LIMIT` of a negative value means "no limit" and zero returns
/// nothing — both footguns for an agent that mistyped a flag. Clamp to `>= 1`.
fn clamp_limit(n: i64) -> i64 {
    n.max(1)
}

/// Pagination offsets below zero are meaningless; clamp to `>= 0`.
fn clamp_offset(n: i64) -> i64 {
    n.max(0)
}

/// The view selectors resolve to a single `ArticleQuery` via a first-match
/// chain, so passing several silently honours just one — broadening a `list`
/// and, worse, the set a `mark-all` mutates. Reject the ambiguity outright.
fn ensure_single_filter(selectors: &[(&str, bool)]) -> Result<(), AxiError> {
    let set: Vec<&str> = selectors
        .iter()
        .filter(|(_, on)| *on)
        .map(|(n, _)| *n)
        .collect();
    if set.len() > 1 {
        return Err(AxiError::usage(
            format!("filters {} cannot be combined", set.join(" + ")),
            vec!["Pass exactly one of --feed / --folder / --tag / --starred / --later".into()],
        ));
    }
    Ok(())
}

/// Whether a settings key holds a credential, so its value is masked in `get`
/// and not echoed back by `set` — keeping secrets out of agent transcripts and
/// terminal scrollback.
fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    ["api_key", "password", "secret", "token"]
        .iter()
        .any(|needle| k.contains(needle))
}

/// A masked preview of a secret: enough to confirm *which* value is set
/// without disclosing it.
fn mask_secret(value: &str) -> String {
    let n = value.chars().count();
    match n {
        0 => "(empty)".to_string(),
        1..=6 => format!("(set, {n} chars)"),
        _ => {
            let tail: String = value.chars().skip(n - 4).collect();
            format!("…{tail} (set, {n} chars)")
        }
    }
}

/// A single-line confirmation, carried in an `ok` field so an agent reads the
/// outcome from a stable key; idempotent mutations acknowledge and exit 0.
fn ok_line(text: String) -> Result<String, AxiError> {
    Ok(Doc::new().set("ok", text).into_toon())
}

// ───────────────────────────── commands ─────────────────────────────

/// No-args home view: identify the tool, then show live unread state so an
/// agent can act immediately (content first: live state, not a manual).
fn cmd_home(path: &Path) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let (unread, starred, later) = db::smart_counts(&conn).map_err(db_err)?;
    let feeds = db::list_feeds(&conn).map_err(db_err)?;

    // Most recent unread first; fall back to recent reads when the inbox is clear.
    let recent =
        db::list_articles(&conn, &ArticleQuery::All, true, None, false, 10, 0).map_err(db_err)?;
    let inbox_clear = recent.is_empty();
    let recent = if inbox_clear {
        db::list_articles(&conn, &ArticleQuery::All, false, None, false, 10, 0).map_err(db_err)?
    } else {
        recent
    };

    let mut d = Doc::new();
    d.set("bin", collapse_home(&current_exe_path()));
    d.set("description", DESCRIPTION);
    d.set("db", collapse_home(&path.display().to_string()));
    d.set("unread", unread);
    d.set("starred", starred);
    d.set("later", later);
    d.set("feeds", feeds.len());
    if inbox_clear {
        d.set("inbox", "0 unread — all caught up");
        d.set("recent", article_rows(&recent, &[]));
    } else {
        d.set("articles", article_rows(&recent, &[]));
    }
    let mut help = Vec::new();
    // The home view shows only the most recent unread; if more exist, tell the
    // agent how to see the rest instead of leaving the count ambiguous.
    if !inbox_clear && unread > recent.len() as i64 {
        help.push(format!("Run `papr list` to see all {unread} unread"));
    }
    help.extend([
        "Run `papr read <id>` to read an article's full text".into(),
        "Run `papr list --feed <id>` to list one feed's articles".into(),
        "Run `papr search \"<query>\"` to search every article".into(),
        "Run `papr refresh` to fetch new articles".into(),
    ]);
    d.help(help);
    Ok(d.into_toon())
}

fn cmd_feeds(path: &Path) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let feeds = db::list_feeds(&conn).map_err(db_err)?;
    let folders = db::list_folders(&conn).map_err(db_err)?;

    if feeds.is_empty() {
        let mut d = Doc::new();
        d.set("feeds", json!([]));
        d.help(vec![
            "Run `papr subscribe <url>` to add your first feed".into()
        ]);
        return Ok(d.into_toon());
    }

    let total_unread: i64 = feeds.iter().map(|f| f.unread_count).sum();
    let mut d = Doc::new();
    d.set("feeds", feeds.len());
    d.set("unread", total_unread);

    // Group by folder so an agent sees the organisation; folderless feeds last.
    let folder_name = |id: Option<i64>| -> Option<String> {
        id.and_then(|fid| folders.iter().find(|f| f.id == fid).map(|f| f.name.clone()))
    };
    let mut groups = serde_json::Map::new();
    for folder in &folders {
        let group: Vec<_> = feeds
            .iter()
            .filter(|f| f.folder_id == Some(folder.id))
            .collect();
        if !group.is_empty() {
            groups.insert(folder.name.clone(), feed_rows(&group));
        }
    }
    let loose: Vec<_> = feeds
        .iter()
        .filter(|f| folder_name(f.folder_id).is_none())
        .collect();
    if !loose.is_empty() {
        groups.insert("(no folder)".to_string(), feed_rows(&loose));
    }
    d.set("by_folder", Value::Object(groups));

    d.help(vec![
        "Run `papr list --feed <id>` to list a feed's articles".into(),
        "Run `papr refresh --feed <id>` to fetch one feed".into(),
        "Run `papr subscribe <url>` to add a feed".into(),
    ]);
    Ok(d.into_toon())
}

fn cmd_list(path: &Path, args: ListArgs) -> Result<String, AxiError> {
    ensure_single_filter(&[
        ("--feed", args.feed.is_some()),
        ("--folder", args.folder.is_some()),
        ("--tag", args.tag.is_some()),
        ("--starred", args.starred),
        ("--later", args.later),
    ])?;
    let extra = match &args.fields {
        Some(spec) => parse_fields(spec)?,
        None => Vec::new(),
    };
    let conn = open_ro(path)?;
    let (query, unread_only) = resolve_query(&args);
    let limit = clamp_limit(args.limit);
    let offset = clamp_offset(args.offset);
    let rows = db::list_articles(&conn, &query, unread_only, None, false, limit, offset)
        .map_err(db_err)?;
    let total = count_articles(&conn, &query, unread_only).map_err(db_err)?;

    let shown = rows.len();
    let mut d = Doc::new();
    d.set(
        "count",
        format!("{shown} of {total} {}", scope_label(&query, unread_only)),
    );
    d.set("articles", article_rows(&rows, &extra));

    let help = if rows.is_empty() {
        vec!["Run `papr refresh` to fetch new articles".into()]
    } else {
        let mut help = vec![
            "Run `papr read <id>` to read an article's full text".into(),
            "Run `papr mark read <id>` to mark an article read".into(),
        ];
        if offset + (shown as i64) < total {
            let next = offset + limit;
            let filters = replay_filters(&args);
            let cmd = if filters.is_empty() {
                format!("papr list --offset {next}")
            } else {
                format!("papr list --offset {next} {filters}")
            };
            help.push(format!("Run `{cmd}` for the next page"));
        }
        help
    };
    d.help(help);
    Ok(d.into_toon())
}

fn cmd_read(path: &Path, args: ReadArgs) -> Result<String, AxiError> {
    ensure_single_filter(&[
        ("--feed", args.feed.is_some()),
        ("--folder", args.folder.is_some()),
        ("--tag", args.tag.is_some()),
    ])?;
    let conn = open_ro(path)?;

    // Resolve the id set: explicit ids win; otherwise pull the latest by filter.
    let ids: Vec<i64> = if !args.ids.is_empty() {
        args.ids.clone()
    } else if args.feed.is_some() || args.folder.is_some() || args.tag.is_some() {
        let query = if let Some(f) = args.feed {
            ArticleQuery::Feed(f)
        } else if let Some(f) = args.folder {
            ArticleQuery::Folder(f)
        } else {
            ArticleQuery::Tag(args.tag.unwrap())
        };
        db::list_articles(
            &conn,
            &query,
            args.unread,
            None,
            false,
            clamp_limit(args.limit),
            0,
        )
        .map_err(db_err)?
        .into_iter()
        .map(|a| a.id)
        .collect()
    } else {
        return Err(AxiError::usage(
            "read needs an article id or a filter",
            vec![
                "Run `papr read <id> [<id>...]` to read by id".into(),
                "Run `papr read --feed <id> --limit 5` to read a feed's latest".into(),
            ],
        ));
    };

    if ids.is_empty() {
        let mut d = Doc::new();
        d.set("articles", json!([]));
        d.help(vec!["Run `papr list` to find article ids".into()]);
        return Ok(d.into_toon());
    }

    let batch = ids.len() > 1;
    let budget = if batch { BATCH_TRUNCATE } else { READ_TRUNCATE };
    let mut any_truncated = false;
    let mut articles: Vec<Value> = Vec::new();
    for id in &ids {
        let detail = match db::get_article(&conn, *id) {
            Ok(d) => d,
            Err(_) => {
                // `tags` as an array (here empty) keeps every element non-tabular
                // so the encoder renders an expanded block, not a one-row table.
                articles.push(json!({ "id": id, "error": "not found", "tags": [] }));
                continue;
            }
        };
        let (_title, text) = db::article_text(&conn, *id).map_err(db_err)?;
        let (shown, total_chars, truncated) =
            truncate(&text, if args.full { usize::MAX } else { budget });
        any_truncated |= truncated;
        let body = if truncated {
            format!("{shown}\n... (truncated, {total_chars} chars total)")
        } else {
            shown
        };
        let tags: Vec<String> = detail.tags.iter().map(|t| t.name.clone()).collect();
        articles.push(json!({
            "id": detail.id,
            "feed": detail.feed_title,
            "title": detail.title,
            "author": detail.author,
            "url": detail.url,
            "published": detail.published_at,
            "state": detail_flags(&detail),
            "tags": tags,
            "text": body,
        }));
    }

    let mut d = Doc::new();
    if batch {
        d.set("count", ids.len());
    }
    d.set("articles", Value::Array(articles));
    if any_truncated && !args.full {
        d.help(vec![
            "Run `papr read <id> --full` to see the complete text".into()
        ]);
    }
    Ok(d.into_toon())
}

fn cmd_search(path: &Path, query: &str, limit: i64) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let hits = db::search_articles_for_rag(&conn, query, clamp_limit(limit)).map_err(db_err)?;
    let mut d = Doc::new();
    d.set("query", query);
    d.set("count", hits.len());
    let rows: Vec<Value> = hits
        .iter()
        .map(|(id, title, feed)| json!({ "id": id, "feed": feed, "title": cap(title, 80) }))
        .collect();
    d.set("matches", Value::Array(rows));
    d.help(vec![if hits.is_empty() {
        "Run `papr refresh` to fetch new articles, then search again".into()
    } else {
        "Run `papr read <id>` to read a match in full".into()
    }]);
    Ok(d.into_toon())
}

fn cmd_mark(path: &Path, state: &str, ids: &[i64]) -> Result<String, AxiError> {
    let conn = open_rw(path)?;
    let mut rows: Vec<Value> = Vec::new();
    for &id in ids {
        // Read current state for idempotency: an already-applied change is a
        // no-op with exit 0, not an error.
        let current = match db::get_article(&conn, id) {
            Ok(d) => d,
            Err(_) => {
                rows.push(json!({ "id": id, "result": "not found" }));
                continue;
            }
        };
        let (already, result) = match state {
            "read" => (current.is_read, db::set_read(&conn, id, true)),
            "unread" => (!current.is_read, db::set_read(&conn, id, false)),
            "star" => (current.is_starred, db::set_starred(&conn, id, true)),
            "unstar" => (!current.is_starred, db::set_starred(&conn, id, false)),
            "later" => (current.read_later, db::set_read_later(&conn, id, true)),
            "unlater" => (!current.read_later, db::set_read_later(&conn, id, false)),
            _ => unreachable!("clap restricts the state value"),
        };
        result.map_err(db_err)?;
        let outcome = if already {
            format!("already {state} (no-op)")
        } else {
            state.to_string()
        };
        rows.push(json!({ "id": id, "result": outcome }));
    }
    Ok(Doc::new().set("marked", Value::Array(rows)).into_toon())
}

fn cmd_tags(path: &Path) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let tags = db::list_tags(&conn).map_err(db_err)?;
    let mut d = Doc::new();
    let rows: Vec<Value> = tags
        .iter()
        .map(|t| json!({ "id": t.id, "name": t.name, "articles": t.article_count }))
        .collect();
    d.set("tags", Value::Array(rows));
    if !tags.is_empty() {
        d.help(vec![
            "Run `papr list --tag <id>` to list a tag's articles".into()
        ]);
    }
    Ok(d.into_toon())
}

async fn cmd_subscribe(path: &Path, url: &str, folder: Option<i64>) -> Result<String, AxiError> {
    let conn = open_rw(path)?;
    let client = http_client()?;

    // Fetch the target. If it parses as a feed, use it directly; otherwise treat
    // it as an HTML page and auto-discover the feed link.
    let (bytes, _etag, final_url) = fetch::get(&client, url)
        .await
        .map_err(|e| clean_err(&format!("could not fetch {url}"), e))?;

    let (feed_url, parsed) = match parse::parse_feed(&bytes, &final_url) {
        Ok(parsed) => (final_url.clone(), parsed),
        Err(_) => {
            // Not a feed document — look for <link rel=alternate> feeds.
            let html = String::from_utf8_lossy(&bytes);
            let candidates = parse::discover_feeds(&html, &final_url);
            let Some(candidate) = candidates.into_iter().next() else {
                return Err(AxiError::runtime_help(
                    format!("no feed found at {url}"),
                    vec!["Pass the feed URL directly if you have it".into()],
                ));
            };
            let (fbytes, _e, furl) = fetch::get(&client, &candidate)
                .await
                .map_err(|e| clean_err(&format!("could not fetch {candidate}"), e))?;
            let parsed = parse::parse_feed(&fbytes, &furl)
                .map_err(|e| AxiError::runtime(format!("discovered feed did not parse: {e}")))?;
            (furl, parsed)
        }
    };

    // Idempotent: re-subscribing to a known feed is a no-op, not an error.
    if let Some(existing) = db::find_feed_by_url(&conn, &feed_url).map_err(db_err)? {
        let mut d = Doc::new();
        d.set("ok", format!("feed #{existing} already subscribed (no-op)"));
        d.help(vec![format!(
            "Run `papr list --feed {existing}` to read it"
        )]);
        return Ok(d.into_toon());
    }

    let source_type =
        parse::refine_source_type(papr_core::models::SourceType::Rss, &parsed, &feed_url);
    let title = parsed.title.clone().unwrap_or_else(|| feed_url.clone());
    let feed_id = db::insert_feed(
        &conn,
        &feed_url,
        parsed.site_url.as_deref(),
        &title,
        parsed.description.as_deref(),
        source_type,
        folder,
    )
    .map_err(db_err)?;

    // Initial population: ingest the articles we already parsed.
    // A failure to load rules is a real DB error — surface it rather than
    // silently ingesting with no rules applied.
    let rules = db::active_rules(&conn).map_err(db_err)?;
    let dedup = db::setting_flag(&conn, "dedup_enabled", false);
    let mut new_count = 0usize;
    let mut failed = 0usize;
    for article in &parsed.articles {
        // A single malformed item shouldn't abort the whole subscription, but
        // its failure is counted and reported, not hidden behind a clean total.
        match db::upsert_article(&conn, feed_id, article, dedup, &rules) {
            Ok(true) => new_count += 1,
            Ok(false) => {}
            Err(_) => failed += 1,
        }
    }
    let _ = db::touch_feed(&conn, feed_id);

    let mut d = Doc::new();
    let mut feed = serde_json::Map::new();
    feed.insert("id".into(), json!(feed_id));
    feed.insert("title".into(), json!(title));
    feed.insert("url".into(), json!(feed_url));
    feed.insert("type".into(), json!(source_type.as_str()));
    feed.insert("articles".into(), json!(new_count));
    if failed > 0 {
        feed.insert("failed".into(), json!(failed));
    }
    d.set("feed", Value::Object(feed));
    d.help(vec![
        format!("Run `papr list --feed {feed_id}` to read it"),
        format!("Run `papr refresh --feed {feed_id}` to fetch more later"),
    ]);
    Ok(d.into_toon())
}

async fn cmd_refresh(
    path: &Path,
    feed: Option<i64>,
    folder: Option<i64>,
) -> Result<String, AxiError> {
    ensure_single_filter(&[("--feed", feed.is_some()), ("--folder", folder.is_some())])?;
    let conn = db::open(path).map_err(db_err)?;
    let dbm = tokio::sync::Mutex::new(conn);
    let client = http_client()?;
    let (scope, label) = match (feed, folder) {
        (Some(id), _) => (refresh::RefreshScope::Feed(id), format!("feed {id}")),
        (_, Some(id)) => (refresh::RefreshScope::Folder(id), format!("folder {id}")),
        _ => (refresh::RefreshScope::All, "all".to_string()),
    };

    // Progress is diagnostic — it goes to stderr so stdout stays pure data.
    let summary = refresh::refresh_core(&dbm, &client, scope, |event| {
        use papr_core::models::RefreshProgress::*;
        match event {
            Started { total } => eprintln!("refreshing {total} feed(s)…"),
            FeedDone {
                feed_id,
                new_articles,
                error,
            } => {
                if let Some(e) = error {
                    eprintln!("  feed {feed_id}: error — {e}");
                } else if new_articles > 0 {
                    eprintln!("  feed {feed_id}: {new_articles} new");
                }
            }
            Finished { new_articles } => eprintln!("done — {new_articles} new article(s)"),
        }
    })
    .await
    .map_err(db_err)?;

    let mut d = Doc::new();
    d.set(
        "refresh",
        json!({
            "scope": label,
            "new": summary.new_articles,
        }),
    );
    if summary.new_articles > 0 {
        d.help(vec!["Run `papr list` to see what's new".into()]);
    }
    Ok(d.into_toon())
}

// ─────────────────────────────── sync ───────────────────────────────

async fn cmd_sync(path: &Path, cmd: SyncCmd) -> Result<String, AxiError> {
    let conn = db::open(path).map_err(db_err)?;
    let dbm = tokio::sync::Mutex::new(conn);
    let client = http_client()?;
    match cmd {
        SyncCmd::Status => {
            let info = sync::connected_url(&dbm)
                .await
                .map_err(|e| clean_err("sync status", e))?;
            let mut d = Doc::new();
            match info {
                Some((url, provider)) => {
                    d.set(
                        "sync",
                        json!({ "connected": true, "provider": provider, "url": url }),
                    );
                    d.help(vec!["Run `papr sync run` to reconcile now".into()]);
                }
                None => {
                    d.set("sync", json!({ "connected": false }));
                    d.help(vec![
                        "Run `papr sync connect --url <u> --user <u> --password <p>` to connect"
                            .into(),
                    ]);
                }
            }
            Ok(d.into_toon())
        }
        SyncCmd::Connect {
            url,
            user,
            password,
            provider,
        } => {
            sync::connect(
                &dbm,
                &client,
                &url,
                &user,
                &password,
                provider.as_deref(),
                None,
            )
            .await
            .map_err(|e| clean_err("sync connect failed", e))?;
            let mut d = Doc::new();
            d.set("ok", format!("connected to {url}"));
            d.help(vec!["Run `papr sync run` to reconcile now".into()]);
            Ok(d.into_toon())
        }
        SyncCmd::Disconnect { yes } => {
            require_yes(yes, "sync disconnect", "papr sync disconnect")?;
            sync::disconnect(&dbm).await.map_err(db_err)?;
            ok_line("sync: disconnected".into())
        }
        SyncCmd::Run => {
            let connected = sync::connected_url(&dbm).await.map_err(db_err)?.is_some();
            if !connected {
                let mut d = Doc::new();
                d.set("ok", "not connected (no-op)");
                d.help(vec!["Run `papr sync connect ...` first".into()]);
                return Ok(d.into_toon());
            }
            eprintln!("syncing…");
            let n = sync::sync_now(&dbm, &client)
                .await
                .map_err(|e| clean_err("sync failed", e))?;
            Ok(Doc::new()
                .set("sync", json!({ "reconciled": n }))
                .into_toon())
        }
    }
}

// ─────────────────────── feeds / folders management ───────────────────────

fn feed_title(conn: &Connection, id: i64) -> Option<String> {
    conn.query_row("SELECT title FROM feeds WHERE id = ?1", [id], |r| r.get(0))
        .ok()
}

fn cmd_unsubscribe(path: &Path, id: i64, yes: bool) -> Result<String, AxiError> {
    let conn = open_rw(path)?;
    let Some(title) = feed_title(&conn, id) else {
        return ok_line(format!("feed: #{id} not found (no-op)"));
    };
    require_yes(yes, "unsubscribe", &format!("papr unsubscribe {id}"))?;
    db::delete_feed(&conn, id).map_err(db_err)?;
    ok_line(format!("unsubscribed: #{id} {title}"))
}

fn cmd_mark_all(path: &Path, f: &FilterArgs) -> Result<String, AxiError> {
    ensure_single_filter(&[
        ("--feed", f.feed.is_some()),
        ("--folder", f.folder.is_some()),
        ("--tag", f.tag.is_some()),
        ("--starred", f.starred),
        ("--later", f.later),
    ])?;
    let conn = open_rw(path)?;
    let query = filter_query(f);
    let n = db::mark_all_read(&conn, &query, true).map_err(db_err)?;
    ok_line(format!("marked {n} article(s) read"))
}

async fn cmd_extract(path: &Path, id: i64) -> Result<String, AxiError> {
    let conn = open_rw(path)?;
    let detail = db::get_article(&conn, id)
        .map_err(|_| AxiError::runtime(format!("article #{id} not found")))?;
    let Some(url) = detail.url.clone() else {
        return Err(AxiError::runtime(format!(
            "article #{id} has no URL to extract"
        )));
    };
    let client = http_client()?;
    let (bytes, _e, final_url) = fetch::get(&client, &url)
        .await
        .map_err(|e| clean_err(&format!("could not fetch {url}"), e))?;
    let html = String::from_utf8_lossy(&bytes);
    let cleaned = papr_core::extraction::extract_article(&html, &final_url)
        .map_err(|e| AxiError::runtime(format!("extraction failed: {e}")))?;
    let image = papr_core::extraction::lead_image(&html, &final_url);
    db::set_extracted_html(&conn, id, &cleaned, image.as_deref()).map_err(db_err)?;
    let chars = papr_core::sanitize::html_to_text(&cleaned).chars().count();
    let mut d = Doc::new();
    d.set("extracted", json!({ "id": id, "chars": chars }));
    d.help(vec![format!(
        "Run `papr read {id} --full` to read the extracted text"
    )]);
    Ok(d.into_toon())
}

fn cmd_folders(path: &Path) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let folders = db::list_folders(&conn).map_err(db_err)?;
    let feeds = db::list_feeds(&conn).map_err(db_err)?;
    let rows: Vec<Value> = folders
        .iter()
        .map(|f| {
            let n = feeds.iter().filter(|x| x.folder_id == Some(f.id)).count();
            json!({ "id": f.id, "name": f.name, "feeds": n })
        })
        .collect();
    let mut d = Doc::new();
    d.set("folders", Value::Array(rows));
    if !folders.is_empty() {
        d.help(vec![
            "Run `papr list --folder <id>` to list a folder's articles".into(),
        ]);
    }
    Ok(d.into_toon())
}

fn cmd_folder(path: &Path, cmd: FolderCmd) -> Result<String, AxiError> {
    let conn = open_rw(path)?;
    match cmd {
        FolderCmd::Create { name } => {
            let id = db::create_folder(&conn, &name).map_err(db_err)?;
            ok_line(format!("folder: #{id} {name}"))
        }
        FolderCmd::Rename { id, name } => {
            db::rename_folder(&conn, id, &name).map_err(db_err)?;
            ok_line(format!("folder: #{id} renamed to {name}"))
        }
        FolderCmd::Delete { id, yes } => {
            require_yes(yes, "folder delete", &format!("papr folder delete {id}"))?;
            db::delete_folder(&conn, id).map_err(db_err)?;
            ok_line(format!("folder: #{id} deleted"))
        }
    }
}

fn cmd_feed(path: &Path, cmd: FeedCmd) -> Result<String, AxiError> {
    let conn = open_rw(path)?;
    match cmd {
        FeedCmd::Rename { id, title } => {
            db::rename_feed(&conn, id, &title).map_err(db_err)?;
            ok_line(format!("feed: #{id} renamed to {title}"))
        }
        FeedCmd::Move { id, folder } => {
            db::move_feed(&conn, id, folder).map_err(db_err)?;
            match folder {
                Some(f) => ok_line(format!("feed: #{id} moved to folder {f}")),
                None => ok_line(format!("feed: #{id} moved out of any folder")),
            }
        }
        FeedCmd::Interval { id, minutes } => {
            db::set_feed_refresh_interval(&conn, id, minutes).map_err(db_err)?;
            match minutes {
                Some(m) => ok_line(format!("feed: #{id} refresh interval set to {m} min")),
                None => ok_line(format!("feed: #{id} now follows the global interval")),
            }
        }
    }
}

// ─────────────────────────────── tags ───────────────────────────────

fn cmd_tag(path: &Path, cmd: TagCmd) -> Result<String, AxiError> {
    let conn = open_rw(path)?;
    match cmd {
        TagCmd::Create { name } => {
            let id = db::create_tag(&conn, &name).map_err(db_err)?;
            ok_line(format!("tag: #{id} {name}"))
        }
        TagCmd::Rename { id, name } => {
            db::rename_tag(&conn, id, &name).map_err(db_err)?;
            ok_line(format!("tag: #{id} renamed to {name}"))
        }
        TagCmd::Color { id, color } => {
            db::set_tag_color(&conn, id, &color).map_err(db_err)?;
            ok_line(format!("tag: #{id} colour {color}"))
        }
        TagCmd::Delete { id, yes } => {
            require_yes(yes, "tag delete", &format!("papr tag delete {id}"))?;
            db::delete_tag(&conn, id).map_err(db_err)?;
            ok_line(format!("tag: #{id} deleted"))
        }
        TagCmd::Add { tag_id, article_id } => {
            db::set_article_tag(&conn, article_id, tag_id, true).map_err(db_err)?;
            ok_line(format!("tagged: article {article_id} += tag {tag_id}"))
        }
        TagCmd::Remove { tag_id, article_id } => {
            db::set_article_tag(&conn, article_id, tag_id, false).map_err(db_err)?;
            ok_line(format!("untagged: article {article_id} -= tag {tag_id}"))
        }
    }
}

// ─────────────────────────────── rules ───────────────────────────────

fn cmd_rules(path: &Path) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let rules = db::list_rules(&conn).map_err(db_err)?;
    let rows: Vec<Value> = rules
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "name": r.name,
                "enabled": r.enabled,
                "field": r.field,
                "query": r.query,
                "action": r.action,
                "feed": r.feed_id.map(|f| f.to_string()).unwrap_or_else(|| "all".into()),
            })
        })
        .collect();
    let mut d = Doc::new();
    d.set("rules", Value::Array(rows));
    d.help(vec![if rules.is_empty() {
        "Run `papr rule create <name> <keywords>` to add one".into()
    } else {
        "Run `papr rule disable <id>` to turn a rule off".into()
    }]);
    Ok(d.into_toon())
}

fn cmd_rule(path: &Path, cmd: RuleCmd) -> Result<String, AxiError> {
    let conn = open_rw(path)?;
    match cmd {
        RuleCmd::Create {
            name,
            query,
            field,
            action,
            feed,
        } => {
            let id =
                db::create_rule(&conn, &name, feed, &field, &query, &action).map_err(db_err)?;
            ok_line(format!("rule: #{id} {name} ({field} ~ {query} → {action})"))
        }
        RuleCmd::Delete { id, yes } => {
            require_yes(yes, "rule delete", &format!("papr rule delete {id}"))?;
            db::delete_rule(&conn, id).map_err(db_err)?;
            ok_line(format!("rule: #{id} deleted"))
        }
        RuleCmd::Enable { id } => set_rule_enabled(&conn, id, true),
        RuleCmd::Disable { id } => set_rule_enabled(&conn, id, false),
    }
}

fn set_rule_enabled(conn: &Connection, id: i64, on: bool) -> Result<String, AxiError> {
    let rules = db::list_rules(conn).map_err(db_err)?;
    let Some(r) = rules.into_iter().find(|r| r.id == id) else {
        return Err(AxiError::runtime(format!("rule #{id} not found")));
    };
    if r.enabled == on {
        return ok_line(format!(
            "rule: #{id} already {} (no-op)",
            if on { "enabled" } else { "disabled" }
        ));
    }
    db::update_rule(
        conn, id, &r.name, on, r.feed_id, &r.field, &r.query, &r.action,
    )
    .map_err(db_err)?;
    ok_line(format!(
        "rule: #{id} {}",
        if on { "enabled" } else { "disabled" }
    ))
}

// ───────────────────────────── highlights ─────────────────────────────

fn cmd_highlights(path: &Path, article: Option<i64>) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let items = match article {
        Some(id) => db::list_highlights(&conn, id).map_err(db_err)?,
        None => db::list_all_highlights(&conn).map_err(db_err)?,
    };
    let rows: Vec<Value> = items
        .iter()
        .map(|h| {
            json!({
                "id": h.id,
                "article": h.article_id,
                "quote": cap(&h.quote, 60),
                "color": h.color,
                "note": cap(&h.note, 40),
            })
        })
        .collect();
    Ok(Doc::new().set("highlights", Value::Array(rows)).into_toon())
}

fn cmd_highlight(path: &Path, cmd: HighlightCmd) -> Result<String, AxiError> {
    let conn = open_rw(path)?;
    match cmd {
        HighlightCmd::Create {
            article,
            quote,
            note,
            color,
        } => {
            let h = db::NewHighlight {
                article_id: article,
                quote: &quote,
                prefix: "",
                suffix: "",
                text_offset: 0,
                color: &color,
                note: &note,
            };
            let id = db::insert_highlight(&conn, &h).map_err(db_err)?;
            ok_line(format!("highlight: #{id} on article {article}"))
        }
        HighlightCmd::Note { id, note } => {
            db::update_highlight_note(&conn, id, &note).map_err(db_err)?;
            ok_line(format!("highlight: #{id} note updated"))
        }
        HighlightCmd::Color { id, color } => {
            db::set_highlight_color(&conn, id, &color).map_err(db_err)?;
            ok_line(format!("highlight: #{id} colour {color}"))
        }
        HighlightCmd::Delete { id, yes } => {
            require_yes(
                yes,
                "highlight delete",
                &format!("papr highlight delete {id}"),
            )?;
            db::delete_highlight(&conn, id).map_err(db_err)?;
            ok_line(format!("highlight: #{id} deleted"))
        }
    }
}

// ──────────────────────────── newsletters ────────────────────────────

fn cmd_newsletters(path: &Path) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let rows = db::list_newsletter_sources(&conn).map_err(db_err)?;
    let table: Vec<Value> = rows
        .iter()
        .map(|n| {
            json!({
                "feed": n.feed_id,
                "title": cap(&n.title, 32),
                "host": format!("{}:{}", n.host, n.port),
                "user": n.username,
                "folder": n.folder,
            })
        })
        .collect();
    let mut d = Doc::new();
    d.set("newsletters", Value::Array(table));
    if rows.is_empty() {
        d.help(vec![
            "Run `papr newsletter add --title .. --host .. --user .. --password ..` to add one"
                .into(),
        ]);
    }
    Ok(d.into_toon())
}

fn cmd_newsletter(path: &Path, cmd: NewsletterCmd) -> Result<String, AxiError> {
    let conn = open_rw(path)?;
    match cmd {
        NewsletterCmd::Add {
            title,
            host,
            port,
            user,
            password,
            folder,
        } => {
            let cfg = papr_core::ingestion::newsletter::NewsletterConfig {
                host: host.clone(),
                port,
                username: user.clone(),
                password,
                folder: folder.clone(),
            };
            // Synthetic, stable feed URL so the source de-dupes like an RSS feed.
            let feed_url = format!("newsletter://{user}@{host}/{folder}");
            if let Some(existing) = db::find_feed_by_url(&conn, &feed_url).map_err(db_err)? {
                return ok_line(format!(
                    "newsletter: #{existing} already configured (no-op)"
                ));
            }
            let id =
                db::insert_newsletter_source(&conn, &feed_url, &title, &cfg).map_err(db_err)?;
            let mut d = Doc::new();
            d.set(
                "newsletter",
                json!({ "feed": id, "title": title, "host": format!("{host}:{port}") }),
            );
            d.help(vec![format!(
                "Run `papr refresh --feed {id}` to poll it now"
            )]);
            Ok(d.into_toon())
        }
        NewsletterCmd::Remove { feed_id, yes } => {
            require_yes(
                yes,
                "newsletter remove",
                &format!("papr newsletter remove {feed_id}"),
            )?;
            db::delete_newsletter_source(&conn, feed_id).map_err(db_err)?;
            db::delete_feed(&conn, feed_id).map_err(db_err)?;
            ok_line(format!("newsletter: #{feed_id} removed"))
        }
    }
}

// ──────────────────────── opml / settings / admin ────────────────────────

fn cmd_opml(path: &Path, cmd: OpmlCmd) -> Result<String, AxiError> {
    match cmd {
        OpmlCmd::Import { file } => {
            let conn = open_rw(path)?;
            let content = std::fs::read_to_string(&file).map_err(|e| {
                AxiError::runtime(format!("could not read {}: {e}", file.display()))
            })?;
            let imported = papr_core::opml::parse(&content)
                .map_err(|e| AxiError::runtime(format!("invalid OPML: {e}")))?;
            let mut added = 0usize;
            let mut skipped = 0usize;
            for f in &imported {
                if db::find_feed_by_url(&conn, &f.feed_url)
                    .map_err(db_err)?
                    .is_some()
                {
                    skipped += 1;
                    continue;
                }
                let folder_id = match &f.folder {
                    Some(name) => Some(db::create_folder(&conn, name).map_err(db_err)?),
                    None => None,
                };
                db::insert_feed(
                    &conn,
                    &f.feed_url,
                    None,
                    &f.title,
                    None,
                    papr_core::models::SourceType::Rss,
                    folder_id,
                )
                .map_err(db_err)?;
                added += 1;
            }
            let mut d = Doc::new();
            d.set("import", json!({ "added": added, "skipped": skipped }));
            if added > 0 {
                d.help(vec!["Run `papr refresh` to fetch the imported feeds".into()]);
            }
            Ok(d.into_toon())
        }
        OpmlCmd::Export { out } => {
            let conn = open_ro(path)?;
            let feeds = db::feeds_for_export(&conn).map_err(db_err)?;
            let xml = papr_core::opml::build(&feeds)
                .map_err(|e| AxiError::runtime(format!("OPML build failed: {e}")))?;
            match out {
                Some(file) => {
                    std::fs::write(&file, &xml).map_err(|e| {
                        AxiError::runtime(format!("could not write {}: {e}", file.display()))
                    })?;
                    ok_line(format!(
                        "exported {} feeds to {}",
                        feeds.len(),
                        file.display()
                    ))
                }
                None => Ok(xml),
            }
        }
    }
}

fn cmd_settings(path: &Path, cmd: SettingsCmd) -> Result<String, AxiError> {
    match cmd {
        SettingsCmd::Get { key } => {
            let conn = open_ro(path)?;
            let value = db::get_setting(&conn, &key).map_err(db_err)?;
            let mut d = Doc::new();
            match value {
                // Mask credential values so a `get` never spills a key into the
                // agent's transcript or the terminal scrollback.
                Some(v) if is_secret_key(&key) => d.set(&key, mask_secret(&v)),
                Some(v) => d.set(&key, v),
                None => d.set(&key, Value::Null),
            };
            Ok(d.into_toon())
        }
        SettingsCmd::Set { key, value } => {
            let conn = open_rw(path)?;
            db::set_setting(&conn, &key, &value).map_err(db_err)?;
            // Don't echo a secret back; confirm the write without disclosing it.
            if is_secret_key(&key) {
                ok_line(format!("{key}: {}", mask_secret(&value)))
            } else {
                ok_line(format!("{key}: {value}"))
            }
        }
    }
}

fn cmd_stats(path: &Path) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let (bytes, articles, feeds) = db::storage_stats(&conn).map_err(db_err)?;
    let (unread, starred, later) = db::smart_counts(&conn).map_err(db_err)?;
    Ok(Doc::new()
        .set(
            "stats",
            json!({
                "db_size": human_bytes(bytes),
                "articles": articles,
                "feeds": feeds,
                "unread": unread,
                "starred": starred,
                "read_later": later,
            }),
        )
        .into_toon())
}

fn cmd_admin(path: &Path, cmd: AdminCmd) -> Result<String, AxiError> {
    let conn = open_rw(path)?;
    match cmd {
        AdminCmd::Cleanup { days, yes } => {
            if days < 0 {
                return Err(AxiError::usage(
                    format!("cleanup window must be >= 0 days, got {days}"),
                    vec!["Run `papr admin cleanup <days>` with a non-negative day count".into()],
                ));
            }
            require_yes(yes, "cleanup", &format!("papr admin cleanup {days}"))?;
            let n = db::cleanup_old_articles(&conn, days).map_err(db_err)?;
            ok_line(format!(
                "cleanup: removed {n} article(s) older than {days} days"
            ))
        }
        AdminCmd::Vacuum { yes } => {
            require_yes(yes, "vacuum", "papr admin vacuum")?;
            conn.execute_batch("VACUUM")
                .map_err(|e| AxiError::runtime(format!("vacuum: {e}")))?;
            ok_line("vacuum: database compacted".into())
        }
        AdminCmd::Reset { yes } => {
            require_yes(yes, "settings reset", "papr admin reset")?;
            db::reset_settings(&conn).map_err(db_err)?;
            ok_line("settings: reset to defaults".into())
        }
    }
}

fn human_bytes(n: i64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

// ───────────────────────────── rendering helpers ─────────────────────────────

/// An optional column an agent can request with `--fields` beyond the default
/// `{id,feed,title,flags,date}` schema.
#[derive(Clone, Copy)]
enum ExtraField {
    Author,
    Url,
    Snippet,
    Type,
    FeedId,
    Published,
}

/// Parse a `--fields a,b,c` value into extra columns, rejecting unknown names
/// with the valid set so the agent can correct in one step.
fn parse_fields(spec: &str) -> Result<Vec<ExtraField>, AxiError> {
    spec.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| match s {
            "author" => Ok(ExtraField::Author),
            "url" => Ok(ExtraField::Url),
            "snippet" => Ok(ExtraField::Snippet),
            "type" => Ok(ExtraField::Type),
            "feed_id" => Ok(ExtraField::FeedId),
            "published" => Ok(ExtraField::Published),
            other => Err(AxiError::usage(
                format!("unknown field `{other}`"),
                vec!["Valid --fields: author, url, snippet, type, feed_id, published".into()],
            )),
        })
        .collect()
}

/// `{id,feed,title,flags,date}` — the minimal schema an agent needs to pick a
/// next action without a follow-up call — plus any `extra` columns requested
/// via `--fields`. Returns a JSON array the encoder lays out as a tabular TOON
/// array (uniform keys across rows keep it tabular).
fn article_rows(rows: &[papr_core::models::ArticleSummary], extra: &[ExtraField]) -> Value {
    Value::Array(
        rows.iter()
            .map(|a| {
                let mut o = serde_json::Map::new();
                o.insert("id".into(), json!(a.id));
                o.insert("feed".into(), json!(cap(&a.feed_title, 28)));
                o.insert("title".into(), json!(cap(&a.title, 80)));
                o.insert("flags".into(), json!(summary_flags(a)));
                o.insert("date".into(), json!(short_date(a.published_at.as_deref())));
                for f in extra {
                    let (k, v) = match f {
                        ExtraField::Author => ("author", json!(a.author)),
                        ExtraField::Url => ("url", json!(a.url)),
                        ExtraField::Snippet => {
                            ("snippet", json!(a.snippet.as_deref().map(|s| cap(s, 120))))
                        }
                        ExtraField::Type => ("type", json!(a.source_type)),
                        ExtraField::FeedId => ("feed_id", json!(a.feed_id)),
                        ExtraField::Published => ("published", json!(a.published_at)),
                    };
                    o.insert(k.into(), v);
                }
                Value::Object(o)
            })
            .collect(),
    )
}

fn feed_rows(feeds: &[&papr_core::models::Feed]) -> Value {
    Value::Array(
        feeds
            .iter()
            .map(|f| {
                json!({
                    "id": f.id,
                    "title": cap(&f.title, 40),
                    "unread": f.unread_count,
                    "type": f.source_type,
                })
            })
            .collect(),
    )
}

fn summary_flags(a: &papr_core::models::ArticleSummary) -> String {
    let mut v = vec![if a.is_read { "read" } else { "unread" }];
    if a.is_starred {
        v.push("star");
    }
    if a.read_later {
        v.push("later");
    }
    v.join(".")
}

fn detail_flags(a: &papr_core::models::ArticleDetail) -> String {
    let mut v = vec![if a.is_read { "read" } else { "unread" }];
    if a.is_starred {
        v.push("star");
    }
    if a.read_later {
        v.push("later");
    }
    v.join(".")
}

/// Map list/read filter flags to a `(query, unread_only)` pair.
fn resolve_query(args: &ListArgs) -> (ArticleQuery, bool) {
    let query = if let Some(f) = args.feed {
        ArticleQuery::Feed(f)
    } else if let Some(f) = args.folder {
        ArticleQuery::Folder(f)
    } else if let Some(t) = args.tag {
        ArticleQuery::Tag(t)
    } else if args.starred {
        ArticleQuery::Starred
    } else if args.later {
        ArticleQuery::ReadLater
    } else {
        ArticleQuery::All
    };
    // Starred / read-later views show regardless of read state; everything else
    // defaults to unread unless --all is given.
    let unread_only = !args.all && !args.starred && !args.later;
    (query, unread_only)
}

fn scope_label(query: &ArticleQuery, unread_only: bool) -> String {
    let base = match query {
        ArticleQuery::Starred => "starred",
        ArticleQuery::ReadLater => "read-later",
        ArticleQuery::Feed(_) => "in feed",
        ArticleQuery::Folder(_) => "in folder",
        ArticleQuery::Tag(_) => "tagged",
        _ => "articles",
    };
    if unread_only {
        format!("unread {base}").replace("unread articles", "unread")
    } else {
        base.to_string()
    }
}

/// A count(*) mirroring a list query + the unread filter, so the list header can
/// state a definitive total instead of forcing the agent to paginate to find out.
fn count_articles(
    conn: &Connection,
    query: &ArticleQuery,
    unread_only: bool,
) -> papr_core::error::AppResult<i64> {
    let unread = if unread_only { " AND is_read = 0" } else { "" };
    let (sql, param): (String, Option<i64>) = match query {
        ArticleQuery::All | ArticleQuery::Unread => (
            format!("SELECT count(*) FROM articles WHERE 1=1{unread}"),
            None,
        ),
        ArticleQuery::Starred => (
            format!("SELECT count(*) FROM articles WHERE is_starred = 1{unread}"),
            None,
        ),
        ArticleQuery::ReadLater => (
            format!("SELECT count(*) FROM articles WHERE read_later = 1{unread}"),
            None,
        ),
        ArticleQuery::Feed(id) => (
            format!("SELECT count(*) FROM articles WHERE feed_id = ?1{unread}"),
            Some(*id),
        ),
        ArticleQuery::Folder(id) => (
            format!(
                "SELECT count(*) FROM articles WHERE feed_id IN \
                 (SELECT id FROM feeds WHERE folder_id = ?1){unread}"
            ),
            Some(*id),
        ),
        ArticleQuery::Tag(id) => (
            format!(
                "SELECT count(*) FROM articles WHERE id IN \
                 (SELECT article_id FROM article_tags WHERE tag_id = ?1){unread}"
            ),
            Some(*id),
        ),
    };
    let count = match param {
        Some(p) => conn.query_row(&sql, [p], |r| r.get::<_, i64>(0))?,
        None => conn.query_row(&sql, [], |r| r.get::<_, i64>(0))?,
    };
    Ok(count)
}

/// Reproduce the active filter flags so a pagination hint carries them forward.
fn replay_filters(args: &ListArgs) -> String {
    let mut parts = Vec::new();
    if let Some(f) = args.feed {
        parts.push(format!("--feed {f}"));
    }
    if let Some(f) = args.folder {
        parts.push(format!("--folder {f}"));
    }
    if let Some(t) = args.tag {
        parts.push(format!("--tag {t}"));
    }
    if args.starred {
        parts.push("--starred".into());
    }
    if args.later {
        parts.push("--later".into());
    }
    if args.all {
        parts.push("--all".into());
    }
    parts.join(" ")
}

/// Truncate on a char boundary, returning `(shown, total_chars, was_truncated)`.
fn truncate(text: &str, budget: usize) -> (String, usize, bool) {
    let total = text.chars().count();
    if total <= budget {
        return (text.to_string(), total, false);
    }
    let shown: String = text.chars().take(budget).collect();
    (shown, total, true)
}

/// Cap a single-line cell, appending an ellipsis when shortened.
fn cap(s: &str, n: usize) -> String {
    let one_line = s.replace(['\n', '\r'], " ");
    if one_line.chars().count() <= n {
        return one_line;
    }
    let mut t: String = one_line.chars().take(n.saturating_sub(1)).collect();
    t.push('…');
    t
}

fn short_date(published: Option<&str>) -> String {
    match published {
        Some(s) if s.len() >= 10 => s[..10].to_string(),
        Some(s) => s.to_string(),
        None => "-".to_string(),
    }
}

// ───────────────────────────── infrastructure ─────────────────────────────

fn http_client() -> Result<reqwest::Client, AxiError> {
    use std::time::Duration;
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        // Bound network waits so a stalled connection or unresponsive server
        // can't hang the CLI forever. The total budget stays generous for a slow
        // feed or article fetch while still failing eventually.
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| AxiError::runtime(format!("http client: {e}")))
}

fn open_ro(path: &Path) -> Result<Connection, AxiError> {
    ensure_db(path)?;
    db::open_reader(path).map_err(db_err)
}

fn open_rw(path: &Path) -> Result<Connection, AxiError> {
    ensure_db(path)?;
    db::open(path).map_err(db_err)
}

fn ensure_db(path: &Path) -> Result<(), AxiError> {
    if path.exists() {
        return Ok(());
    }
    Err(AxiError::runtime_help(
        format!(
            "no Papr database at {}",
            collapse_home(&path.display().to_string())
        ),
        vec![
            "Install and launch Papr to create it, or".into(),
            "Run any papr command with `--db <path>` / set PAPR_DB to point at one".into(),
        ],
    ))
}

fn db_path(cli: &Cli) -> Result<PathBuf, AxiError> {
    if let Some(p) = &cli.db {
        return Ok(p.clone());
    }
    Ok(app_data_dir()?.join(APP_IDENTIFIER).join("papr.db"))
}

/// The platform application-data directory Tauri stores the DB under.
fn app_data_dir() -> Result<PathBuf, AxiError> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").map_err(|_| home_err())?;
        Ok(PathBuf::from(home).join("Library/Application Support"))
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA").map_err(|_| home_err())?;
        Ok(PathBuf::from(appdata))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Ok(x) = std::env::var("XDG_DATA_HOME") {
            if !x.is_empty() {
                return Ok(PathBuf::from(x));
            }
        }
        let home = std::env::var("HOME").map_err(|_| home_err())?;
        Ok(PathBuf::from(home).join(".local/share"))
    }
}

fn home_err() -> AxiError {
    AxiError::runtime_help(
        "could not resolve the home directory",
        vec!["Set PAPR_DB or pass --db <path> to the database".into()],
    )
}

fn current_exe_path() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "papr".into())
}

/// Collapse the user's home prefix to `~` for compact, portable display.
fn collapse_home(p: &str) -> String {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            if let Some(rest) = p.strip_prefix(&home) {
                return format!("~{rest}");
            }
        }
    }
    p.to_string()
}

// ───────────────────────────── errors ─────────────────────────────

/// A structured, agent-readable error. Rendered to **stdout** in TOON (same
/// channel as success) so the agent can read and act on it; exit code conveys
/// the class (1 = runtime, 2 = usage).
struct AxiError {
    message: String,
    help: Vec<String>,
    usage: bool,
}

impl AxiError {
    fn runtime(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            help: Vec::new(),
            usage: false,
        }
    }
    fn runtime_help(message: impl Into<String>, help: Vec<String>) -> Self {
        Self {
            message: message.into(),
            help,
            usage: false,
        }
    }
    fn usage(message: impl Into<String>, help: Vec<String>) -> Self {
        Self {
            message: message.into(),
            help,
            usage: true,
        }
    }
    fn exit_code(&self) -> ExitCode {
        if self.usage {
            ExitCode::from(2)
        } else {
            ExitCode::FAILURE
        }
    }
}

/// Translate a database/core error into a runtime AxiError, discarding noisy
/// internals — the agent gets actionable meaning, not a stack trace.
fn db_err(e: papr_core::error::AppError) -> AxiError {
    AxiError::runtime(format!("{e}"))
}

/// Translate a network-facing core error into a clean, actionable message,
/// `context` describing the operation. Raw `reqwest` failures (which embed the
/// internal endpoint URL and a verbose chain) collapse to a short phrase, so no
/// dependency name or internal path leaks into the agent's output.
fn clean_err(context: &str, e: papr_core::error::AppError) -> AxiError {
    use papr_core::error::AppError;
    let detail = match &e {
        AppError::Http(h) => {
            if h.is_timeout() {
                "the request timed out".to_string()
            } else if h.is_connect() {
                "could not reach the server".to_string()
            } else if let Some(s) = h.status() {
                format!("the server returned HTTP {}", s.as_u16())
            } else {
                "the network request failed".to_string()
            }
        }
        // Our own typed errors (coded, parse, db) are already clean.
        other => other.to_string(),
    };
    AxiError::runtime(format!("{context}: {detail}"))
}

fn render_error(e: &AxiError) -> String {
    let mut d = Doc::new();
    d.set("error", e.message.clone());
    d.help(e.help.clone());
    d.into_toon()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// CI guard against the installable skill drifting from the actual CLI
    /// surface: every command must be documented as a `papr <name>` example, and
    /// every `papr <name>` the doc references must be a real command (so a
    /// removed command left behind in the skill fails the build).
    #[test]
    fn skill_doc_matches_cli_commands() {
        let names: std::collections::HashSet<String> = Cli::command()
            .get_subcommands()
            .map(|c| c.get_name().to_string())
            .collect();
        let skill = include_str!("../../../skills/papr-rss/SKILL.md");

        // Forward: every subcommand appears as a `papr <name>` invocation.
        for n in &names {
            assert!(
                skill.contains(&format!("papr {n}")),
                "SKILL.md has no `papr {n}` example — command is undocumented"
            );
        }
        // Reverse: every `papr <token>` the doc shows is a real command.
        for span in skill.split("papr ").skip(1) {
            let token: String = span
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                .collect();
            // Skip bare `papr` (the home view) and any `papr --flag` usage.
            if token.is_empty() || token.starts_with('-') {
                continue;
            }
            assert!(
                names.contains(&token),
                "SKILL.md references `papr {token}`, which is not a CLI command"
            );
        }
    }
}
