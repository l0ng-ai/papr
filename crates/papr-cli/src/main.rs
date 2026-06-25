//! papr — an Agent eXperience Interface (AXI) over a local Papr RSS database.
//!
//! Reads, searches, triages and refreshes your feeds from the shell, emitting
//! token-efficient TOON on stdout. Designed to be driven by autonomous agents:
//! minimal default schemas, truncated long text with an escape hatch,
//! pre-computed aggregates, definitive empty states, idempotent mutations, and
//! structured errors (also on stdout). Diagnostics go to stderr.

mod setup;
mod toon;

use clap::{Parser, Subcommand};
use papr_core::ai::{self, AiConfig};
use papr_core::db;
use papr_core::ingestion::{fetch, parse, refresh};
use papr_core::models::ArticleQuery;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use toon::{opt, scalar, Out};

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
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            print!("{}", render_error(&AxiError::runtime(format!("runtime: {e}"))));
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
    /// Manage a feed's settings (rename / move / interval / translate).
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
    /// Summarize an article with the configured AI provider.
    Summarize {
        /// The article id.
        id: i64,
        /// Also cache the summary on the article (so the app shows it too).
        #[arg(long)]
        save: bool,
    },
    /// Ask a question answered from your subscribed articles (RAG over FTS5).
    Ask {
        /// The question.
        question: String,
        /// How many articles to retrieve as context.
        #[arg(long, default_value_t = 6)]
        limit: i64,
    },
    /// Generate an AI briefing of your most recent articles.
    Digest {
        /// How many recent articles to brief over.
        #[arg(long, default_value_t = 30)]
        limit: i64,
    },
    /// Translate an article's text into a target language.
    Translate {
        /// The article id.
        id: i64,
        /// Target language (e.g. "English", "Simplified Chinese", "日本語").
        #[arg(long)]
        lang: String,
    },
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
    /// Toggle a feed's auto-translate-on-open.
    Translate {
        id: i64,
        #[arg(value_parser = ["on", "off"])]
        state: String,
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
        Some(Cmd::Refresh { feed }) => cmd_refresh(&path, feed).await,
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
        // Setup writes agent config files, not the DB — it never opens `path`.
        Some(Cmd::Setup { app }) => setup::run(&app),
        Some(Cmd::Summarize { id, save }) => cmd_summarize(&path, id, save).await,
        Some(Cmd::Ask { question, limit }) => cmd_ask(&path, &question, limit).await,
        Some(Cmd::Digest { limit }) => cmd_digest(&path, limit).await,
        Some(Cmd::Translate { id, lang }) => cmd_translate(&path, id, &lang).await,
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

fn ok_line(text: String) -> Result<String, AxiError> {
    let mut out = Out::new();
    out.line(0, &text);
    Ok(out.into_string())
}

// ───────────────────────────── commands ─────────────────────────────

/// No-args home view: identify the tool, then show live unread state so an
/// agent can act immediately (AXI "content first").
fn cmd_home(path: &Path) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let (unread, starred, later) = db::smart_counts(&conn).map_err(db_err)?;
    let feeds = db::list_feeds(&conn).map_err(db_err)?;

    // Most recent unread first; fall back to recent reads when the inbox is clear.
    let recent = db::list_articles(&conn, &ArticleQuery::All, true, None, false, 10, 0)
        .map_err(db_err)?;
    let inbox_clear = recent.is_empty();
    let recent = if inbox_clear {
        db::list_articles(&conn, &ArticleQuery::All, false, None, false, 10, 0).map_err(db_err)?
    } else {
        recent
    };

    let mut out = Out::new();
    out.kv(0, "bin", &collapse_home(&current_exe_path()));
    out.kv(0, "description", DESCRIPTION);
    out.kv(0, "db", &collapse_home(&path.display().to_string()));
    out.line(
        0,
        &format!("unread: {unread} · starred: {starred} · later: {later}"),
    );
    out.kv(0, "feeds", &feeds.len().to_string());

    if inbox_clear {
        out.line(0, "inbox: 0 unread — all caught up");
        out.header(0, &format!("recent[{}]", recent.len()));
    }
    article_table(
        &mut out,
        if inbox_clear { "recent" } else { "unread" },
        &recent,
    );

    out.help(&[
        "Run `papr read <id>` to read an article's full text".into(),
        "Run `papr list --feed <id>` to list one feed's articles".into(),
        "Run `papr search \"<query>\"` to search every article".into(),
        "Run `papr refresh` to fetch new articles".into(),
    ]);
    Ok(out.into_string())
}

