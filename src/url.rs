//! Minimal URL helpers for combining a base URL with a (possibly
//! relative) second URL and for appending `key=value` query
//! parameters. Enough for our two use sites — resolving the server's
//! `Refresh:` header `url=` against the URL we just fetched, and
//! tacking `?action=refresh` onto the configured base — without
//! pulling in a full URL crate.

use alloc::format;
use alloc::string::String;

/// Resolve a (possibly relative) URL `other` against `base`, returning
/// an absolute URL. Supports:
///
/// - `http://…` / `https://…` in `other` → returned as-is.
/// - `//host/path` (protocol-relative) → scheme from `base`.
/// - `/path` (absolute-path) → scheme + authority from `base`.
/// - `path` (relative-path) → scheme + authority + `base`'s directory
///   part plus `other`. No dot-segment folding (`..`).
///
/// Returns `None` if `base` itself doesn't parse as `scheme://host…` —
/// callers should either check beforehand (the portal's URL
/// validation already does) or treat `None` as "drop the override."
pub fn resolve(base: &str, other: &str) -> Option<String> {
    if other.starts_with("http://") || other.starts_with("https://") {
        return Some(String::from(other));
    }
    let scheme_end = base.find("://")?;
    let scheme = &base[..scheme_end];
    let after_scheme = &base[scheme_end + 3..];
    let (authority, base_path) = match after_scheme.find('/') {
        Some(i) => (&after_scheme[..i], &after_scheme[i..]),
        None => (after_scheme, "/"),
    };
    if let Some(host_and_path) = other.strip_prefix("//") {
        return Some(format!("{}://{}", scheme, host_and_path));
    }
    if other.starts_with('/') {
        return Some(format!("{}://{}{}", scheme, authority, other));
    }
    let dir = match base_path.rfind('/') {
        Some(i) => &base_path[..=i],
        None => "/",
    };
    Some(format!("{}://{}{}{}", scheme, authority, dir, other))
}

/// Append `key=value` to `url` as a new query parameter, picking `?`
/// or `&` as the separator depending on whether `url` already carries
/// a query string. Neither `key` nor `value` is URL-encoded — they
/// go in verbatim, so the caller is responsible for passing values
/// that don't need escaping.
pub fn append_query_param(url: &str, key: &str, value: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{}{}{}={}", url, sep, key, value)
}

/// Remove every occurrence of a `key=…` query parameter from `url`,
/// keeping the rest of the query intact. If the query becomes empty
/// as a result, the trailing `?` is dropped too.
pub fn strip_query_param(url: &str, key: &str) -> String {
    let Some((prefix, query)) = url.split_once('?') else {
        return String::from(url);
    };
    let mut out = String::new();
    for pair in query.split('&') {
        let drop = pair == key
            || pair
                .split_once('=')
                .map(|(k, _)| k == key)
                .unwrap_or(false);
        if drop {
            continue;
        }
        if !out.is_empty() {
            out.push('&');
        }
        out.push_str(pair);
    }
    if out.is_empty() {
        String::from(prefix)
    } else {
        format!("{}?{}", prefix, out)
    }
}
