//! Dedup engine — deduplicates URLs by their normalized form.
//!
//! Uses a Cuckoo filter for probabilistic membership testing on normalized URLs.
//!
//! For scale beyond typical bug-bounty targets (>5 GB RAM usage), a redb
//! on-disk spill can be added later — see the TODO comment below.

use std::collections::hash_map::DefaultHasher;

use cuckoofilter::CuckooFilter;

// ---------------------------------------------------------------------------
// Normalization helper
// ---------------------------------------------------------------------------

/// Decode a single percent-encoded byte if the decoded character is an
/// RFC 3986 "unreserved" character (alphanumeric or `-`, `.`, `_`, `~`).
/// All other percent-encoded sequences are left as-is.
fn decode_safe_percent(encoded: &str) -> String {
    let bytes = encoded.as_bytes();
    let mut out = String::with_capacity(encoded.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            // Try to parse the two hex digits that follow.
            let hi = bytes[i + 1];
            let lo = bytes[i + 2];
            if let (Some(h), Some(l)) = (hex_nibble(hi), hex_nibble(lo)) {
                let byte = (h << 4) | l;
                if is_unreserved(byte) {
                    out.push(byte as char);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

#[inline]
fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[inline]
fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

// ---------------------------------------------------------------------------
// Public: normalize
// ---------------------------------------------------------------------------

/// Normalize a URL according to gau's canonicalization rules:
///
/// 1. Lowercase scheme and host.
/// 2. Strip default ports (`80` for http, `443` for https).
/// 3. Drop the fragment (`#...`).
/// 4. Preserve trailing slash as-is.
/// 5. Do NOT sort query parameters.
/// 6. Decode safe (RFC 3986 unreserved) percent-encodings; leave others intact.
///
/// Returns the original string unchanged if it cannot be parsed as a URL.
pub fn normalize(url: &str) -> String {
    // Strip fragment manually before handing to the `url` crate — the crate
    // preserves fragments in its serialization when they are present.
    let without_fragment = match url.find('#') {
        Some(pos) => &url[..pos],
        None => url,
    };

    let parsed = match url::Url::parse(without_fragment) {
        Ok(u) => u,
        Err(_) => return url.to_string(),
    };

    // Scheme is already lowercased by the url crate.
    let scheme = parsed.scheme();

    // Host is lowercased by the url crate.
    let host = match parsed.host_str() {
        Some(h) => h,
        None => return url.to_string(),
    };

    // Port: omit if it is the scheme's default.
    let port: Option<u16> = parsed.port(); // None if port == scheme default
    // The `url` crate already strips default ports (80 for http, 443 for
    // https) from `port()`. If it returns Some(p) the port is non-default.

    // Path: preserve as-is from the raw string (the url crate may
    // percent-normalize, so we decode safe chars ourselves then re-apply).
    let raw_path = parsed.path();
    let path = decode_safe_percent(raw_path);

    // Query: preserve order and case; decode safe percent-encodings.
    let query: Option<String> = parsed.query().map(decode_safe_percent);

    // Re-assemble.
    let mut result = String::with_capacity(url.len());
    result.push_str(scheme);
    result.push_str("://");
    result.push_str(host);
    if let Some(p) = port {
        result.push(':');
        result.push_str(&p.to_string());
    }
    result.push_str(&path);
    if let Some(q) = query {
        result.push('?');
        result.push_str(&q);
    }

    result
}

// ---------------------------------------------------------------------------
// Public: Dedup
// ---------------------------------------------------------------------------

/// Cuckoo filter with 1M capacity (~8MB). False-positive rate is ~1-2%
/// at full capacity due to the crate's 1-byte fingerprint.
///
/// # Memory / spill
///
/// The in-memory Cuckoo filter handles ~500 k URLs comfortably in a few MB.
/// For larger workloads a redb on-disk spill would be needed; see the TODO
/// comment inside `check_and_insert`.
pub struct Dedup {
    filter: CuckooFilter<DefaultHasher>,
    seen_count: usize,
    duplicate_count: usize,
    // TODO: spill state for >5 GB RAM threshold (redb table handle, etc.)
}

impl Dedup {
    /// Create a new `Dedup` instance with a reasonable default capacity.
    ///
    /// The cuckoo filter is initialized for ~1 million items, which covers
    /// the vast majority of bug-bounty target sizes without significant RAM
    /// overhead (~8 MB).
    pub fn new() -> Self {
        Self {
            filter: CuckooFilter::with_capacity(1_000_000),
            seen_count: 0,
            duplicate_count: 0,
        }
    }

    /// Returns `true` if this URL has not been seen before (and inserts it),
    /// `false` if it is a duplicate.
    pub fn check_and_insert(&mut self, url: &str) -> bool {
        let normalized = normalize(url);
        // TODO: spill to redb above 5 GB RAM threshold
        if self.filter.contains(normalized.as_bytes()) {
            self.duplicate_count += 1;
            false
        } else {
            let _ = self.filter.add(normalized.as_bytes());
            self.seen_count += 1;
            true
        }
    }

    /// Total unique URLs admitted so far.
    pub fn seen_count(&self) -> usize {
        self.seen_count
    }

    /// Total duplicate URLs rejected so far.
    pub fn duplicate_count(&self) -> usize {
        self.duplicate_count
    }
}

impl Default for Dedup {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- normalize tests ----

    #[test]
    fn normalize_lowercases_scheme_and_host() {
        let result = normalize("HTTP://Example.COM/path");
        assert!(result.starts_with("http://example.com/"));
    }

    #[test]
    fn normalize_strips_default_port_80_http() {
        let result = normalize("http://example.com:80/path");
        assert_eq!(result, "http://example.com/path");
    }

    #[test]
    fn normalize_strips_default_port_443_https() {
        let result = normalize("https://example.com:443/path");
        assert_eq!(result, "https://example.com/path");
    }

    #[test]
    fn normalize_keeps_non_default_port() {
        let result = normalize("http://example.com:8080/path");
        assert_eq!(result, "http://example.com:8080/path");
    }

    #[test]
    fn normalize_keeps_non_default_https_port() {
        let result = normalize("https://example.com:8443/api");
        assert_eq!(result, "https://example.com:8443/api");
    }

    #[test]
    fn normalize_drops_fragment() {
        let result = normalize("http://example.com/page#section");
        assert_eq!(result, "http://example.com/page");
    }

    #[test]
    fn normalize_drops_fragment_with_query() {
        let result = normalize("http://example.com/page?q=1#anchor");
        assert_eq!(result, "http://example.com/page?q=1");
    }

    #[test]
    fn normalize_preserves_trailing_slash() {
        let with_slash = normalize("http://example.com/path/");
        assert!(with_slash.ends_with('/'), "expected trailing slash, got: {with_slash}");

        let without_slash = normalize("http://example.com/path");
        assert!(!without_slash.ends_with('/'), "unexpected trailing slash: {without_slash}");
    }

    #[test]
    fn normalize_preserves_query_string_order() {
        let url = "http://example.com/path?b=2&a=1&c=3";
        let result = normalize(url);
        assert!(result.contains("?b=2&a=1&c=3"), "query order changed: {result}");
    }

    #[test]
    fn normalize_decodes_safe_percent_encodings() {
        // %41 = 'A' (alphanumeric), %2E = '.' (unreserved dot)
        let result = normalize("http://example.com/%41bc%2Ehtml");
        assert!(
            result.contains("/Abc.html"),
            "expected safe chars decoded, got: {result}"
        );
    }

    #[test]
    fn normalize_keeps_unsafe_percent_encodings() {
        // %20 = space — must NOT be decoded (not unreserved)
        let result = normalize("http://example.com/path%20with%20spaces");
        assert!(
            result.contains("%20"),
            "expected %20 preserved, got: {result}"
        );
    }

    #[test]
    fn normalize_preserves_query_param_case() {
        let url = "http://example.com/path?Token=ABC&user=john";
        let result = normalize(url);
        assert!(result.contains("Token=ABC"), "query case changed: {result}");
        assert!(result.contains("user=john"), "query case changed: {result}");
    }

    // ---- check_and_insert tests ----

    #[test]
    fn dedup_first_call_returns_true() {
        let mut d = Dedup::new();
        assert!(d.check_and_insert("http://example.com/page"));
        assert_eq!(d.seen_count(), 1);
        assert_eq!(d.duplicate_count(), 0);
    }

    #[test]
    fn dedup_second_call_same_url_returns_false() {
        let mut d = Dedup::new();
        assert!(d.check_and_insert("http://example.com/page"));
        assert!(!d.check_and_insert("http://example.com/page"));
        assert_eq!(d.seen_count(), 1);
        assert_eq!(d.duplicate_count(), 1);
    }

    #[test]
    fn dedup_normalized_variants_are_duplicates() {
        let mut d = Dedup::new();
        // Uppercase scheme+host and default port should normalize to same key.
        assert!(d.check_and_insert("http://example.com/"));
        assert!(!d.check_and_insert("HTTP://Example.com/"));
        assert!(!d.check_and_insert("http://example.com:80/"));
        assert_eq!(d.seen_count(), 1);
        assert_eq!(d.duplicate_count(), 2);
    }

    #[test]
    fn dedup_fragment_stripped_urls_are_duplicates() {
        let mut d = Dedup::new();
        assert!(d.check_and_insert("http://example.com/page"));
        // Same page with a fragment must map to the same dedup key.
        assert!(!d.check_and_insert("http://example.com/page#section"));
        assert_eq!(d.seen_count(), 1);
        assert_eq!(d.duplicate_count(), 1);
    }

    #[test]
    fn dedup_different_urls_not_flagged_as_duplicates() {
        let mut d = Dedup::new();
        assert!(d.check_and_insert("http://example.com/page1"));
        assert!(d.check_and_insert("http://example.com/page2"));
        assert!(d.check_and_insert("http://other.com/page1"));
        assert_eq!(d.seen_count(), 3);
        assert_eq!(d.duplicate_count(), 0);
    }

    #[test]
    fn dedup_query_string_differences_are_unique() {
        let mut d = Dedup::new();
        assert!(d.check_and_insert("http://example.com/page?a=1"));
        assert!(d.check_and_insert("http://example.com/page?a=2"));
        assert_eq!(d.seen_count(), 2);
    }
}