fn cmd_feeds(path: &Path) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let feeds = db::list_feeds(&conn).map_err(db_err)?;
    let folders = db::list_folders(&conn).map_err(db_err)?;

    if feeds.is_empty() {
        let mut out = Out::new();
        out.line(0, "feeds: 0 subscriptions yet");
        out.help(&["Run `papr subscribe <url>` to add your first feed".into()]);
        return Ok(out.into_string());
    }

    let total_unread: i64 = feeds.iter().map(|f| f.unread_count).sum();
    let mut out = Out::new();
    out.line(
        0,
        &format!("count: {} feeds · {total_unread} unread", feeds.len()),
    );

    // Group by folder so an agent sees the organisation; folderless feeds last.
    let folder_name = |id: Option<i64>| -> Option<String> {
        id.and_then(|fid| folders.iter().find(|f| f.id == fid).map(|f| f.name.clone()))
    };
    let mut printed_any_group = false;
    for folder in &folders {
        let group: Vec<_> = feeds.iter().filter(|f| f.folder_id == Some(folder.id)).collect();
        if group.is_empty() {
            continue;
        }
        printed_any_group = true;
        out.header(0, &scalar(&folder.name));
        feed_table(&mut out, 1, &group);
    }
    let loose: Vec<_> = feeds.iter().filter(|f| folder_name(f.folder_id).is_none()).collect();
    if !loose.is_empty() {
        if printed_any_group {
            out.header(0, "(no folder)");
            feed_table(&mut out, 1, &loose);
        } else {
            feed_table(&mut out, 0, &loose);
        }
    }

    out.help(&[
        "Run `papr list --feed <id>` to list a feed's articles".into(),
        "Run `papr refresh --feed <id>` to fetch one feed".into(),
        "Run `papr subscribe <url>` to add a feed".into(),
    ]);
    Ok(out.into_string())
}

fn cmd_list(path: &Path, args: ListArgs) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let (query, unread_only) = resolve_query(&args);
    let rows = db::list_articles(&conn, &query, unread_only, None, false, args.limit, args.offset)
        .map_err(db_err)?;
    let total = count_articles(&conn, &query, unread_only).map_err(db_err)?;

    if rows.is_empty() {
        let mut out = Out::new();
        out.line(0, &format!("articles: 0 {} found", scope_label(&query, unread_only)));
        out.help(&["Run `papr refresh` to fetch new articles".into()]);
        return Ok(out.into_string());
    }

    let mut out = Out::new();
    let shown = rows.len();
    out.line(
        0,
        &format!(
            "count: {shown} of {total} {}",
            scope_label(&query, unread_only)
        ),
    );
    article_table(&mut out, "articles", &rows);

    let mut help = vec![
        "Run `papr read <id>` to read an article's full text".into(),
        "Run `papr mark read <id>` to mark an article read".into(),
    ];
    if args.offset + (shown as i64) < total {
        help.push(format!(
            "Run `papr list --offset {} {}` for the next page",
            args.offset + args.limit,
            replay_filters(&args)
        ));
    }
    out.help(&help);
    Ok(out.into_string())
}

