//! Captive-portal HTTP server for configuration mode. Serves the same
//! HTML form for every GET (so iOS / Android / Windows probe URLs all
//! land on the portal), validates on `POST /save`, hands the credentials
//! back to `config_mode::run` via `SAVE_SIGNAL`, and returns a "saved,
//! rebooting" page on success. On validation failure the form is
//! re-served inline with a red error banner at the top and the fields
//! the user just submitted preserved so they can fix the offending one.

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

/// Sentinel written into the pre-filled password field when an existing
/// non-empty password is stored. Round-trips as the field's `value="…"`
/// in the served form; the `maxlength="64"` on the input means a user
/// who wants to set a new password can only type ≤ 64 chars, while an
/// untouched form submits the full sentinel. WPA2 passphrases are
/// 8–63 printable ASCII (or exactly 64 hex chars), so anything longer
/// than 64 provably *isn't* a passphrase — anything longer that's
/// *not* the sentinel is therefore a malformed submission.
pub const PASSWORD_SENTINEL: &str =
    "___________________________unchanged___________________________unchanged___________________________";

/// Device-specific label for the Refresh button — on the E1002 it's the
/// unmarked green button, so we spell that out in the instructions; on
/// the E1004 the button is clearly iconed and needs no qualifier. Used
/// both in the portal HTML and in the panel instructions rendered by
/// `config_mode`.
#[cfg(feature = "e1002")]
pub const REFRESH_BUTTON_LABEL: &str = "Refresh button (green)";
#[cfg(feature = "e1004")]
pub const REFRESH_BUTTON_LABEL: &str = "Refresh button";

const FORM_TEMPLATE: &str = include_str!("portal/form.html");
const SAVED_HTML: &[u8] = include_bytes!("portal/saved.html");

/// Values parsed from the submitted form, handed from the web handler
/// to `config_mode::run` so the latter can actually touch NVS and
/// reboot. `password = None` means the sentinel round-tripped through
/// the form — NVS should be left alone for that field.
#[derive(Debug, Clone)]
pub struct PortalCreds {
    pub ssid: String,
    pub password: Option<String>,
    pub url: String,
}

/// Raw fields pulled out of a POST body before validation. Kept around
/// so the error re-render can echo the user's submission back into the
/// form.
struct RawSubmission {
    ssid: String,
    password: String,
    url: String,
}

/// Reasons a submitted form is rejected. Each maps to a user-facing
/// message on the re-rendered form.
enum FormError {
    MissingSsid,
    MissingUrl,
    MalformedUrl,
    MalformedPassword,
    /// Body wasn't valid URL-encoded UTF-8; nothing to echo back. The
    /// caller renders with the stored defaults instead of submitted
    /// values.
    Unparseable,
}

impl FormError {
    fn message(&self) -> &'static str {
        match self {
            Self::MissingSsid => "WiFi SSID is required.",
            Self::MissingUrl => "Image URL is required.",
            Self::MalformedUrl => "Image URL must start with http://.",
            Self::MalformedPassword => {
                "WiFi password is longer than 64 characters; WPA2 allows at most 63."
            }
            Self::Unparseable => "Could not read the submitted form.",
        }
    }
}

/// Single global handoff between the web handler and the config-mode
/// task. Fired once at `POST /save` with the parsed form.
pub static SAVE_SIGNAL: Signal<CriticalSectionRawMutex, PortalCreds> = Signal::new();

