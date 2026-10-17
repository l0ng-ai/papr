//! Local transport preferences. Never included in library replication.
use super::*;

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Options {
    pub concurrency: usize,
    pub timeout_secs: u64,
    pub proxy: String,
}
impl Options {
    pub fn read(conn: &Connection) -> AppResult<Self> {
        Ok(Self {
            concurrency: db::setting_parsed::<usize>(conn, "webdav_concurrency", 5).clamp(1, 8),
            timeout_secs: db::setting_parsed::<u64>(conn, "webdav_timeout_sec", 300)
                .clamp(60, 1800),
            proxy: db::get_setting(conn, "webdav_proxy")?.unwrap_or_else(|| "inherit".into()),
        })
    }
    fn validate(&self) -> AppResult<()> {
        if !(1..=8).contains(&self.concurrency) || !(60..=1800).contains(&self.timeout_secs) {
            return Err(AppError::code("webdavInvalidNetwork"));
        }
        if !["inherit", "system", "none"].contains(&self.proxy.as_str()) {
            validate_proxy(&self.proxy)?;
        }
        Ok(())
    }
    pub fn save(&self, conn: &Connection) -> AppResult<()> {
        self.validate()?;
        let tx = conn.unchecked_transaction()?;
        db::set_setting(&tx, "webdav_concurrency", &self.concurrency.to_string())?;
        db::set_setting(&tx, "webdav_timeout_sec", &self.timeout_secs.to_string())?;
        db::set_setting(&tx, "webdav_proxy", &self.proxy)?;
        tx.commit()?;
        Ok(())
    }
}
fn validate_proxy(proxy: &str) -> AppResult<()> {
    let u = Url::parse(proxy).map_err(|_| AppError::code("webdavInvalidProxy"))?;
    if !["http", "https", "socks5", "socks5h"].contains(&u.scheme())
        || u.host_str().is_none()
        || u.query().is_some()
        || u.fragment().is_some()
        || (u.path() != "/" && !u.path().is_empty())
    {
        return Err(AppError::code("webdavInvalidProxy"));
    }
    Ok(())
}