fn cmd_read(path: &Path, args: ReadArgs) -> Result<String, AxiError> {
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
        db::list_articles(&conn, &query, args.unread, None, false, args.limit, 0)
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
        let mut out = Out::new();
        out.line(0, "articles: 0 matched that filter");
        out.help(&["Run `papr list` to find article ids".into()]);
        return Ok(out.into_string());
    }

    let batch = ids.len() > 1;
    let budget = if batch { BATCH_TRUNCATE } else { READ_TRUNCATE };
    let mut out = Out::new();
    if batch {
        out.line(0, &format!("count: {} articles", ids.len()));
    }
    let mut any_truncated = false;
    for id in &ids {
        let detail = match db::get_article(&conn, *id) {
            Ok(d) => d,
            Err(_) => {
                out.header(0, "article");
                out.kv(1, "id", &id.to_string());
                out.kv(1, "error", "not found");
                continue;
            }
        };
        let (_title, text) = db::article_text(&conn, *id).map_err(db_err)?;
        out.header(0, "article");
        out.kv(1, "id", &detail.id.to_string());
        out.kv(1, "feed", &detail.feed_title);
        out.kv(1, "title", &detail.title);
        out.kv(1, "author", &opt(detail.author.as_deref()));
        out.kv(1, "url", &opt(detail.url.as_deref()));
        out.kv(1, "published", &opt(detail.published_at.as_deref()));
        out.kv(1, "state", &detail_flags(&detail));
        if !detail.tags.is_empty() {
            let names: Vec<String> = detail.tags.iter().map(|t| t.name.clone()).collect();
            out.kv(1, "tags", &names.join(", "));
        }
        let (shown, total_chars, truncated) = truncate(&text, if args.full { usize::MAX } else { budget });
        any_truncated |= truncated;
        let body = if truncated {
            format!("{shown}\n... (truncated, {total_chars} chars total)")
        } else {
            shown
        };
        out.block(1, "text", &body);
    }

    if any_truncated && !args.full {
        out.help(&["Run `papr read <id> --full` to see the complete text".into()]);
    }
    Ok(out.into_string())
}

fn cmd_search(path: &Path, query: &str, limit: i64) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let hits = db::search_articles_for_rag(&conn, query, limit).map_err(db_err)?;
    if hits.is_empty() {
        let mut out = Out::new();
        out.line(0, &format!("search: 0 matches for {}", scalar(query)));
        out.help(&["Run `papr refresh` to fetch new articles, then search again".into()]);
        return Ok(out.into_string());
    }
    let mut out = Out::new();
    out.line(0, &format!("count: {} matches", hits.len()));
    let rows: Vec<Vec<String>> = hits
        .iter()
        .map(|(id, title, feed)| vec![id.to_string(), scalar(feed), scalar(&cap(title, 80))])
        .collect();
    out.table(0, "matches", &["id", "feed", "title"], &rows);
    out.help(&["Run `papr read <id>` to read a match in full".into()]);
    Ok(out.into_string())
}

