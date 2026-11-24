// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Mycena Project
//! Minimal HTTP server for offline mobile app distribution from desktop (same LAN, no internet).

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

pub const SHARE_PORT: u16 = 8765;
const MAX_TRANSFERS: u32 = 32;

const IOS_BETA_URL: &str = "https://mycena-beta.vercel.app/";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareServerStatus {
    pub active: bool,
    pub base_url: Option<String>,
    pub android_url: Option<String>,
    pub ios_url: Option<String>,
    pub android_apk_available: bool,
    pub apk_path: String,
    pub transfer_count: u32,
    pub port: u16,
}

pub struct ShareServer {
    apk_path: Mutex<PathBuf>,
    running: Arc<AtomicBool>,
    transfer_count: Arc<AtomicU32>,
    base_url: Mutex<Option<String>>,
    share_token: Mutex<Option<String>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl ShareServer {
    pub fn new(apk_path: PathBuf) -> Self {
        Self {
            apk_path: Mutex::new(apk_path),
            running: Arc::new(AtomicBool::new(false)),
            transfer_count: Arc::new(AtomicU32::new(0)),
            base_url: Mutex::new(None),
            share_token: Mutex::new(None),
            task: Mutex::new(None),
        }
    }

    pub async fn set_apk_path(&self, path: PathBuf) {
        *self.apk_path.lock().await = path;
    }

    pub async fn status(&self) -> ShareServerStatus {
        let apk_path = self.apk_path.lock().await.clone();
        let base_url = self.base_url.lock().await.clone();
        ShareServerStatus {
            active: self.running.load(Ordering::SeqCst),
            android_url: base_url.as_ref().map(|u| format!("{u}android")),
            ios_url: base_url.as_ref().map(|u| format!("{u}ios")),
            android_apk_available: apk_path.is_file(),
            apk_path: apk_path.to_string_lossy().into_owned(),
            transfer_count: self.transfer_count.load(Ordering::SeqCst),
            base_url,
            port: SHARE_PORT,
        }
    }

    pub async fn start(&self) -> Result<String, String> {
        if self.running.load(Ordering::SeqCst) {
            return self
                .base_url
                .lock()
                .await
                .clone()
                .ok_or_else(|| "share server starting".to_string());
        }

        let ip = local_ipv4().ok_or_else(|| {
            "No local network address found. Connect to Wi‑Fi or Ethernet first.".to_string()
        })?;
        let token = uuid::Uuid::new_v4().simple().to_string();
        let base_url = format!("http://{ip}:{SHARE_PORT}/s/{token}/");
        let listener = TcpListener::bind((ip, SHARE_PORT))
            .await
            .map_err(|e| format!("Could not start share server on {ip}:{SHARE_PORT}: {e}"))?;

        self.running.store(true, Ordering::SeqCst);
        self.transfer_count.store(0, Ordering::SeqCst);
        *self.base_url.lock().await = Some(base_url.clone());
        *self.share_token.lock().await = Some(token.clone());

        let running = Arc::clone(&self.running);
        let transfer_count = Arc::clone(&self.transfer_count);
        let apk_path = self.apk_path.lock().await.clone();
        let expected_token = token;

        let handle = tokio::spawn(async move {
            loop {
                if !running.load(Ordering::SeqCst) {
                    break;
                }
                let accept = listener.accept().await;
                let Ok((socket, _)) = accept else {
                    if running.load(Ordering::SeqCst) {
                        tracing::warn!("share server accept failed");
                    }
                    continue;
                };
                if transfer_count.load(Ordering::SeqCst) >= MAX_TRANSFERS {
                    tracing::warn!("share server transfer cap reached");
                    continue;
                }
                let path = apk_path.clone();
                let transfers = Arc::clone(&transfer_count);
                let keep_running = Arc::clone(&running);
                let tok = expected_token.clone();
                tokio::spawn(async move {
                    if keep_running.load(Ordering::SeqCst) {
                        if let Err(e) = handle_connection(socket, &path, transfers, &tok).await {
                            tracing::debug!("share connection: {e}");
                        }
                    }
                });
            }
        });

        *self.task.lock().await = Some(handle);
        Ok(base_url)
    }

    pub async fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(task) = self.task.lock().await.take() {
            task.abort();
        }
        *self.base_url.lock().await = None;
        *self.share_token.lock().await = None;
    }
}

