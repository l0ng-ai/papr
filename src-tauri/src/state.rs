//! Shared application state, registered via `Builder::manage` and injected into
//! commands as `tauri::State<AppState>`.

use reqwest::Client;
use rusqlite::Connection;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::RwLock;
use tokio::sync::Mutex;

pub struct AppState {
    /// The writer SQLite connection, guarded by an async mutex. All mutations
    /// and the background scheduler hold this exclusively; access is short and
    /// synchronous, so the lock is never held across `.await`.
    pub db: Mutex<Connection>,
    /// A small pool of read-only connections for UI queries. Under WAL these
    /// run concurrently with the writer (and each other), so the interface
    /// stays responsive while a background refresh is writing.
    readers: Vec<Mutex<Connection>>,
    /// Round-robin cursor into `readers`, so concurrent reads spread across
    /// the pool instead of contending on one connection.
    next_reader: AtomicUsize,
    /// Shared HTTP client (connection pooling) for all feed fetching. Held
    /// behind an `RwLock` so the network settings (proxy, timeout) can rebuild
    /// it without an app restart. The lock is only ever held to clone the
    /// (cheap, `Arc`-backed) client out — never across an `.await`.
    pub http: RwLock<Client>,
    /// Held for the duration of a `refresh_all` run. The manual refresh
    /// command and the periodic scheduler can otherwise fire concurrently —
    /// `try_lock` lets a second run bow out instead of duplicating the work.
    pub refresh_lock: Mutex<()>,
    /// A `papr://subscribe` URL delivered before the webview registered its
    /// `deep-link-subscribe` listener — typically a cold-start launch where the
    /// link arrives during `setup()`. The frontend drains this once on mount
    /// (`take_pending_deep_link`); a live link, arriving after the listener
    /// exists, is emitted directly and never lands here.
    pending_deep_link: std::sync::Mutex<Option<String>>,
}

impl AppState {
    /// Build the shared state. `readers` must be non-empty.
    pub fn new(db: Connection, readers: Vec<Connection>, http: Client) -> Self {
        assert!(!readers.is_empty(), "read pool must have at least one connection");
        Self {
            db: Mutex::new(db),
            readers: readers.into_iter().map(Mutex::new).collect(),
            next_reader: AtomicUsize::new(0),
            http: RwLock::new(http),
            refresh_lock: Mutex::new(()),
            pending_deep_link: std::sync::Mutex::new(None),
        }
    }

    /// Stash a deep-link URL that arrived before the webview could receive it,
    /// so the frontend can pick it up once mounted. The most recent link wins —
    /// the user only acts on one Add-feed dialog at a time.
    pub fn set_pending_deep_link(&self, url: String) {
        *self
            .pending_deep_link
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(url);
    }

    /// Take the buffered deep-link URL, if any, clearing it so it is delivered
    /// exactly once.
    pub fn take_pending_deep_link(&self) -> Option<String> {
        self.pending_deep_link
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// Acquire a read-only connection from the pool (round-robin). Use this for
    /// every UI query; reserve `db` for writes so reads never block on the
    /// background refresh.
    pub async fn read(&self) -> tokio::sync::MutexGuard<'_, Connection> {
        let i = self.next_reader.fetch_add(1, Ordering::Relaxed) % self.readers.len();
        self.readers[i].lock().await
    }

    /// Clone the current HTTP client out for use.
    ///
    /// The lock is never held across an `.await`, but a panic elsewhere could
    /// still poison it. The guarded `Client` is `Arc`-backed and immutable, so
    /// poisoning carries no torn state — recover the guard rather than
    /// propagating the panic and taking the whole backend down.
    pub fn http(&self) -> Client {
        self.http
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Swap in a freshly built HTTP client (e.g. after a proxy/timeout change).
    pub fn set_http(&self, client: Client) {
        *self.http.write().unwrap_or_else(|e| e.into_inner()) = client;
    }
}