fn cmd_mark(path: &Path, state: &str, ids: &[i64]) -> Result<String, AxiError> {
    let conn = open_rw(path)?;
    let mut out = Out::new();
    let mut lines = Vec::new();
    for &id in ids {
        // Read current state for idempotency: an already-applied change is a
        // no-op with exit 0, not an error.
        let current = match db::get_article(&conn, id) {
            Ok(d) => d,
            Err(_) => {
                lines.push(format!("#{id}: not found"));
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
        if already {
            lines.push(format!("#{id}: already {state} (no-op)"));
        } else {
            lines.push(format!("#{id}: {state}"));
        }
    }
    out.header(0, &format!("marked[{}]", lines.len()));
    for l in &lines {
        out.line(1, l);
    }
    Ok(out.into_string())
}

fn cmd_tags(path: &Path) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let tags = db::list_tags(&conn).map_err(db_err)?;
    if tags.is_empty() {
        let mut out = Out::new();
        out.line(0, "tags: 0 defined");
        return Ok(out.into_string());
    }
    let mut out = Out::new();
    out.line(0, &format!("count: {} tags", tags.len()));
    let rows: Vec<Vec<String>> = tags
        .iter()
        .map(|t| vec![t.id.to_string(), scalar(&t.name), t.article_count.to_string()])
        .collect();
    out.table(0, "tags", &["id", "name", "articles"], &rows);
    out.help(&["Run `papr list --tag <id>` to list a tag's articles".into()]);
    Ok(out.into_string())
}

async fn cmd_subscribe(path: &Path, url: &str, folder: Option<i64>) -> Result<String, AxiError> {
    let conn = open_rw(path)?;
    let client = http_client()?;

    // Fetch the target. If it parses as a feed, use it directly; otherwise treat
    // it as an HTML page and auto-discover the feed link.
    let (bytes, _etag, final_url) = fetch::get(&client, url)
        .await
        .map_err(|e| AxiError::runtime(format!("could not fetch {url}: {e}")))?;

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
                .map_err(|e| AxiError::runtime(format!("could not fetch {candidate}: {e}")))?;
            let parsed = parse::parse_feed(&fbytes, &furl)
                .map_err(|e| AxiError::runtime(format!("discovered feed did not parse: {e}")))?;
            (furl, parsed)
        }
    };

    // Idempotent: re-subscribing to a known feed is a no-op, not an error.
    if let Some(existing) = db::find_feed_by_url(&conn, &feed_url).map_err(db_err)? {
        let mut out = Out::new();
        out.line(0, &format!("feed: #{existing} already subscribed (no-op)"));
        out.help(&[format!("Run `papr list --feed {existing}` to read it")]);
        return Ok(out.into_string());
    }

    let source_type =
        parse::refine_source_type(papr_core::models::SourceType::Rss, &parsed, &feed_url);
    let title = parsed
        .title
        .clone()
        .unwrap_or_else(|| feed_url.clone());
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
    let rules = db::active_rules(&conn).unwrap_or_default();
    let dedup = db::setting_flag(&conn, "dedup_enabled", false);
    let mut new_count = 0usize;
    for article in &parsed.articles {
        if let Ok(true) = db::upsert_article(&conn, feed_id, article, dedup, &rules) {
            new_count += 1;
        }
    }
    let _ = db::touch_feed(&conn, feed_id);

    let mut out = Out::new();
    out.header(0, "feed");
    out.kv(1, "id", &feed_id.to_string());
    out.kv(1, "title", &title);
    out.kv(1, "url", &feed_url);
    out.kv(1, "type", source_type.as_str());
    out.kv(1, "articles", &new_count.to_string());
    out.help(&[
        format!("Run `papr list --feed {feed_id}` to read it"),
        format!("Run `papr refresh --feed {feed_id}` to fetch more later"),
    ]);
    Ok(out.into_string())
}

