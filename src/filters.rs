//! URL filtering logic for lancer-gau.
//!
//! All per-URL filtering (extension blacklist, status code match/exclude,
//! MIME match/exclude, regex match/exclude, date range) lives here.

use std::collections::HashSet;

use crate::config::Config;
use crate::providers::UrlRecord;

// ---------------------------------------------------------------------------
// Public Filter struct
// ---------------------------------------------------------------------------

/// Compiled filter state built once from the resolved [`Config`].
pub struct Filter {
    blacklist_exts: HashSet<String>,
    match_codes: Option<HashSet<u16>>,
    filter_codes: HashSet<u16>,
    match_mime: Option<HashSet<String>>,
    filter_mime: HashSet<String>,
    match_regex: Option<regex::Regex>,
    filter_regex: Option<regex::Regex>,
    from_date: Option<String>,
    to_date: Option<String>,
}

impl Filter {
    /// Build a `Filter` from the resolved configuration.
    pub fn from_config(cfg: &Config) -> anyhow::Result<Self> {
        if let Some(ref f) = cfg.from {
            if f.len() != 6 || !f.bytes().all(|b| b.is_ascii_digit()) {
                anyhow::bail!("--from must be YYYYMM (6 digits), got {:?}", f);
            }
        }
        if let Some(ref t) = cfg.to {
            if t.len() != 6 || !t.bytes().all(|b| b.is_ascii_digit()) {
                anyhow::bail!("--to must be YYYYMM (6 digits), got {:?}", t);
            }
        }
        if let (Some(f), Some(t)) = (&cfg.from, &cfg.to) {
            if f > t {
                anyhow::bail!("--from ({}) must be <= --to ({})", f, t);
            }
        }

        let blacklist_exts: HashSet<String> = cfg
            .blacklist
            .iter()
            .map(|e| e.trim_start_matches('.').to_lowercase())
            .collect();

        let match_codes: Option<HashSet<u16>> = if cfg.match_codes.is_empty() {
            None
        } else {
            Some(cfg.match_codes.iter().copied().collect())
        };

        let filter_codes: HashSet<u16> = cfg.filter_codes.iter().copied().collect();

        let match_mime: Option<HashSet<String>> = if cfg.match_mime.is_empty() {
            None
        } else {
            Some(
                cfg.match_mime
                    .iter()
                    .map(|m| m.to_lowercase())
                    .collect(),
            )
        };

        let filter_mime: HashSet<String> = cfg
            .filter_mime
            .iter()
            .map(|m| m.to_lowercase())
            .collect();

        let match_regex = cfg
            .match_regex
            .as_deref()
            .map(regex::Regex::new)
            .transpose()
            .map_err(|e| anyhow::anyhow!("Invalid --mr regex: {}", e))?;

        let filter_regex = cfg
            .filter_regex
            .as_deref()
            .map(regex::Regex::new)
            .transpose()
            .map_err(|e| anyhow::anyhow!("Invalid --fr regex: {}", e))?;

        Ok(Self {
            blacklist_exts,
            match_codes,
            filter_codes,
            match_mime,
            filter_mime,
            match_regex,
            filter_regex,
            from_date: cfg.from.clone(),
            to_date: cfg.to.clone(),
        })
    }

