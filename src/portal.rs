//! Captive-portal HTTP server for configuration mode. Serves the same
//! HTML form for every GET (so iOS / Android / Windows probe URLs all
//! land on the portal) and writes the submitted credentials to NVS on
//! `POST /save`, then signals the config-mode task to trigger a software
//! reset.

use alloc::string::String;
use core::fmt::{Debug, Display};

use edge_http::Method;
use edge_http::io::Error;
use edge_http::io::server::{Connection, Handler};
use embassy_net::Stack;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embedded_io_async::{Read, Write};
use esp_println::println;

/// Values parsed from the submitted form, handed from the web handler to
/// `config_mode::run` so the latter can actually touch NVS + reboot.
#[derive(Debug, Clone)]
pub struct PortalCreds {
    pub ssid: String,
    pub password: String,
    pub url: String,
}

/// Single global handoff between the web handler and the config-mode
/// task. Fired once at `POST /save` with the parsed form; the config-mode
/// task does the NVS writes and `software_reset()` so the HTTP response
/// has time to flush before we reboot.
pub static SAVE_SIGNAL: Signal<CriticalSectionRawMutex, PortalCreds> = Signal::new();

const PORTAL_HTML: &[u8] = br#"<!DOCTYPE html>
<html><head>
<title>reTerminal Setup</title>
<meta name="viewport" content="width=device-width,initial-scale=1">
<style>body{font-family:sans-serif;max-width:400px;margin:2em auto;padding:1em}
input{width:100%;font-size:1em;padding:.5em;margin:.25em 0;box-sizing:border-box}
label{display:block;margin-top:1em;font-weight:bold}
button{padding:.75em 1.5em;font-size:1em;margin-top:1em}</style>
</head><body>
<h1>reTerminal Setup</h1>
<form method="POST" action="/save">
<label>WiFi SSID<input name="ssid" required maxlength="32" autocomplete="off"></label>
<label>WiFi Password<input name="password" type="password" maxlength="64" autocomplete="off"></label>
<label>Image URL<input name="url" required maxlength="256" autocomplete="off"></label>
<button type="submit">Save &amp; Restart</button>
</form></body></html>"#;

const SAVED_HTML: &[u8] = br#"<!DOCTYPE html>
<html><body style="font-family:sans-serif;max-width:400px;margin:2em auto;padding:1em">
<h1>Saved</h1><p>Restarting the device now. It will exit configuration mode and start fetching images using the new settings.</p>
</body></html>"#;

const BAD_FORM_HTML: &[u8] = br#"<!DOCTYPE html>
<html><body><h1>Invalid form</h1><p>Missing SSID or URL.</p><p><a href="/">Back</a></p></body></html>"#;

pub struct PortalHandler;

impl Handler for PortalHandler {
    type Error<E>
        = Error<E>
    where
        E: Debug;

    async fn handle<T, const N: usize>(
        &self,
        _task_id: impl Display + Copy,
        conn: &mut Connection<'_, T, N>,
    ) -> Result<(), Self::Error<T::Error>>
    where
        T: Read + Write + edge_nal::TcpSplit,
    {
        // Snapshot what we need from the request before we split the
        // connection for body reading / response writing.
        let (method, content_len) = {
            let req = conn.headers()?;
            (
                req.method,
                req.headers.content_len().unwrap_or(0) as usize,
            )
        };

        if method == Method::Post {
            let path_is_save = conn.headers()?.path == "/save";
            if path_is_save {
                return handle_save(conn, content_len).await;
            }
        }
        // Everything else (including all captive-portal probes) returns
        // the form. Logged so the serial output shows which URLs phones
        // actually hit when the OS detects the network.
        println!("portal: {:?} {}", method, conn.headers()?.path);
        conn.initiate_response(
            200,
            Some("OK"),
            &[
                ("Content-Type", "text/html; charset=utf-8"),
                ("Cache-Control", "no-store"),
                ("Connection", "close"),
            ],
        )
        .await?;
        conn.write_all(PORTAL_HTML).await?;
        Ok(())
    }
}

async fn handle_save<T, const N: usize>(
    conn: &mut Connection<'_, T, N>,
    content_len: usize,
) -> Result<(), Error<T::Error>>
where
    T: Read + Write + edge_nal::TcpSplit,
{
    let mut body_buf = [0u8; 1024];
    let to_read = content_len.min(body_buf.len());
    let mut filled = 0;
    {
        let (_headers, body) = conn.split();
        while filled < to_read {
            match body.read(&mut body_buf[filled..to_read]).await {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(_) => break,
            }
        }
    }

    match parse_form(&body_buf[..filled]) {
        Some(creds) => {
            println!(
                "portal: received config (ssid={:?}, pass=<{} chars>, url={:?})",
                creds.ssid,
                creds.password.len(),
                creds.url
            );
            SAVE_SIGNAL.signal(creds);
            conn.initiate_response(
                200,
                Some("OK"),
                &[
                    ("Content-Type", "text/html; charset=utf-8"),
                    ("Connection", "close"),
                ],
            )
            .await?;
            conn.write_all(SAVED_HTML).await?;
        }
        None => {
            println!("portal: form did not parse");
            conn.initiate_response(
                400,
                Some("Bad Request"),
                &[
                    ("Content-Type", "text/html; charset=utf-8"),
                    ("Connection", "close"),
                ],
            )
            .await?;
            conn.write_all(BAD_FORM_HTML).await?;
        }
    }
    Ok(())
}

fn parse_form(body: &[u8]) -> Option<PortalCreds> {
    let s = core::str::from_utf8(body).ok()?;
    let mut ssid: Option<String> = None;
    let mut password: Option<String> = None;
    let mut url: Option<String> = None;
    for pair in s.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        let decoded = url_decode(v);
        match k {
            "ssid" => ssid = Some(decoded),
            "password" => password = Some(decoded),
            "url" => url = Some(decoded),
            _ => {}
        }
    }
    Some(PortalCreds {
        ssid: ssid.filter(|s| !s.is_empty())?,
        password: password.unwrap_or_default(),
        url: url.filter(|s| !s.is_empty())?,
    })
}

fn url_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push(((hi as u8) << 4 | lo as u8) as char);
                    i += 3;
                } else {
                    out.push(bytes[i] as char);
                    i += 1;
                }
            }
            b => {
                out.push(b as char);
                i += 1;
            }
        }
    }
    out
}

/// Embassy task: accept on TCP/:80 and run the edge-http server with
/// `PortalHandler`. Two concurrent handler tasks covers phones that
/// pipeline captive-portal probes without burning too much RAM on buffers.
#[embassy_executor::task]
pub async fn web_task(stack: Stack<'static>) {
    use core::net::{IpAddr, Ipv4Addr, SocketAddr};

    use edge_nal::TcpBind;

    let tcp_buffers: edge_nal_embassy::TcpBuffers<2, 1024, 1024> =
        edge_nal_embassy::TcpBuffers::new();
    let tcp = edge_nal_embassy::Tcp::new(stack, &tcp_buffers);
    let acceptor = tcp
        .bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 80))
        .await
        .expect("bind :80");
    let mut server: edge_http::io::server::Server<2, 1024, 32> =
        edge_http::io::server::Server::new();
    println!("HTTP portal listening on :80");
    loop {
        match server.run(Some(10_000), acceptor, &PortalHandler).await {
            Ok(()) => println!("HTTP server returned Ok; restarting"),
            Err(e) => println!("HTTP server error ({:?}); restarting", e),
        }
    }
}