async fn cmd_refresh(path: &Path, feed: Option<i64>) -> Result<String, AxiError> {
    let conn = db::open(path).map_err(db_err)?;
    let dbm = tokio::sync::Mutex::new(conn);
    let client = http_client()?;
    let scope = match feed {
        Some(id) => refresh::RefreshScope::One(id),
        None => refresh::RefreshScope::All,
    };

    // Progress is diagnostic — it goes to stderr so stdout stays pure data.
    let summary = refresh::refresh_core(&dbm, &client, scope, |event| {
        use papr_core::models::RefreshProgress::*;
        match event {
            Started { total } => eprintln!("refreshing {total} feed(s)…"),
            FeedDone { feed_id, new_articles, error } => {
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

    let mut out = Out::new();
    out.header(0, "refresh");
    out.kv(1, "scope", &feed.map(|f| format!("feed {f}")).unwrap_or_else(|| "all".into()));
    out.kv(1, "new", &summary.new_articles.to_string());
    if summary.new_articles > 0 {
        out.help(&["Run `papr list` to see what's new".into()]);
    }
    Ok(out.into_string())
}

// ─────────────────────────────── AI ───────────────────────────────

/// Load the AI provider config from settings, with an agent-actionable error
/// when no key is configured.
fn load_ai(conn: &Connection) -> Result<AiConfig, AxiError> {
    AiConfig::new(
        db::get_setting(conn, "ai_provider").map_err(db_err)?,
        db::get_setting(conn, "ai_api_key").map_err(db_err)?,
        db::get_setting(conn, "ai_model").map_err(db_err)?,
        db::get_setting(conn, "ai_base_url").map_err(db_err)?,
    )
    .map_err(|_| {
        AxiError::runtime_help(
            "no AI provider configured",
            vec![
                "Run `papr settings set ai_api_key <key>` (and optionally ai_provider / ai_model)".into(),
                "Or configure AI in the Papr desktop app".into(),
            ],
        )
    })
}

/// A response-language instruction matching the user's configured UI language.
fn response_language(conn: &Connection) -> &'static str {
    match db::get_setting(conn, "language").ok().flatten().as_deref() {
        Some("zh") => "\n\nAlways write your response in Simplified Chinese.",
        Some("ja") => "\n\nAlways write your response in Japanese.",
        _ => "\n\nAlways write your response in English.",
    }
}

/// Emit an AI text result as a `text:` block, with a small header.
fn ai_output(header_kv: &[(&str, String)], text: &str) -> String {
    let mut out = Out::new();
    if !header_kv.is_empty() {
        for (k, v) in header_kv {
            out.kv(0, k, v);
        }
    }
    out.block(0, "text", text.trim());
    out.into_string()
}

async fn cmd_summarize(path: &Path, id: i64, save: bool) -> Result<String, AxiError> {
    let conn = if save { open_rw(path)? } else { open_ro(path)? };
    let (title, body) = db::article_text(&conn, id)
        .map_err(|_| AxiError::runtime(format!("article #{id} not found")))?;
    if body.trim().is_empty() {
        return Err(AxiError::runtime(format!("article #{id} has no body to summarize")));
    }
    let cfg = load_ai(&conn)?;
    let lang = response_language(&conn);
    let system = format!(
        "You are a sharp news editor. Summarize the article so a reader can decide \
         whether to read it in full.\n\nFormat in markdown: a bold **TL;DR** one-liner, \
         then 3-5 single-idea bullets. Output only that.{lang}"
    );
    let user = format!("Title: {title}\n\n{}", truncate(&body, 8000).0);
    let client = http_client()?;
    let text = ai::complete_chat(&client, &cfg, &system, &user, ai::MAX_TOKENS)
        .await
        .map_err(|e| AxiError::runtime(format!("AI request failed: {e}")))?;
    if save && !text.trim().is_empty() {
        db::set_ai_summary(&conn, id, text.trim()).map_err(db_err)?;
    }
    Ok(ai_output(&[("id", id.to_string()), ("model", cfg.model().to_string())], &text))
}

async fn cmd_ask(path: &Path, question: &str, limit: i64) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let cfg = load_ai(&conn)?;
    let lang = response_language(&conn);
    let hits = db::search_articles_for_rag(&conn, question, limit).map_err(db_err)?;
    let mut context = String::new();
    let mut cited = Vec::new();
    for (aid, _title, feed_title) in &hits {
        let (title, body) = db::article_text(&conn, *aid).map_err(db_err)?;
        context.push_str(&format!("## {title} — {feed_title}\n{}\n\n", truncate(&body, 1200).0));
        cited.push(aid.to_string());
    }
    let system = format!(
        "You answer the user's question using only the provided articles from their RSS \
         subscriptions. Cite the article titles you draw from. If the articles do not \
         contain the answer, say so plainly.{lang}"
    );
    let user = if context.trim().is_empty() {
        format!("No relevant articles were found.\n\nQuestion: {question}")
    } else {
        format!("Articles from the user's feeds:\n\n{context}---\n\nQuestion: {question}")
    };
    let client = http_client()?;
    let text = ai::complete_chat(&client, &cfg, &system, &user, ai::MAX_TOKENS)
        .await
        .map_err(|e| AxiError::runtime(format!("AI request failed: {e}")))?;
    Ok(ai_output(
        &[("sources", if cited.is_empty() { "none".into() } else { cited.join(",") })],
        &text,
    ))
}

async fn cmd_digest(path: &Path, limit: i64) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let cfg = load_ai(&conn)?;
    let lang = response_language(&conn);
    let articles = db::digest_source(&conn, limit).map_err(db_err)?;
    if articles.is_empty() {
        return ok_line("digest: 0 recent articles to brief on".into());
    }
    let mut corpus = String::new();
    for (title, feed, text) in &articles {
        corpus.push_str(&format!("- [{feed}] {title}: {}\n", truncate(text, 400).0));
    }
    let system = format!(
        "You are the user's personal news briefer. From the recent articles, write a crisp \
         briefing: group related items into 2-4 themed sections with short headers, lead with \
         what matters most, keep it skimmable. Plain prose, no preamble.{lang}"
    );
    let user = format!("Recent articles from my feeds:\n\n{corpus}");
    let client = http_client()?;
    let text = ai::complete_chat(&client, &cfg, &system, &user, ai::MAX_TOKENS)
        .await
        .map_err(|e| AxiError::runtime(format!("AI request failed: {e}")))?;
    Ok(ai_output(&[("articles", articles.len().to_string())], &text))
}

async fn cmd_translate(path: &Path, id: i64, lang: &str) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let (title, body) = db::article_text(&conn, id)
        .map_err(|_| AxiError::runtime(format!("article #{id} not found")))?;
    if body.trim().is_empty() {
        return Err(AxiError::runtime(format!("article #{id} has no text to translate")));
    }
    let cfg = load_ai(&conn)?;
    let system = format!(
        "You are a professional translator. Translate the article into {lang}, preserving \
         meaning, tone and paragraph structure. Output only the translation, no notes."
    );
    let user = format!("Title: {title}\n\n{}", truncate(&body, 8000).0);
    let client = http_client()?;
    let text = ai::complete_chat(&client, &cfg, &system, &user, ai::TRANSLATE_MAX_TOKENS)
        .await
        .map_err(|e| AxiError::runtime(format!("AI request failed: {e}")))?;
    Ok(ai_output(&[("id", id.to_string()), ("lang", lang.to_string())], &text))
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
        return Err(AxiError::runtime(format!("article #{id} has no URL to extract")));
    };
    let client = http_client()?;
    let (bytes, _e, final_url) = fetch::get(&client, &url)
        .await
        .map_err(|e| AxiError::runtime(format!("could not fetch {url}: {e}")))?;
    let html = String::from_utf8_lossy(&bytes);
    let cleaned = papr_core::extraction::extract_article(&html, &final_url)
        .map_err(|e| AxiError::runtime(format!("extraction failed: {e}")))?;
    let image = papr_core::extraction::lead_image(&html, &final_url);
    db::set_extracted_html(&conn, id, &cleaned, image.as_deref()).map_err(db_err)?;
    let chars = papr_core::sanitize::html_to_text(&cleaned).chars().count();
    let mut out = Out::new();
    out.header(0, "extracted");
    out.kv(1, "id", &id.to_string());
    out.kv(1, "chars", &chars.to_string());
    out.help(&[format!("Run `papr read {id} --full` to read the extracted text")]);
    Ok(out.into_string())
}

