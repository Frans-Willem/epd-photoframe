//! Thin helpers over `fluent_uri` for the two URL operations the app
//! does beyond plain parsing:
//!
//! - [`resolve`] — RFC 3986 §5 reference resolution (absolute base + a
//!   possibly-relative reference → absolute URL). Used to turn the
//!   server's `Refresh: … url=<raw>` value into a fully-qualified URL
//!   before stashing it in RTC memory for the next wake.
//! - [`set_query_variable`] — add / replace / remove a single query
//!   parameter by name. Used to tag our current-URL with an `action=…`
//!   marker per button press, and to strip the marker back off on the
//!   next Timer-driven wake.
//!
//! Also [`parse_http_url`]: decomposes an absolute `http://` URL into
//! the three values the TCP/HTTP path needs (`host`, `port`,
//! `path_and_query`). Shared between the fetch path in `main.rs` and
//! the portal's submit-time validation.

use alloc::format;
use alloc::string::String;

use fluent_uri::pct_enc::encoder::{Data, Query};
use fluent_uri::pct_enc::{EStr, EString};
use fluent_uri::{Uri, UriRef};

/// Resolve a (possibly relative) URL `other` against the absolute URL
/// `base`, returning an absolute URL. Full RFC 3986 §5 behaviour
/// (absolute references pass through; protocol-relative, root-relative,
/// and path-relative references inherit as specified; dot-segments are
/// folded).
///
/// Returns `None` if either input fails to parse, or if `base` is not
/// actually absolute (has no scheme).
pub fn resolve(base: &str, other: &str) -> Option<String> {
    let base = Uri::parse(base).ok()?;
    let other = UriRef::parse(other).ok()?;
    let resolved = other.resolve_against(&base).ok()?;
    Some(String::from(resolved.as_str()))
}

/// Set or remove a query parameter on `url` by name.
///
/// - `value = Some(v)`: any existing pairs whose key decodes to `name`
///   are removed, then `name=<v>` (with `v` percent-encoded) is
///   appended to the end of the query.
/// - `value = None`: any existing pairs whose key decodes to `name`
///   are removed. If that leaves the query empty, the `?` is dropped
///   too.
///
/// Other query pairs are preserved verbatim (same percent-encoding as
/// in the input). The rest of the URL — scheme, authority, path,
/// fragment — is preserved unchanged.
///
/// If `url` doesn't parse as a URI reference, it's returned as-is:
/// callers upstream have already validated the URLs we mutate, so a
/// parse failure here would be a bug, not a recoverable state.
pub fn set_query_variable(url: &str, name: &str, value: Option<&str>) -> String {
    let Ok(parsed) = UriRef::parse(url) else {
        return String::from(url);
    };

    let mut new_query = EString::<Query>::new();
    if let Some(q) = parsed.query() {
        for pair in q.split('&') {
            if pair.as_str().is_empty() {
                continue;
            }
            let (key_estr, _) = pair.split_once('=').unwrap_or((pair, EStr::EMPTY));
            if key_estr.decode().to_string().ok().as_deref() == Some(name) {
                continue;
            }
            if !new_query.as_str().is_empty() {
                new_query.push('&');
            }
            new_query.push_estr(pair);
        }
    }

    if let Some(v) = value {
        if !new_query.as_str().is_empty() {
            new_query.push('&');
        }
        new_query.encode_str::<Data>(name);
        new_query.push('=');
        new_query.encode_str::<Data>(v);
    }

    let mut out = String::new();
    if let Some(scheme) = parsed.scheme() {
        out.push_str(scheme.as_str());
        out.push(':');
    }
    if let Some(auth) = parsed.authority() {
        out.push_str("//");
        out.push_str(auth.as_str());
    }
    out.push_str(parsed.path().as_str());
    if !new_query.as_str().is_empty() {
        out.push('?');
        out.push_str(new_query.as_str());
    }
    if let Some(frag) = parsed.fragment() {
        out.push('#');
        out.push_str(frag.as_str());
    }
    out
}

/// Parse an `http://host[:port]/path[?query]` URL into the three
/// values the TCP dial + HTTP request path need. HTTPS is rejected up
/// front (we don't ship embedded-tls), as are non-HTTP schemes.
///
/// Defaults: missing port → `80`; missing path → `/`.
pub fn parse_http_url(url: &str) -> Result<(String, u16, String), String> {
    let parsed = Uri::parse(url).map_err(|e| format!("URL parse error in {}: {:?}", url, e))?;
    if parsed.scheme().as_str() != "http" {
        return Err(format!("URL must be http://… : {}", url));
    }
    let authority = parsed
        .authority()
        .ok_or_else(|| format!("Missing host in URL: {}", url))?;
    let host = authority.host();
    if host.is_empty() {
        return Err(format!("Empty host in {}", url));
    }
    let port = authority
        .port_to_u16()
        .map_err(|_| format!("Invalid port in {}", url))?
        .unwrap_or(80);
    let mut path_and_query = String::from(parsed.path().as_str());
    if path_and_query.is_empty() {
        path_and_query.push('/');
    }
    if let Some(q) = parsed.query() {
        path_and_query.push('?');
        path_and_query.push_str(q.as_str());
    }
    Ok((String::from(host), port, path_and_query))
}
