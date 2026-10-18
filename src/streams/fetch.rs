// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

//! Bounded retrieval of user-selected playlists. Never expose a URL, path, header
//! or dependency error in Debug, Display, tracing or a UI-facing failure.
use std::{fmt, time::Duration};

use adw::{gio, glib, prelude::*};
use reqwest::{header::LOCATION, redirect::Policy, Client, Url};
use tokio::sync::watch;

use super::parser::MAX_PLAYLIST_BYTES;

#[derive(Clone)]
pub(crate) struct Cancellation {
    sender: watch::Sender<bool>,
}

impl Cancellation {
    pub(crate) fn new() -> Self {
        let (sender, _) = watch::channel(false);
        Self { sender }
    }

    pub(crate) fn cancel(&self) {
        self.sender.send_replace(true);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        *self.sender.borrow()
    }

    async fn cancelled(&self) {
        let mut receiver = self.sender.subscribe();
        while !*receiver.borrow_and_update() {
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FetchError {
    InvalidOrigin,
    Redirect,
    HttpStatus,
    Network,
    Timeout,
    BodyTooLarge,
    LocalAccess,
    Cancelled,
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidOrigin => "Invalid playlist origin",
            Self::Redirect => "Playlist redirect was rejected",
            Self::HttpStatus => "Playlist server returned an error status",
            Self::Network => "Playlist connection failed",
            Self::Timeout => "Playlist retrieval timed out",
            Self::BodyTooLarge => "Playlist exceeds the 32 MiB limit",
            Self::LocalAccess => "Local playlist cannot be accessed",
            Self::Cancelled => "Playlist retrieval was cancelled",
        })
    }
}

pub(crate) struct Download {
    pub bytes: Vec<u8>,
    /// Validated HTTP(S) URL *after* manual redirects; may carry private tokens.
    pub final_url: String,
}

pub(crate) fn http_client() -> Result<Client, FetchError> {
    Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .redirect(Policy::none())
        .build()
        .map_err(|_| FetchError::Network)
}

fn valid_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https") && url.host().is_some()
}

/// A 30-second budget spans ALL redirects and streamed chunks; the size cap
/// applies to bytes returned by reqwest's decoder, not untrusted Content-Length.
pub(crate) async fn fetch_remote(
    client: &Client,
    origin: &str,
    cancellation: &Cancellation,
) -> Result<Download, FetchError> {
    fetch_remote_with_timeout(client, origin, cancellation, Duration::from_secs(30)).await
}

/// Testable deadline, applied to the complete request including redirects.
async fn fetch_remote_with_timeout(
    client: &Client,
    origin: &str,
    cancellation: &Cancellation,
    timeout: Duration,
) -> Result<Download, FetchError> {
    let mut url = Url::parse(origin).map_err(|_| FetchError::InvalidOrigin)?;
    if !valid_url(&url) || origin.len() > 8192 {
        return Err(FetchError::InvalidOrigin);
    }
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(FetchError::Cancelled),
        result = tokio::time::timeout(timeout, fetch_redirects(client, &mut url)) => {
            result.map_err(|_| FetchError::Timeout)?
        }
    }
}