fn cmd_folders(path: &Path) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let folders = db::list_folders(&conn).map_err(db_err)?;
    let feeds = db::list_feeds(&conn).map_err(db_err)?;
    if folders.is_empty() {
        return ok_line("folders: 0 defined".into());
    }
    let mut out = Out::new();
    out.line(0, &format!("count: {} folders", folders.len()));
    let rows: Vec<Vec<String>> = folders
        .iter()
        .map(|f| {
            let n = feeds.iter().filter(|x| x.folder_id == Some(f.id)).count();
            vec![f.id.to_string(), scalar(&f.name), n.to_string()]
        })
        .collect();
    out.table(0, "folders", &["id", "name", "feeds"], &rows);
    out.help(&["Run `papr list --folder <id>` to list a folder's articles".into()]);
    Ok(out.into_string())
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
        FeedCmd::Translate { id, state } => {
            let on = state == "on";
            db::set_feed_auto_translate(&conn, id, on).map_err(db_err)?;
            ok_line(format!("feed: #{id} auto-translate {state}"))
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
    if rules.is_empty() {
        let mut out = Out::new();
        out.line(0, "rules: 0 defined");
        out.help(&["Run `papr rule create <name> <keywords>` to add one".into()]);
        return Ok(out.into_string());
    }
    let mut out = Out::new();
    out.line(0, &format!("count: {} rules", rules.len()));
    let rows: Vec<Vec<String>> = rules
        .iter()
        .map(|r| {
            vec![
                r.id.to_string(),
                scalar(&r.name),
                if r.enabled { "on".into() } else { "off".into() },
                r.field.clone(),
                scalar(&r.query),
                r.action.clone(),
                r.feed_id.map(|f| f.to_string()).unwrap_or_else(|| "all".into()),
            ]
        })
        .collect();
    out.table(
        0,
        "rules",
        &["id", "name", "enabled", "field", "query", "action", "feed"],
        &rows,
    );
    out.help(&["Run `papr rule disable <id>` to turn a rule off".into()]);
    Ok(out.into_string())
}

