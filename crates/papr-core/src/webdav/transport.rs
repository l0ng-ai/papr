//! Bounded retries and response draining preserve pooled proxy/TLS connections.
use super::*;
use std::future::Future;

pub(super) struct Failure {
    error: AppError,
    retry: bool,
    after: Option<Duration>,
}
pub(super) type Result<T> = std::result::Result<T, Failure>;
impl From<AppError> for Failure {
    fn from(error: AppError) -> Self {
        let retry = matches!(&error,AppError::Http(e) if e.is_timeout()||e.is_connect()||e.is_body()||e.is_request());
        Self {
            error,
            retry,
            after: None,
        }
    }
}
impl From<reqwest::Error> for Failure {
    fn from(e: reqwest::Error) -> Self {
        network_error(e).into()
    }
}
impl From<url::ParseError> for Failure {
    fn from(e: url::ParseError) -> Self {
        AppError::from(e).into()
    }
}

// Share server-requested cooldowns across retries and concurrent chunks. The
// key is only the public origin, never a credential-bearing DAV path.
static COOLDOWNS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::BTreeMap<String, (std::time::Instant, StatusCode)>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(Default::default()));
async fn wait_for_server(url: &Url) -> AppResult<()> {
    let origin = url.origin().ascii_serialization();
    let pending = {
        let mut gates = COOLDOWNS.lock().unwrap_or_else(|e| e.into_inner());
        gates.retain(|_, (until, _)| *until > std::time::Instant::now());
        gates.get(&origin).copied()
    };
    if let Some((until, status)) = pending {
        let delay = until.saturating_duration_since(std::time::Instant::now());
        if delay > Duration::from_secs(60) {
            return Err(http_error(status));
        }
        tokio::time::sleep(delay).await;
    }
    Ok(())
}
pub(super) async fn retry<T, F, Fut>(url: &Url, mut operation: F) -> AppResult<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    for attempt in 0..4 {
        wait_for_server(url).await?;
        match operation().await {
            Ok(value) => return Ok(value),
            Err(f) if f.retry && attempt < 3 => {
                progress::retry();
                let delay = f
                    .after
                    .unwrap_or_else(|| Duration::from_millis(300 * (1 << attempt)))
                    .max(Duration::from_millis(100))
                    .min(Duration::from_secs(60));
                tokio::time::sleep(delay).await;
            }
            Err(f) => return Err(f.error),
        }
    }
    unreachable!()
}
pub(super) async fn checked(resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let after = resp
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.parse::<u64>().ok().map(Duration::from_secs).or_else(|| {
                chrono::DateTime::parse_from_rfc2822(v).ok().and_then(|t| {
                    (t.with_timezone(&chrono::Utc) - chrono::Utc::now())
                        .to_std()
                        .ok()
                })
            })
        });
    if matches!(status.as_u16(), 429 | 503) {
        if let Some(until) = after.and_then(|d| std::time::Instant::now().checked_add(d)) {
            let key = resp.url().origin().ascii_serialization();
            let mut gates = COOLDOWNS.lock().unwrap_or_else(|e| e.into_inner());
            if gates.get(&key).is_none_or(|(old, _)| *old < until) {
                gates.insert(key, (until, status));
            }
        }
    }
    drain(resp).await;
    Err(Failure {
        error: http_error(status),
        retry: matches!(status.as_u16(), 408 | 425 | 429 | 500 | 502 | 503 | 504)
            && !after.is_some_and(|d| d > Duration::from_secs(60)),
        after,
    })
}
/// Most DAV servers return an empty body for writes, some return a short HTML
/// page. Consume it so reqwest can reuse the same HTTP/TLS/proxy connection.
pub(super) async fn drain(mut response: reqwest::Response) {
    let mut size = 0;
    while let Ok(Some(b)) = response.chunk().await {
        size += b.len();
        if size > 64 * 1024 {
            break;
        }
    }
}
pub(super) fn etag(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.starts_with('"') && v.ends_with('"'))
        .map(str::to_owned)
}

pub(super) fn upload_body(body: bytes::Bytes) -> reqwest::Body {
    reqwest::Body::wrap_stream(futures_util::stream::unfold(
        (body, 0),
        |(body, offset)| async move {
            if offset == body.len() {
                return None;
            }
            let end = (offset + 64 * 1024).min(body.len());
            let part = body.slice(offset..end);
            progress::bytes(part.len(), true);
            Some((Ok::<_, std::io::Error>(part), (body, end)))
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn successful_write_bodies_are_drained_and_reuse_the_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/object", listener.local_addr().unwrap());
        let accepted = Arc::new(AtomicUsize::new(0));
        let count = accepted.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                count.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    loop {
                        let mut bytes = Vec::new();
                        loop {
                            let mut b = [0; 1024];
                            let Ok(n) = socket.read(&mut b).await else {
                                return;
                            };
                            if n == 0 {
                                return;
                            }
                            bytes.extend_from_slice(&b[..n]);
                            if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        if socket.write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 4\r\nConnection: keep-alive\r\n\r\ndone").await.is_err(){return;}
                    }
                });
            }
        });
        let http = Client::builder().no_proxy().build().unwrap();
        for _ in 0..2 {
            let r = http.put(&url).send().await.unwrap();
            drain(r).await;
        }
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
        server.abort();
    }
}

#[cfg(test)]
mod error_tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn auth_is_not_retried_and_long_retry_after_blocks_early_new_runs() {
        for code in [401, 429] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url =
                Url::parse(&format!("http://{}/test", listener.local_addr().unwrap())).unwrap();
            let requests = Arc::new(AtomicUsize::new(0));
            let count = requests.clone();
            let task = tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut b = [0; 4096];
                    let _ = socket.read(&mut b).await;
                    count.fetch_add(1, Ordering::SeqCst);
                    let after = if code == 429 {
                        "Retry-After: 120\r\n"
                    } else {
                        ""
                    };
                    let reply=format!("HTTP/1.1 {code} Test\r\n{after}Content-Length: 0\r\nConnection: close\r\n\r\n");
                    let _ = socket.write_all(reply.as_bytes()).await;
                }
            });
            let http = Client::builder().no_proxy().build().unwrap();
            let result = retry(&url, || async {
                checked(http.get(url.clone()).send().await?).await
            })
            .await;
            assert!(matches!(
                result,
                Err(AppError::Coded("webdavAuthFailed" | "webdavRateLimited"))
            ));
            assert_eq!(requests.load(Ordering::SeqCst), 1);
            if code == 429 {
                assert!(retry(&url, || async {
                    checked(http.get(url.clone()).send().await?).await
                })
                .await
                .is_err());
                assert_eq!(requests.load(Ordering::SeqCst), 1);
            }
            COOLDOWNS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&url.origin().ascii_serialization());
            task.abort();
        }
    }
}