    /// Returns `true` if the record should be emitted; `false` to drop.
    pub fn accept(&self, record: &UrlRecord) -> bool {
        // 1. Extension blacklist
        if !self.blacklist_exts.is_empty() {
            if let Some(ext) = extract_extension(&record.url) {
                if self.blacklist_exts.contains(&ext) {
                    return false;
                }
            }
        }

        // 2. Status code match (if match_codes set, must be in set)
        if let Some(ref codes) = self.match_codes {
            match record.status_code {
                Some(code) if codes.contains(&code) => {}
                _ => return false,
            }
        }

        // 3. Status code filter (must NOT be in filter_codes)
        if !self.filter_codes.is_empty() {
            if let Some(code) = record.status_code {
                if self.filter_codes.contains(&code) {
                    return false;
                }
            }
        }

        // 4. MIME match
        if let Some(ref mime_set) = self.match_mime {
            match &record.mime_type {
                Some(mt) => {
                    let mt_lower = mt.to_lowercase();
                    // Compare base MIME (before ';')
                    let base = mt_lower.split(';').next().unwrap_or(&mt_lower).trim();
                    if !mime_set.contains(base) {
                        return false;
                    }
                }
                None => return false,
            }
        }

        // 5. MIME filter (must NOT match)
        if !self.filter_mime.is_empty() {
            if let Some(ref mt) = record.mime_type {
                let mt_lower = mt.to_lowercase();
                let base = mt_lower.split(';').next().unwrap_or(&mt_lower).trim();
                if self.filter_mime.contains(base) {
                    return false;
                }
            }
        }

        // 6. Regex match
        if let Some(ref re) = self.match_regex {
            if !re.is_match(&record.url) {
                return false;
            }
        }

        // 7. Regex filter (must NOT match)
        if let Some(ref re) = self.filter_regex {
            if re.is_match(&record.url) {
                return false;
            }
        }

        // 8. Date range
        if self.from_date.is_some() || self.to_date.is_some() {
            if let Some(ref ts) = record.timestamp {
                if let Some(yyyymm) = extract_yyyymm(ts) {
                    if let Some(ref from) = self.from_date {
                        if yyyymm.as_str() < from.as_str() {
                            return false;
                        }
                    }
                    if let Some(ref to) = self.to_date {
                        if yyyymm.as_str() > to.as_str() {
                            return false;
                        }
                    }
                }
                // If we couldn't extract YYYYMM, accept the record
            }
            // If timestamp is None, accept the record
        }

        true
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract the lowercased file extension from a URL, ignoring query strings
/// and fragments.
///
/// Note: URLs without a path component (e.g. "https://example.com") will
/// return the TLD ("com") as the extension. This is intentional and matches
/// the behavior of upstream gau.
fn extract_extension(url: &str) -> Option<String> {
    // Strip query string
    let path = url.split('?').next().unwrap_or(url);
    // Strip fragment
    let path = path.split('#').next().unwrap_or(path);
    // Take last path segment
    let last = path.rsplit('/').next()?;
    // Get the extension (part after last '.')
    let ext = last.rsplit('.').next()?;
    // Must have a dot; ext must not equal the whole segment
    if ext == last {
        return None;
    }
    Some(ext.to_lowercase())
}

/// Extract a YYYYMM string from a timestamp.
///
/// Handles:
/// - Wayback: `"20210115120000"` (14 digits, no separator)
/// - ISO 8601: `"2021-01-15T12:00:00Z"` (with `-` separator)
/// - OTX: `"2021-01-15T12:00:00"`
fn extract_yyyymm(timestamp: &str) -> Option<String> {
    if timestamp.len() < 6 {
        return None;
    }
    let bytes = timestamp.as_bytes();

    // First 4 chars must be digits (YYYY)
    let year: String = bytes[0..4].iter().map(|b| *b as char).collect();
    if !year.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    // MM starts at index 4 (no separator) or index 5 (after '-')
    let mm_start = if bytes[4] == b'-' { 5 } else { 4 };
    if bytes.len() < mm_start + 2 {
        return None;
    }
    let month: String = bytes[mm_start..mm_start + 2]
        .iter()
        .map(|b| *b as char)
        .collect();
    if !month.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    Some(format!("{}{}", year, month))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::providers::UrlRecord;

    // -- Helpers --

    fn make_record(url: &str) -> UrlRecord {
        UrlRecord {
            url: url.to_string(),
            source: "test".to_string(),
            status_code: None,
            mime_type: None,
            timestamp: None,
        }
    }

    fn default_filter() -> Filter {
        Filter::from_config(&Config {
            blacklist: vec![],
            match_codes: vec![],
            filter_codes: vec![],
            match_mime: vec![],
            filter_mime: vec![],
            match_regex: None,
            filter_regex: None,
            from: None,
            to: None,
            ..Config::default()
        })
        .unwrap()
    }

    // -----------------------------------------------------------------------
    // extract_extension tests
    // -----------------------------------------------------------------------

    #[test]
    fn ext_simple() {
        assert_eq!(extract_extension("https://example.com/foo.html"), Some("html".to_string()));
    }

    #[test]
    fn ext_with_query() {
        assert_eq!(
            extract_extension("https://example.com/page.php?id=1"),
            Some("php".to_string())
        );
    }

    #[test]
    fn ext_with_fragment() {
        assert_eq!(
            extract_extension("https://example.com/page.html#section"),
            Some("html".to_string())
        );
    }

    #[test]
    fn ext_with_query_and_fragment() {
        assert_eq!(
            extract_extension("https://example.com/page.html?a=1#top"),
            Some("html".to_string())
        );
    }

    #[test]
    fn ext_no_extension() {
        assert_eq!(extract_extension("https://example.com/about"), None);
    }

    #[test]
    fn ext_no_path() {
        // "https://example.com" — last segment after '/' is "example.com",
        // rsplit('.') yields "com" != "example.com", so Some("com") is returned.
        assert_eq!(extract_extension("https://example.com"), Some("com".to_string()));
    }

    #[test]
    fn ext_multi_dot() {
        assert_eq!(
            extract_extension("https://example.com/archive.tar.gz"),
            Some("gz".to_string())
        );
    }

    #[test]
    fn ext_lowercase() {
        assert_eq!(
            extract_extension("https://example.com/IMAGE.PNG"),
            Some("png".to_string())
        );
    }

    #[test]
    fn ext_root_path() {
        // "example.com" has a dot, but 'com' != the full last segment after splitting by '/'
        // The last segment after '/' is "example.com" and ext would be "com"
        // ext ("com") != last ("example.com"), so it returns Some("com")
        assert_eq!(extract_extension("https://example.com/"), None);
    }

    // -----------------------------------------------------------------------
    // extract_yyyymm tests
    // -----------------------------------------------------------------------

    #[test]
    fn yyyymm_wayback_format() {
        assert_eq!(extract_yyyymm("20210115120000"), Some("202101".to_string()));
    }

    #[test]
    fn yyyymm_iso8601() {
        assert_eq!(
            extract_yyyymm("2021-01-15T12:00:00Z"),
            Some("202101".to_string())
        );
    }

    #[test]
    fn yyyymm_otx_format() {
        assert_eq!(
            extract_yyyymm("2021-01-15T12:00:00"),
            Some("202101".to_string())
        );
    }

    #[test]
    fn yyyymm_iso_with_millis() {
        assert_eq!(
            extract_yyyymm("2021-01-15T12:00:00.000Z"),
            Some("202101".to_string())
        );
    }

    #[test]
    fn yyyymm_too_short() {
        assert_eq!(extract_yyyymm("2021"), None);
    }

    #[test]
    fn yyyymm_empty() {
        assert_eq!(extract_yyyymm(""), None);
    }

    #[test]
    fn yyyymm_non_digit_year() {
        assert_eq!(extract_yyyymm("abcd-01-15"), None);
    }

    #[test]
    fn yyyymm_non_digit_month() {
        assert_eq!(extract_yyyymm("2021-ab-15"), None);
    }

    // -----------------------------------------------------------------------
    // Filter::accept — extension blacklist
    // -----------------------------------------------------------------------

    #[test]
    fn blacklist_drops_extension() {
        let filter = Filter::from_config(&Config {
            blacklist: vec!["js".to_string()],
            ..Config::default()
        })
        .unwrap();
        let rec = make_record("https://example.com/app.js");
        assert!(!filter.accept(&rec));
    }

    #[test]
    fn blacklist_allows_other_extension() {
        let filter = Filter::from_config(&Config {
            blacklist: vec!["js".to_string()],
            ..Config::default()
        })
        .unwrap();
        let rec = make_record("https://example.com/page.html");
        assert!(filter.accept(&rec));
    }

    #[test]
    fn blacklist_case_insensitive() {
        let filter = Filter::from_config(&Config {
            blacklist: vec!["PNG".to_string()],
            ..Config::default()
        })
        .unwrap();
        let rec = make_record("https://example.com/image.png");
        assert!(!filter.accept(&rec));
    }

    #[test]
    fn blacklist_strips_leading_dot() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![".css".to_string()],
            ..Config::default()
        })
        .unwrap();
        let rec = make_record("https://example.com/style.css");
        assert!(!filter.accept(&rec));
    }

    // -----------------------------------------------------------------------
    // Filter::accept — status code match
    // -----------------------------------------------------------------------

    #[test]
    fn match_codes_accepts_matching_code() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            match_codes: vec![200],
            ..Config::default()
        })
        .unwrap();
        let mut rec = make_record("https://example.com/");
        rec.status_code = Some(200);
        assert!(filter.accept(&rec));
    }

    #[test]
    fn match_codes_drops_non_matching_code() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            match_codes: vec![200],
            ..Config::default()
        })
        .unwrap();
        let mut rec = make_record("https://example.com/");
        rec.status_code = Some(404);
        assert!(!filter.accept(&rec));
    }

    #[test]
    fn match_codes_drops_none_status_when_set() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            match_codes: vec![200],
            ..Config::default()
        })
        .unwrap();
        let rec = make_record("https://example.com/");
        assert!(!filter.accept(&rec));
    }

    #[test]
    fn match_codes_unset_accepts_any_status() {
        let filter = default_filter();
        let mut rec = make_record("https://example.com/");
        rec.status_code = Some(500);
        assert!(filter.accept(&rec));
    }

    // -----------------------------------------------------------------------
    // Filter::accept — status code filter
    // -----------------------------------------------------------------------

    #[test]
    fn filter_codes_drops_matching_code() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            filter_codes: vec![404],
            ..Config::default()
        })
        .unwrap();
        let mut rec = make_record("https://example.com/");
        rec.status_code = Some(404);
        assert!(!filter.accept(&rec));
    }

    #[test]
    fn filter_codes_allows_other_code() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            filter_codes: vec![404],
            ..Config::default()
        })
        .unwrap();
        let mut rec = make_record("https://example.com/");
        rec.status_code = Some(200);
        assert!(filter.accept(&rec));
    }

    #[test]
    fn filter_codes_accepts_none_status() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            filter_codes: vec![404],
            ..Config::default()
        })
        .unwrap();
        let rec = make_record("https://example.com/");
        assert!(filter.accept(&rec));
    }

    // -----------------------------------------------------------------------
    // Filter::accept — MIME match
    // -----------------------------------------------------------------------

    #[test]
    fn match_mime_accepts_matching_type() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            match_mime: vec!["text/html".to_string()],
            ..Config::default()
        })
        .unwrap();
        let mut rec = make_record("https://example.com/");
        rec.mime_type = Some("text/html".to_string());
        assert!(filter.accept(&rec));
    }

    #[test]
    fn match_mime_drops_non_matching_type() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            match_mime: vec!["text/html".to_string()],
            ..Config::default()
        })
        .unwrap();
        let mut rec = make_record("https://example.com/");
        rec.mime_type = Some("application/json".to_string());
        assert!(!filter.accept(&rec));
    }

    #[test]
    fn match_mime_drops_none_mime_when_set() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            match_mime: vec!["text/html".to_string()],
            ..Config::default()
        })
        .unwrap();
        let rec = make_record("https://example.com/");
        assert!(!filter.accept(&rec));
    }

    #[test]
    fn match_mime_strips_charset() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            match_mime: vec!["text/html".to_string()],
            ..Config::default()
        })
        .unwrap();
        let mut rec = make_record("https://example.com/");
        rec.mime_type = Some("text/html; charset=utf-8".to_string());
        assert!(filter.accept(&rec));
    }

    #[test]
    fn match_mime_partial_type_does_not_match() {
        let mut cfg = Config::default();
        cfg.match_mime = vec!["json".to_string()];
        let filter = Filter::from_config(&cfg).unwrap();
        let mut rec = make_record("https://example.com/api");
        rec.mime_type = Some("application/json".to_string());
        // partial "json" must NOT match "application/json" — full type required
        assert!(!filter.accept(&rec));
    }

    // -----------------------------------------------------------------------
    // Filter::accept — MIME filter
    // -----------------------------------------------------------------------

    #[test]
    fn filter_mime_drops_matching_type() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            filter_mime: vec!["text/css".to_string()],
            ..Config::default()
        })
        .unwrap();
        let mut rec = make_record("https://example.com/style.css");
        rec.mime_type = Some("text/css".to_string());
        assert!(!filter.accept(&rec));
    }

    #[test]
    fn filter_mime_allows_other_type() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            filter_mime: vec!["text/css".to_string()],
            ..Config::default()
        })
        .unwrap();
        let mut rec = make_record("https://example.com/");
        rec.mime_type = Some("text/html".to_string());
        assert!(filter.accept(&rec));
    }

    // -----------------------------------------------------------------------
    // Filter::accept — regex match
    // -----------------------------------------------------------------------

    #[test]
    fn match_regex_accepts_matching_url() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            match_regex: Some(r"\.php$".to_string()),
            ..Config::default()
        })
        .unwrap();
        let rec = make_record("https://example.com/page.php");
        assert!(filter.accept(&rec));
    }

    #[test]
    fn match_regex_drops_non_matching_url() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            match_regex: Some(r"\.php$".to_string()),
            ..Config::default()
        })
        .unwrap();
        let rec = make_record("https://example.com/page.html");
        assert!(!filter.accept(&rec));
    }

    #[test]
    fn match_regex_unset_accepts_all() {
        let filter = default_filter();
        let rec = make_record("https://example.com/anything");
        assert!(filter.accept(&rec));
    }

    // -----------------------------------------------------------------------
    // Filter::accept — regex filter
    // -----------------------------------------------------------------------

    #[test]
    fn filter_regex_drops_matching_url() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            filter_regex: Some(r"logout".to_string()),
            ..Config::default()
        })
        .unwrap();
        let rec = make_record("https://example.com/logout");
        assert!(!filter.accept(&rec));
    }

    #[test]
    fn filter_regex_accepts_non_matching_url() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            filter_regex: Some(r"logout".to_string()),
            ..Config::default()
        })
        .unwrap();
        let rec = make_record("https://example.com/dashboard");
        assert!(filter.accept(&rec));
    }

    // -----------------------------------------------------------------------
    // Filter::accept — date range
    // -----------------------------------------------------------------------

    #[test]
    fn date_from_accepts_record_on_boundary() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            from: Some("202101".to_string()),
            ..Config::default()
        })
        .unwrap();
        let mut rec = make_record("https://example.com/");
        rec.timestamp = Some("20210115120000".to_string());
        assert!(filter.accept(&rec));
    }

    #[test]
    fn date_from_drops_record_before_range() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            from: Some("202101".to_string()),
            ..Config::default()
        })
        .unwrap();
        let mut rec = make_record("https://example.com/");
        rec.timestamp = Some("20201201000000".to_string());
        assert!(!filter.accept(&rec));
    }

    #[test]
    fn date_to_accepts_record_on_boundary() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            to: Some("202112".to_string()),
            ..Config::default()
        })
        .unwrap();
        let mut rec = make_record("https://example.com/");
        rec.timestamp = Some("20211201000000".to_string());
        assert!(filter.accept(&rec));
    }

    #[test]
    fn date_to_drops_record_after_range() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            to: Some("202112".to_string()),
            ..Config::default()
        })
        .unwrap();
        let mut rec = make_record("https://example.com/");
        rec.timestamp = Some("20220101000000".to_string());
        assert!(!filter.accept(&rec));
    }

    #[test]
    fn date_range_accepts_none_timestamp() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            from: Some("202101".to_string()),
            to: Some("202112".to_string()),
            ..Config::default()
        })
        .unwrap();
        // No timestamp: should be accepted
        let rec = make_record("https://example.com/");
        assert!(filter.accept(&rec));
    }

    #[test]
    fn date_range_accepts_unparseable_timestamp() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            from: Some("202101".to_string()),
            ..Config::default()
        })
        .unwrap();
        let mut rec = make_record("https://example.com/");
        rec.timestamp = Some("unknown".to_string());
        assert!(filter.accept(&rec));
    }

    #[test]
    fn date_range_iso8601_timestamp() {
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            from: Some("202101".to_string()),
            to: Some("202112".to_string()),
            ..Config::default()
        })
        .unwrap();
        let mut rec = make_record("https://example.com/");
        rec.timestamp = Some("2021-06-15T12:00:00Z".to_string());
        assert!(filter.accept(&rec));
    }

    // -----------------------------------------------------------------------
    // Combined filter scenarios
    // -----------------------------------------------------------------------

    #[test]
    fn combined_all_pass() {
        let filter = Filter::from_config(&Config {
            blacklist: vec!["js".to_string()],
            match_codes: vec![200],
            filter_codes: vec![],
            match_mime: vec!["text/html".to_string()],
            filter_mime: vec![],
            match_regex: Some(r"\.html$".to_string()),
            filter_regex: None,
            from: Some("202101".to_string()),
            to: Some("202112".to_string()),
            ..Config::default()
        })
        .unwrap();
        let mut rec = make_record("https://example.com/page.html");
        rec.status_code = Some(200);
        rec.mime_type = Some("text/html".to_string());
        rec.timestamp = Some("2021-06-15T12:00:00Z".to_string());
        assert!(filter.accept(&rec));
    }

    #[test]
    fn combined_blacklist_blocks_even_if_codes_match() {
        let filter = Filter::from_config(&Config {
            blacklist: vec!["js".to_string()],
            match_codes: vec![200],
            ..Config::default()
        })
        .unwrap();
        let mut rec = make_record("https://example.com/app.js");
        rec.status_code = Some(200);
        assert!(!filter.accept(&rec));
    }

    #[test]
    fn combined_regex_filter_wins_over_match() {
        // match_regex accepts .php, but filter_regex also rejects if "admin" is present
        let filter = Filter::from_config(&Config {
            blacklist: vec![],
            match_regex: Some(r"\.php".to_string()),
            filter_regex: Some(r"admin".to_string()),
            ..Config::default()
        })
        .unwrap();
        let rec = make_record("https://example.com/admin.php");
        assert!(!filter.accept(&rec));
    }

    #[test]
    fn invalid_match_regex_returns_error() {
        let result = Filter::from_config(&Config {
            blacklist: vec![],
            match_regex: Some(r"[invalid".to_string()),
            ..Config::default()
        });
        assert!(result.is_err());
    }

    #[test]
    fn invalid_filter_regex_returns_error() {
        let result = Filter::from_config(&Config {
            blacklist: vec![],
            filter_regex: Some(r"[invalid".to_string()),
            ..Config::default()
        });
        assert!(result.is_err());
    }
}