fn cmd_rule(path: &Path, cmd: RuleCmd) -> Result<String, AxiError> {
    let conn = open_rw(path)?;
    match cmd {
        RuleCmd::Create { name, query, field, action, feed } => {
            let id = db::create_rule(&conn, &name, feed, &field, &query, &action).map_err(db_err)?;
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
    db::update_rule(conn, id, &r.name, on, r.feed_id, &r.field, &r.query, &r.action)
        .map_err(db_err)?;
    ok_line(format!("rule: #{id} {}", if on { "enabled" } else { "disabled" }))
}

// ───────────────────────────── highlights ─────────────────────────────

fn cmd_highlights(path: &Path, article: Option<i64>) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let items = match article {
        Some(id) => db::list_highlights(&conn, id).map_err(db_err)?,
        None => db::list_all_highlights(&conn).map_err(db_err)?,
    };
    if items.is_empty() {
        return ok_line("highlights: 0 found".into());
    }
    let mut out = Out::new();
    out.line(0, &format!("count: {} highlights", items.len()));
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|h| {
            vec![
                h.id.to_string(),
                h.article_id.to_string(),
                scalar(&cap(&h.quote, 60)),
                h.color.clone(),
                scalar(&cap(&h.note, 40)),
            ]
        })
        .collect();
    out.table(0, "highlights", &["id", "article", "quote", "color", "note"], &rows);
    Ok(out.into_string())
}

fn cmd_highlight(path: &Path, cmd: HighlightCmd) -> Result<String, AxiError> {
    let conn = open_rw(path)?;
    match cmd {
        HighlightCmd::Create { article, quote, note, color } => {
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
            require_yes(yes, "highlight delete", &format!("papr highlight delete {id}"))?;
            db::delete_highlight(&conn, id).map_err(db_err)?;
            ok_line(format!("highlight: #{id} deleted"))
        }
    }
}

// ──────────────────────────── newsletters ────────────────────────────

fn cmd_newsletters(path: &Path) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let rows = db::list_newsletter_sources(&conn).map_err(db_err)?;
    if rows.is_empty() {
        let mut out = Out::new();
        out.line(0, "newsletters: 0 configured");
        out.help(&["Run `papr newsletter add --title .. --host .. --user .. --password ..` to add one".into()]);
        return Ok(out.into_string());
    }
    let mut out = Out::new();
    out.line(0, &format!("count: {} newsletter sources", rows.len()));
    let table: Vec<Vec<String>> = rows
        .iter()
        .map(|n| {
            vec![
                n.feed_id.to_string(),
                scalar(&cap(&n.title, 32)),
                scalar(&format!("{}:{}", n.host, n.port)),
                scalar(&n.username),
                scalar(&n.folder),
            ]
        })
        .collect();
    out.table(0, "newsletters", &["feed", "title", "host", "user", "folder"], &table);
    Ok(out.into_string())
}

fn cmd_newsletter(path: &Path, cmd: NewsletterCmd) -> Result<String, AxiError> {
    let conn = open_rw(path)?;
    match cmd {
        NewsletterCmd::Add { title, host, port, user, password, folder } => {
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
                return ok_line(format!("newsletter: #{existing} already configured (no-op)"));
            }
            let id = db::insert_newsletter_source(&conn, &feed_url, &title, &cfg).map_err(db_err)?;
            let mut out = Out::new();
            out.header(0, "newsletter");
            out.kv(1, "feed", &id.to_string());
            out.kv(1, "title", &title);
            out.kv(1, "host", &format!("{host}:{port}"));
            out.help(&[format!("Run `papr refresh --feed {id}` to poll it now")]);
            Ok(out.into_string())
        }
        NewsletterCmd::Remove { feed_id, yes } => {
            require_yes(yes, "newsletter remove", &format!("papr newsletter remove {feed_id}"))?;
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
                if db::find_feed_by_url(&conn, &f.feed_url).map_err(db_err)?.is_some() {
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
            let mut out = Out::new();
            out.header(0, "import");
            out.kv(1, "added", &added.to_string());
            out.kv(1, "skipped", &skipped.to_string());
            if added > 0 {
                out.help(&["Run `papr refresh` to fetch the imported feeds".into()]);
            }
            Ok(out.into_string())
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
                    ok_line(format!("exported {} feeds to {}", feeds.len(), file.display()))
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
            let mut out = Out::new();
            match value {
                Some(v) => out.kv(0, &key, &v),
                None => out.line(0, &format!("{key}: (unset)")),
            };
            Ok(out.into_string())
        }
        SettingsCmd::Set { key, value } => {
            let conn = open_rw(path)?;
            db::set_setting(&conn, &key, &value).map_err(db_err)?;
            ok_line(format!("{key}: {value}"))
        }
    }
}