/// Render the form HTML with the given field values and an optional
/// error banner. SSID / URL values are HTML-attribute-escaped so
/// arbitrary user input can't break out of the `value="…"` attribute.
/// The password field is echoed back verbatim (modulo escaping) — the
/// user and the browser already saw it when it was typed, so echoing
/// it on a re-render leaks nothing and spares them a retype.
fn render_form(ssid: &str, password: &str, url: &str, error: Option<&str>) -> String {
    let error_html = match error {
        Some(msg) => alloc::format!(r#"<p class="error">{}</p>"#, html_attr_escape(msg)),
        None => String::new(),
    };
    FORM_TEMPLATE
        .replace("{error}", &error_html)
        .replace("{ssid}", &html_attr_escape(ssid))
        .replace("{password}", &html_attr_escape(password))
        .replace("{url}", &html_attr_escape(url))
        .replace("{refresh_hint}", REFRESH_BUTTON_LABEL)
}

/// Minimal HTML escape for values going into a double-quoted attribute.
/// `&` must come first so we don't double-escape the other replacements.
fn html_attr_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

pub struct PortalHandler {
    /// NVS-derived defaults used to pre-fill the form on GET / and on
    /// re-renders that don't have submitted values to echo. Rendered
    /// at request time rather than pre-built, so the GET and error
    /// paths go through one code path.
    default_ssid: String,
    default_url: String,
    default_password_is_set: bool,
}

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
            (req.method, req.headers.content_len().unwrap_or(0) as usize)
        };

        if method == Method::Post {
            let path_is_save = conn.headers()?.path == "/save";
            if path_is_save {
                return handle_save(conn, content_len).await;
            }
        }
        // Everything else (including all captive-portal probes) returns
        // the form, pre-filled with the NVS defaults and no error.
        // Logged so the serial output shows which URLs phones actually
        // hit when the OS detects the network.
        println!("portal: {:?} {}", method, conn.headers()?.path);
        let default_password = if self.default_password_is_set {
            PASSWORD_SENTINEL
        } else {
            ""
        };
        let html = render_form(
            &self.default_ssid,
            default_password,
            &self.default_url,
            None,
        );
        write_form(conn, &html, 200, "OK").await
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

    let raw = match parse_form(&body_buf[..filled]) {
        Some(r) => r,
        None => {
            println!("portal: form body did not parse");
            let html = render_form("", "", "", Some(FormError::Unparseable.message()));
            return write_form(conn, &html, 400, "Bad Request").await;
        }
    };

    match validate(&raw) {
        Ok(creds) => {
            println!(
                "portal: received config (ssid={:?}, pass={}, url={:?})",
                creds.ssid,
                match &creds.password {
                    Some(p) => alloc::format!("<{} chars>", p.len()),
                    None => String::from("<unchanged>"),
                },
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
            Ok(())
        }
        Err(err) => {
            println!("portal: validation failed: {}", err.message());
            let html = render_form(&raw.ssid, &raw.password, &raw.url, Some(err.message()));
            write_form(conn, &html, 400, "Bad Request").await
        }
    }
}

async fn write_form<T, const N: usize>(
    conn: &mut Connection<'_, T, N>,
    html: &str,
    status: u16,
    reason: &'static str,
) -> Result<(), Error<T::Error>>
where
    T: Read + Write + edge_nal::TcpSplit,
{
    conn.initiate_response(
        status,
        Some(reason),
        &[
            ("Content-Type", "text/html; charset=utf-8"),
            ("Cache-Control", "no-store"),
            ("Connection", "close"),
        ],
    )
    .await?;
    conn.write_all(html.as_bytes()).await?;
    Ok(())
}

fn parse_form(body: &[u8]) -> Option<RawSubmission> {
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
    Some(RawSubmission {
        ssid: ssid.unwrap_or_default(),
        password: password.unwrap_or_default(),
        url: url.unwrap_or_default(),
    })
}

fn validate(raw: &RawSubmission) -> Result<PortalCreds, FormError> {
    if raw.ssid.is_empty() {
        return Err(FormError::MissingSsid);
    }
    if raw.url.is_empty() {
        return Err(FormError::MissingUrl);
    }
    // `try_build_frame` requires http:// (no TLS in the build), so we
    // catch the typo at submit time rather than letting the next
    // refresh fail.
    if !raw.url.starts_with("http://") {
        return Err(FormError::MalformedUrl);
    }
    let password = if raw.password == PASSWORD_SENTINEL {
        // Untouched field → keep the stored password.
        None
    } else if raw.password.len() > 64 {
        // Longer than WPA2 allows and not the sentinel — malformed.
        return Err(FormError::MalformedPassword);
    } else {
        // Anything ≤ 64 is a real user-supplied value (including
        // empty, which means "open network").
        Some(raw.password.clone())
    };
    Ok(PortalCreds {
        ssid: raw.ssid.clone(),
        password,
        url: raw.url.clone(),
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

/// Embassy task: accept on TCP/:80 and run the edge-http server with a
/// `PortalHandler` holding the NVS defaults for the pre-fill. Two
/// concurrent handler tasks cover phones that pipeline captive-portal
/// probes without burning too much RAM on buffers.
#[embassy_executor::task]
pub async fn web_task(
    stack: Stack<'static>,
    ssid: String,
    url: String,
    password_is_set: bool,
) {
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
    let handler = PortalHandler {
        default_ssid: ssid,
        default_url: url,
        default_password_is_set: password_is_set,
    };
    println!("HTTP portal listening on :80");
    loop {
        match server.run(Some(10_000), acceptor, &handler).await {
            Ok(()) => println!("HTTP server returned Ok; restarting"),
            Err(e) => println!("HTTP server error ({:?}); restarting", e),
        }
    }
}