async fn fetch_redirects(client: &Client, url: &mut Url) -> Result<Download, FetchError> {
    for redirects in 0..=3 {
        let mut response = client.get(url.clone()).send().await.map_err(|e| {
            if e.is_timeout() {
                FetchError::Timeout
            } else {
                FetchError::Network
            }
        })?;
        if response.status().is_redirection() {
            if redirects == 3 {
                return Err(FetchError::Redirect);
            }
            let header = response
                .headers()
                .get(LOCATION)
                .ok_or(FetchError::Redirect)?;
            let location = header.to_str().map_err(|_| FetchError::Redirect)?;
            let next = url.join(location).map_err(|_| FetchError::Redirect)?;
            if !valid_url(&next)
                || next.as_str().len() > 8192
                || (url.scheme() == "https" && next.scheme() != "https")
            {
                return Err(FetchError::Redirect);
            }
            *url = next;
            continue;
        }
        if !response.status().is_success() {
            return Err(FetchError::HttpStatus);
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_PLAYLIST_BYTES as u64)
        {
            return Err(FetchError::BodyTooLarge);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| {
            if e.is_timeout() {
                FetchError::Timeout
            } else {
                FetchError::Network
            }
        })? {
            if chunk.len() > MAX_PLAYLIST_BYTES.saturating_sub(bytes.len()) {
                return Err(FetchError::BodyTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        return Ok(Download {
            bytes,
            final_url: url.as_str().to_owned(),
        });
    }
    Err(FetchError::Redirect)
}

/// GtkFileDialog owns selection/portal permission. The returned gio::File must
/// be read on a GLib context, never by turning its path into a std::fs::File.
pub(crate) async fn fetch_local(
    file: &gio::File,
    cancellation: &Cancellation,
) -> Result<Vec<u8>, FetchError> {
    if cancellation.is_cancelled() {
        return Err(FetchError::Cancelled);
    }
    let stream = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err(FetchError::Cancelled),
        result = file.read_future(glib::Priority::DEFAULT) => {
            result.map_err(|_| FetchError::LocalAccess)?
        }
    };
    let mut contents = Vec::new();
    loop {
        if cancellation.is_cancelled() {
            return Err(FetchError::Cancelled);
        }
        let chunk = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(FetchError::Cancelled),
            result = stream.read_bytes_future(64 * 1024, glib::Priority::DEFAULT) => {
                result.map_err(|_| FetchError::LocalAccess)?
            }
        };
        if chunk.is_empty() {
            break;
        }
        if chunk.len() > MAX_PLAYLIST_BYTES.saturating_sub(contents.len()) {
            return Err(FetchError::BodyTooLarge);
        }
        contents.extend_from_slice(chunk.as_ref());
    }
    if cancellation.is_cancelled() {
        return Err(FetchError::Cancelled);
    }
    Ok(contents)
}

/// Portable when the underlying GIO implementation retains a usable URI.
/// A document-portal grant is not promised to survive reinstall/restart.
pub(crate) fn local_locator(file: &gio::File) -> Result<String, FetchError> {
    if file.uri_scheme().as_deref() != Some("file") {
        return Err(FetchError::InvalidOrigin);
    }
    let locator = file.uri().to_string();
    if locator.is_empty() || locator.len() > 8192 {
        return Err(FetchError::InvalidOrigin);
    }
    Ok(locator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_origin_and_sanitizes_errors() {
        for raw in [
            "file:///etc/passwd",
            "data:text/html,xyz",
            "ftp://localhost/x",
        ] {
            assert_eq!(fetch_remote_url_check(raw), Err(FetchError::InvalidOrigin));
        }
        for e in [
            FetchError::Network,
            FetchError::Redirect,
            FetchError::LocalAccess,
            FetchError::InvalidOrigin,
        ] {
            assert!(!format!("{e:?}: {e}").contains("CANARY-SECRET"));
        }
    }

    fn fetch_remote_url_check(value: &str) -> Result<(), FetchError> {
        let url = Url::parse(value).map_err(|_| FetchError::InvalidOrigin)?;
        if !valid_url(&url) {
            return Err(FetchError::InvalidOrigin);
        }
        Ok(())
    }

    #[tokio::test]
    async fn local_http_fetch_redirect_and_oversize() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buffer = [0u8; 1024];
                let size = stream.read(&mut buffer).await.unwrap();
                let request = String::from_utf8_lossy(&buffer[..size]);
                let response = if request.starts_with("GET /redirect ") {
                    "HTTP/1.1 302 Found\r\nLocation: /playlist\r\nContent-Length: 0\r\n\r\n"
                        .to_string()
                } else if request.starts_with("GET /oversize ") {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                        MAX_PLAYLIST_BYTES + 1
                    )
                } else {
                    let body = "#EXTM3U\n#EXTINF:-1,Fixture\nhttps://example.invalid/stream\n";
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                };
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let client = http_client().unwrap();
        let cancellation = Cancellation::new();
        let fetched = fetch_remote(
            &client,
            &format!("http://{address}/redirect"),
            &cancellation,
        )
        .await
        .unwrap();
        assert!(fetched.final_url.ends_with("/playlist"));
        assert!(fetched.bytes.starts_with(b"#EXTM3U"));
        let too_big = fetch_remote(
            &client,
            &format!("http://{address}/oversize"),
            &cancellation,
        )
        .await;
        assert!(matches!(too_big, Err(FetchError::BodyTooLarge)));
        server.await.unwrap();
    }
    #[tokio::test]
    async fn rejects_malicious_redirect_redirect_loops_and_http_errors() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for _ in 0..6 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 1024];
                let count = stream.read(&mut request).await.unwrap();
                let first = String::from_utf8_lossy(&request[..count]);
                let response = if first.starts_with("GET /bad ") {
                    "HTTP/1.1 302 Found\r\nLocation: file:///etc/passwd\r\nContent-Length: 0\r\n\r\n"
                        .to_owned()
                } else if first.starts_with("GET /forbidden ") {
                    "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n".to_owned()
                } else {
                    let path = first
                        .split_ascii_whitespace()
                        .nth(1)
                        .unwrap()
                        .trim_start_matches("/chain-")
                        .parse::<usize>()
                        .unwrap();
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: /chain-{}\r\nContent-Length: 0\r\n\r\n",
                        path + 1
                    )
                };
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let client = http_client().unwrap();
        let token = Cancellation::new();
        for (path, expected) in [
            ("bad", FetchError::Redirect),
            ("chain-0", FetchError::Redirect),
            ("forbidden", FetchError::HttpStatus),
        ] {
            let answer = fetch_remote(&client, &format!("http://{address}/{path}"), &token).await;
            assert!(matches!(answer, Err(error) if error == expected));
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn bounded_total_deadline_and_pre_cancelled_request() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await;
        });
        let client = http_client().unwrap();
        let token = Cancellation::new();
        let url = format!("http://{address}/slow");
        let timed_out =
            fetch_remote_with_timeout(&client, &url, &token, Duration::from_millis(30)).await;
        assert!(matches!(timed_out, Err(FetchError::Timeout)));
        token.cancel();
        let cancelled = fetch_remote(&client, &url, &token).await;
        assert!(matches!(cancelled, Err(FetchError::Cancelled)));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn failed_connect_has_only_sanitized_error_without_userinfo() {
        let client = http_client().unwrap();
        let result = fetch_remote(
            &client,
            "http://synthetic-user:SECRET-CANARY@127.0.0.1:0/list",
            &Cancellation::new(),
        )
        .await;
        assert!(matches!(result, Err(FetchError::Network)));
        let error = result.err().unwrap();
        assert!(!format!("{error:?}: {error}").contains("SECRET-CANARY"));
    }

    #[tokio::test]
    async fn rejects_streamed_body_without_content_length_above_limit() {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            let block = vec![b'a'; 64 * 1024];
            let prefix = format!("{:x}\r\n", block.len());
            for _ in 0..=MAX_PLAYLIST_BYTES / block.len() {
                if socket.write_all(prefix.as_bytes()).await.is_err() {
                    break;
                }
                if socket.write_all(&block).await.is_err() {
                    break;
                }
                if socket.write_all(b"\r\n").await.is_err() {
                    break;
                }
            }
        });
        let client = http_client().unwrap();
        let result = fetch_remote(
            &client,
            &format!("http://{address}/stream"),
            &Cancellation::new(),
        )
        .await;
        assert!(matches!(result, Err(FetchError::BodyTooLarge)));
        server.await.unwrap();
    }
}