fn cmd_stats(path: &Path) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let (bytes, articles, feeds) = db::storage_stats(&conn).map_err(db_err)?;
    let (unread, starred, later) = db::smart_counts(&conn).map_err(db_err)?;
    let mut out = Out::new();
    out.header(0, "stats");
    out.kv(1, "db_size", &human_bytes(bytes));
    out.kv(1, "articles", &articles.to_string());
    out.kv(1, "feeds", &feeds.to_string());
    out.kv(1, "unread", &unread.to_string());
    out.kv(1, "starred", &starred.to_string());
    out.kv(1, "read_later", &later.to_string());
    Ok(out.into_string())
}

fn cmd_admin(path: &Path, cmd: AdminCmd) -> Result<String, AxiError> {
    let conn = open_rw(path)?;
    match cmd {
        AdminCmd::Cleanup { days, yes } => {
            require_yes(yes, "cleanup", &format!("papr admin cleanup {days}"))?;
            let n = db::cleanup_old_articles(&conn, days).map_err(db_err)?;
            ok_line(format!("cleanup: removed {n} article(s) older than {days} days"))
        }
        AdminCmd::Vacuum { yes } => {
            require_yes(yes, "vacuum", "papr admin vacuum")?;
            conn.execute_batch("VACUUM").map_err(|e| AxiError::runtime(format!("vacuum: {e}")))?;
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

/// `{id,feed,title,flags,date}` — the minimal schema an agent needs to pick a
/// next action without a follow-up call.
fn article_table(out: &mut Out, name: &str, rows: &[papr_core::models::ArticleSummary]) {
    let table_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|a| {
            vec![
                a.id.to_string(),
                scalar(&cap(&a.feed_title, 28)),
                scalar(&cap(&a.title, 80)),
                summary_flags(a),
                short_date(a.published_at.as_deref()),
            ]
        })
        .collect();
    out.table(0, name, &["id", "feed", "title", "flags", "date"], &table_rows);
}

fn feed_table(out: &mut Out, depth: usize, feeds: &[&papr_core::models::Feed]) {
    let rows: Vec<Vec<String>> = feeds
        .iter()
        .map(|f| {
            vec![
                f.id.to_string(),
                scalar(&cap(&f.title, 40)),
                f.unread_count.to_string(),
                f.source_type.clone(),
            ]
        })
        .collect();
    out.table(depth, "feeds", &["id", "title", "unread", "type"], &rows);
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
        ArticleQuery::All | ArticleQuery::Unread => {
            (format!("SELECT count(*) FROM articles WHERE 1=1{unread}"), None)
        }
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
        Some(s) => scalar(s),
        None => "-".to_string(),
    }
}

// ───────────────────────────── infrastructure ─────────────────────────────

fn http_client() -> Result<reqwest::Client, AxiError> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
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
        format!("no Papr database at {}", collapse_home(&path.display().to_string())),
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
        Self { message: message.into(), help: Vec::new(), usage: false }
    }
    fn runtime_help(message: impl Into<String>, help: Vec<String>) -> Self {
        Self { message: message.into(), help, usage: false }
    }
    fn usage(message: impl Into<String>, help: Vec<String>) -> Self {
        Self { message: message.into(), help, usage: true }
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

fn render_error(e: &AxiError) -> String {
    let mut out = Out::new();
    out.kv(0, "error", &e.message);
    out.help(&e.help);
    out.into_string()
}