/// Build afresh for each run so changing a Japanese proxy node or Wi-Fi proxy
/// does not leave a previous connection pool/route attached to WebDAV.
pub fn client(conn: &Connection) -> AppResult<Client> {
    let options = Options::read(conn)?;
    let proxy = if options.proxy == "inherit" {
        db::get_setting(conn, "net_proxy")?.unwrap_or_else(|| "system".into())
    } else {
        options.proxy
    };
    let mut builder = Client::builder()
        .user_agent(crate::ingestion::fetch::USER_AGENT)
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(options.timeout_secs))
        .tcp_keepalive(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(8);
    match proxy.as_str() {
        "none" => builder = builder.no_proxy(),
        "system" | "" => {
            // CFNetwork also covers iOS Wi-Fi proxies; reqwest's native
            // system-proxy support otherwise covers desktop platforms only.
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            if let Some(proxies) = apple_system_proxies()? {
                builder = builder.no_proxy();
                for p in proxies {
                    builder = builder.proxy(p);
                }
            }
        }
        custom => {
            validate_proxy(custom)?;
            let p =
                reqwest::Proxy::all(custom).map_err(|_| AppError::code("webdavInvalidProxy"))?;
            builder = builder.no_proxy().proxy(p);
        }
    }
    builder.build().map_err(network_error)
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn apple_system_proxies() -> AppResult<Option<Vec<reqwest::Proxy>>> {
    use core_foundation::{
        base::{CFType, TCFType},
        dictionary::{CFDictionary, CFDictionaryRef},
        number::CFNumber,
        string::CFString,
    };
    #[link(name = "CFNetwork", kind = "framework")]
    extern "C" {
        fn CFNetworkCopySystemProxySettings() -> CFDictionaryRef;
    }
    let raw = unsafe { CFNetworkCopySystemProxySettings() };
    if raw.is_null() {
        return Ok(None);
    }
    let d: CFDictionary<CFString, CFType> = unsafe { TCFType::wrap_under_create_rule(raw) };
    let number = |key: &str| {
        d.find(CFString::new(key))
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|v| v.to_i64())
            .unwrap_or(0)
    };
    let string = |key: &str| {
        d.find(CFString::new(key))
            .and_then(|v| v.downcast::<CFString>())
            .map(|v| v.to_string())
    };
    if number("ProxyAutoConfigEnable") == 1 || number("ProxyAutoDiscoveryEnable") == 1 {
        return Err(AppError::code("webdavProxyPac"));
    }
    let address = |prefix: &str| -> Option<String> {
        if number(&format!("{prefix}Enable")) != 1 {
            return None;
        }
        let host = string(&format!("{prefix}Proxy"))?;
        let port = number(&format!("{prefix}Port"));
        if host.is_empty() || !(1..=65535).contains(&port) {
            return None;
        }
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host
        };
        Some(format!(
            "{}://{host}:{port}",
            if prefix == "SOCKS" { "socks5h" } else { "http" }
        ))
    };
    let mut proxies = Vec::new();
    // System exception lists are interpreted by reqwest; include loopback even
    // on systems that omit it, so local library/test services stay local.
    let mut exceptions = vec!["localhost".to_owned(), "127.0.0.1".into(), "::1".into()];
    if let Some(array) = d
        .find(CFString::new("ExceptionsList"))
        .and_then(|v| v.downcast::<core_foundation::array::CFArray>())
    {
        for v in array.iter() {
            let value: CFType = unsafe { TCFType::wrap_under_get_rule(*v) };
            if let Some(s) = value.downcast::<CFString>() {
                exceptions.push(s.to_string());
            }
        }
    }
    let bypass = exceptions.join(",");
    if let Some(a) = address("HTTP") {
        proxies.push(
            reqwest::Proxy::http(&a)
                .map_err(|_| AppError::code("webdavInvalidProxy"))?
                .no_proxy(reqwest::NoProxy::from_string(&bypass)),
        );
    }
    if let Some(a) = address("HTTPS") {
        proxies.push(
            reqwest::Proxy::https(&a)
                .map_err(|_| AppError::code("webdavInvalidProxy"))?
                .no_proxy(reqwest::NoProxy::from_string(&bypass)),
        );
    }
    if let Some(a) = address("SOCKS") {
        proxies.push(
            reqwest::Proxy::all(&a)
                .map_err(|_| AppError::code("webdavInvalidProxy"))?
                .no_proxy(reqwest::NoProxy::from_string(&bypass)),
        );
    }
    // No manual proxy: leave env/desktop detection to reqwest (and VPN/TUN to OS).
    Ok(if proxies.is_empty() {
        None
    } else {
        Some(proxies)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn options_are_bounded_and_bad_proxy_never_silently_falls_back_to_direct() {
        let c = db::open(std::path::Path::new(":memory:")).unwrap();
        let mut o = Options::read(&c).unwrap();
        assert_eq!(o.concurrency, 5);
        for proxy in [
            "http://127.0.0.1:7897",
            "socks5h://127.0.0.1:1080",
            "none",
            "system",
            "inherit",
        ] {
            o.proxy = proxy.into();
            o.save(&c).unwrap();
            assert!(client(&c).is_ok());
        }
        for proxy in [
            "file:///tmp/proxy",
            "socks4://localhost:1080",
            "invalid",
            "https://proxy.test/secret/path",
        ] {
            o.proxy = proxy.into();
            assert!(o.save(&c).is_err());
        }
        o.proxy = "none".into();
        o.concurrency = 100;
        assert!(o.save(&c).is_err());
    }
}

#[cfg(test)]
mod proxy_tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn explicit_http_and_socks5h_routes_never_resolve_the_origin_locally() {
        for socks in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let count = calls.clone();
            let server = tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let count = count.clone();
                    tokio::spawn(async move {
                        if socks {
                            let mut greeting = [0; 2];
                            socket.read_exact(&mut greeting).await.unwrap();
                            assert_eq!(greeting[0], 5);
                            let mut methods = vec![0; greeting[1] as usize];
                            socket.read_exact(&mut methods).await.unwrap();
                            socket.write_all(&[5, 0]).await.unwrap();
                            let mut command = [0; 4];
                            socket.read_exact(&mut command).await.unwrap();
                            assert_eq!(
                                command,
                                [5, 1, 0, 3],
                                "SOCKS5h must send a domain, not a locally resolved IP"
                            );
                            let len = socket.read_u8().await.unwrap();
                            let mut domain = vec![0; len as usize];
                            socket.read_exact(&mut domain).await.unwrap();
                            assert_eq!(domain, b"unresolvable.invalid");
                            let _port = socket.read_u16().await.unwrap();
                            socket
                                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 80])
                                .await
                                .unwrap();
                        }
                        let mut bytes = Vec::new();
                        let end = loop {
                            let mut b = [0; 2048];
                            let n = socket.read(&mut b).await.unwrap();
                            if n == 0 {
                                return;
                            }
                            bytes.extend_from_slice(&b[..n]);
                            if let Some(p) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                                break p + 4;
                            }
                        };
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_string();
                        let method = headers.split_whitespace().next().unwrap();
                        let target = headers.split_whitespace().nth(1).unwrap();
                        if !socks {
                            assert!(target.starts_with("http://unresolvable.invalid/"));
                        }
                        let length = headers
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        while bytes.len() < end + length {
                            let mut b = [0; 100];
                            let n = socket.read(&mut b).await.unwrap();
                            if n == 0 {
                                return;
                            }
                            bytes.extend_from_slice(&b[..n]);
                        }
                        count.fetch_add(1, Ordering::SeqCst);
                        let code = match method {
                            "MKCOL" => 405,
                            "GET" => 404,
                            "PUT" => 201,
                            "DELETE" => 204,
                            _ => 500,
                        };
                        let response=format!("HTTP/1.1 {code} Proxy\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        socket.write_all(response.as_bytes()).await.unwrap();
                    });
                }
            });
            let db = Mutex::new(db::open(std::path::Path::new(":memory:")).unwrap());
            let options = Options {
                concurrency: 5,
                timeout_secs: 300,
                proxy: format!("{}://{addr}", if socks { "socks5h" } else { "http" }),
            };
            options.save(&*db.lock().await).unwrap();
            let http = client(&*db.lock().await).unwrap();
            tokio::time::timeout(
                Duration::from_secs(90),
                super::super::connect(
                    &db,
                    &http,
                    "http://unresolvable.invalid/papr/",
                    "test",
                    "not-a-real-password",
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 4);
            let exported = crate::db::webdav::records(&*db.lock().await).unwrap();
            assert!(!serde_json::to_string(&exported)
                .unwrap()
                .contains(&addr.to_string()));
            server.abort();
        }
    }
}