async fn handle_connection(
    mut socket: tokio::net::TcpStream,
    apk_path: &Path,
    transfer_count: Arc<AtomicU32>,
    expected_token: &str,
) -> Result<(), String> {
    let mut buf = vec![0u8; 4096];
    let n = socket.read(&mut buf).await.map_err(|e| e.to_string())?;
    if n == 0 {
        return Ok(());
    }
    let request = String::from_utf8_lossy(&buf[..n]);
    let request_line = request.lines().next().unwrap_or("");
    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let Some(rest) = authorized_share_rest(path, expected_token) else {
        return write_http(&mut socket, 404, "text/plain; charset=utf-8", b"not found").await;
    };
    let route = rest.trim_start_matches('/');
    match route {
        p if p.starts_with("mycena.apk") => serve_apk(&mut socket, apk_path, &transfer_count).await,
        "android" => write_html(&mut socket, &android_install_html(apk_path.is_file())).await,
        "ios" => write_html(&mut socket, &ios_install_html()).await,
        _ => write_html(&mut socket, &hub_html(apk_path.is_file())).await,
    }
}

async fn write_http(
    socket: &mut tokio::net::TcpStream,
    code: u16,
    content_type: &str,
    body: &[u8],
) -> Result<(), String> {
    let reason = if code == 404 { "Not Found" } else { "OK" };
    let header = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    socket
        .write_all(header.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    socket.write_all(body).await.map_err(|e| e.to_string())?;
    socket.flush().await.map_err(|e| e.to_string())?;
    Ok(())
}

async fn write_html(socket: &mut tokio::net::TcpStream, body: &str) -> Result<(), String> {
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    socket
        .write_all(header.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    socket
        .write_all(body.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    socket.flush().await.map_err(|e| e.to_string())?;
    Ok(())
}

async fn serve_apk(
    socket: &mut tokio::net::TcpStream,
    apk_path: &Path,
    transfer_count: &Arc<AtomicU32>,
) -> Result<(), String> {
    if !apk_path.is_file() {
        return write_html(socket, &android_missing_apk_html()).await;
    }
    let bytes = tokio::fs::read(apk_path)
        .await
        .map_err(|e| format!("Could not read APK: {e}"))?;
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.android.package-archive\r\nContent-Disposition: attachment; filename=\"mycena.apk\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        bytes.len()
    );
    socket
        .write_all(header.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    socket.write_all(&bytes).await.map_err(|e| e.to_string())?;
    socket.flush().await.map_err(|e| e.to_string())?;
    transfer_count.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

pub fn hub_html(apk_available: bool) -> String {
    let android = if apk_available {
        r#"<a href="android">Install on Android</a>"#
    } else {
        r#"<p>Android APK not loaded on this computer yet.</p><a href="android">Android instructions</a>"#
    };
    format!(
        r#"<!DOCTYPE html>
<html lang="en"><head>
<meta charset="utf-8"/><meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>Mycena — shared offline</title>
<style>
body {{ font-family: system-ui, sans-serif; margin: 2rem; text-align: center; }}
a {{ display: block; margin: 1rem auto; padding: 1rem 1.5rem; max-width: 16rem;
    background: #2e7d32; color: #fff; text-decoration: none; border-radius: 8px; }}
.secondary {{ background: #1565c0; }}
</style></head><body>
<h1>Mycena</h1>
<p>Offline mesh messaging — shared from a friend&apos;s computer nearby.</p>
<p style="font-size:13px;color:#555">HTTP on the local network only. Anyone on this Wi‑Fi with this link can download. Prefer a checksum from the person sharing.</p>
{android}
<a class="secondary" href="ios">iPhone / iOS</a>
</body></html>"#
    )
}

pub fn android_install_html(apk_available: bool) -> String {
    let action = if apk_available {
        r#"<a href="mycena.apk">Download &amp; install Mycena</a>
<p style="font-size:14px;color:#555">Open in Chrome. Allow installs from this source if prompted.</p>"#
    } else {
        r#"<p>The host computer does not have an Android APK loaded yet.</p>
<p style="font-size:14px;color:#555">Ask them to add <code>mycena.apk</code> in Mycena desktop → Share.</p>"#
    };
    format!(
        r#"<!DOCTYPE html>
<html lang="en"><head>
<meta charset="utf-8"/><meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>Install Mycena — Android</title>
<style>
body {{ font-family: system-ui, sans-serif; margin: 2rem; text-align: center; }}
a {{ display: inline-block; margin-top: 1.5rem; padding: 1rem 1.5rem;
    background: #2e7d32; color: #fff; text-decoration: none; border-radius: 8px; }}
</style></head><body>
<h1>Mycena for Android</h1>
<p>No Play Store or internet required on this Wi‑Fi network.</p>
{action}
<p><a href="./">← Back</a></p>
</body></html>"#
    )
}

pub fn android_missing_apk_html() -> String {
    android_install_html(false)
}

pub fn ios_install_html() -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en"><head>
<meta charset="utf-8"/><meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>Mycena — iPhone</title>
<style>
body {{ font-family: system-ui, sans-serif; margin: 2rem; text-align: center; }}
a {{ display: inline-block; margin-top: 1rem; padding: 0.85rem 1.25rem;
    background: #1565c0; color: #fff; text-decoration: none; border-radius: 8px; }}
.note {{ font-size: 14px; color: #555; max-width: 22rem; margin: 1rem auto; }}
</style></head><body>
<h1>Mycena for iPhone</h1>
<p class="note">The native iOS app is not available for offline install yet (Apple does not allow APK-style sharing).</p>
<p class="note">When you have internet again, check the beta page for TestFlight or App Store updates.</p>
<a href="{IOS_BETA_URL}">Open Mycena beta page</a>
<p class="note">Save this link or scan the iOS QR from desktop when online.</p>
<p><a href="./">← Back</a></p>
</body></html>"#
    )
}

/// Path after `/s/{token}/`, or `None` if the token is missing/wrong.
fn authorized_share_rest<'a>(path: &'a str, expected_token: &str) -> Option<&'a str> {
    let path = path.split('?').next().unwrap_or(path);
    let rest = path.strip_prefix("/s/")?;
    let (tok, remainder) = rest.split_once('/').unwrap_or((rest, ""));
    (tok == expected_token).then_some(remainder)
}

pub fn local_ipv4() -> Option<Ipv4Addr> {
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("192.0.2.1:80").is_ok() {
            if let Ok(addr) = socket.local_addr() {
                if let std::net::IpAddr::V4(v4) = addr.ip() {
                    if !v4.is_loopback() {
                        return Some(v4);
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn share_urls_require_token() {
        assert!(authorized_share_rest("/", "abc").is_none());
        assert!(authorized_share_rest("/s/wrong/android", "abc").is_none());
        assert_eq!(
            authorized_share_rest("/s/abc/android", "abc"),
            Some("android")
        );
        assert_eq!(authorized_share_rest("/s/abc/", "abc"), Some(""));
    }

    #[test]
    fn hub_html_links_android_and_ios() {
        let html = hub_html(true);
        assert!(html.contains("href=\"android\"") || html.contains("href=\"mycena.apk\""));
        assert!(html.contains("href=\"ios\""));
    }

    #[test]
    fn android_page_offers_apk_when_available() {
        let html = android_install_html(true);
        assert!(html.contains("mycena.apk"));
    }

    #[test]
    fn ios_page_mentions_beta_url() {
        let html = ios_install_html();
        assert!(html.contains(IOS_BETA_URL));
    }
}
