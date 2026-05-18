//! Diff mode — loads a previous run's output and filters out already-known URLs.
//!
//! Supports both plain-text (one URL per line, optionally prefixed by a score)
//! and JSON-lines (one `{"url": "...", ...}` object per line) input formats.

use std::collections::HashSet;
use std::io::{BufRead, BufReader};

use crate::dedup::normalize;
use crate::providers::UrlRecord;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Load the set of normalized URLs from a previous run file.
///
/// Format is auto-detected:
/// - If the first non-empty line parses as JSON, the file is treated as
///   JSON-lines and `.url` is extracted from each object.
/// - Otherwise the file is treated as plain text; each line is stripped of a
///   leading numeric score (e.g. `"8 https://..."`) if present.
///
/// Lines that cannot be parsed are silently skipped.
pub fn load_previous(path: &str) -> anyhow::Result<HashSet<String>> {
    let file = std::fs::File::open(path)
        .map_err(|e| anyhow::anyhow!("diff: cannot open previous file '{}': {}", path, e))?;

    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    // Peek at the first non-empty line to decide format.
    let mut peeked: Option<String> = None;
    let mut use_json = false;

    for line_result in lines.by_ref() {
        let line = line_result?;
        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        // Try JSON parse on the first real line.
        if serde_json::from_str::<serde_json::Value>(&trimmed).is_ok() {
            use_json = true;
        }
        peeked = Some(trimmed);
        break;
    }

    let mut set = HashSet::new();

    // Process the peeked line first, then the rest.
    let first_line = match peeked {
        Some(l) => l,
        None => return Ok(set), // empty file
    };

    if use_json {
        // JSON-lines mode.
        if let Some(url) = extract_json_url(&first_line) {
            set.insert(normalize(&url));
        }
        for line_result in lines {
            let line = line_result?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(url) = extract_json_url(trimmed) {
                set.insert(normalize(&url));
            }
        }
    } else {
        // Plain-text mode.
        if let Some(url) = extract_plain_url(&first_line) {
            set.insert(normalize(&url));
        }
        for line_result in lines {
            let line = line_result?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(url) = extract_plain_url(trimmed) {
                set.insert(normalize(url));
            }
        }
    }

    Ok(set)
}

/// Keep only records whose normalized URL is NOT in `previous`.
pub fn keep_new(records: Vec<UrlRecord>, previous: &HashSet<String>) -> Vec<UrlRecord> {
    records
        .into_iter()
        .filter(|r| !previous.contains(&normalize(&r.url)))
        .collect()
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Extract `.url` from a JSON-lines object. Returns `None` on parse failure.
fn extract_json_url(line: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    v.get("url")?.as_str().map(str::to_string)
}

/// Extract the URL from a plain-text line, stripping a leading score prefix
/// like `"8 https://..."` if present. Returns `None` for blank/garbage lines.
fn extract_plain_url(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Check for "<digits> <url>" pattern (score prefix from --ai-rank output).
    let mut parts = trimmed.splitn(2, ' ');
    if let (Some(first), Some(rest)) = (parts.next(), parts.next()) {
        if first.chars().all(|c| c.is_ascii_digit()) && !rest.is_empty() {
            return Some(rest);
        }
    }
    // No score prefix — treat the whole line as a URL.
    Some(trimmed)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // Helper: write content to a temp file and return the path string.
    fn temp_file(content: &str) -> (tempfile::NamedTempFile, String) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        let path = f.path().to_str().unwrap().to_string();
        (f, path)
    }

    fn make_record(url: &str) -> UrlRecord {
        UrlRecord {
            url: url.to_string(),
            source: "test".to_string(),
            status_code: None,
            mime_type: None,
            timestamp: None,
            score: None,
        }
    }

    // ---- load_previous: plain text ------------------------------------------

    #[test]
    fn load_plain_text_one_url_per_line() {
        let content = "https://example.com/a\nhttps://example.com/b\nhttps://example.com/c\n";
        let (_f, path) = temp_file(content);
        let set = load_previous(&path).unwrap();
        assert_eq!(set.len(), 3);
        assert!(set.contains(&normalize("https://example.com/a")));
        assert!(set.contains(&normalize("https://example.com/b")));
        assert!(set.contains(&normalize("https://example.com/c")));
    }

    #[test]
    fn load_plain_text_with_score_prefix() {
        let content = "8 https://example.com/admin\n3 https://example.com/about\n";
        let (_f, path) = temp_file(content);
        let set = load_previous(&path).unwrap();
        assert_eq!(set.len(), 2);
        assert!(set.contains(&normalize("https://example.com/admin")));
        assert!(set.contains(&normalize("https://example.com/about")));
    }

    #[test]
    fn load_plain_text_mixed_with_and_without_score() {
        let content =
            "https://example.com/no-score\n10 https://example.com/with-score\n";
        let (_f, path) = temp_file(content);
        let set = load_previous(&path).unwrap();
        assert_eq!(set.len(), 2);
        assert!(set.contains(&normalize("https://example.com/no-score")));
        assert!(set.contains(&normalize("https://example.com/with-score")));
    }

    // ---- load_previous: JSON-lines ------------------------------------------

    #[test]
    fn load_json_lines_extracts_url_field() {
        let content = concat!(
            r#"{"url":"https://example.com/x","source":"wayback"}"#,
            "\n",
            r#"{"url":"https://example.com/y","source":"otx"}"#,
            "\n"
        );
        let (_f, path) = temp_file(content);
        let set = load_previous(&path).unwrap();
        assert_eq!(set.len(), 2);
        assert!(set.contains(&normalize("https://example.com/x")));
        assert!(set.contains(&normalize("https://example.com/y")));
    }

    // ---- load_previous: mixed/garbage lines ---------------------------------

    #[test]
    fn load_skips_blank_lines() {
        let content = "https://example.com/a\n\n\nhttps://example.com/b\n";
        let (_f, path) = temp_file(content);
        let set = load_previous(&path).unwrap();
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn load_empty_file_returns_empty_set() {
        let (_f, path) = temp_file("");
        let set = load_previous(&path).unwrap();
        assert!(set.is_empty());
    }

    #[test]
    fn load_nonexistent_file_returns_error() {
        let result = load_previous("/this/file/does/not/exist.txt");
        assert!(result.is_err());
    }

    // ---- keep_new -----------------------------------------------------------

    #[test]
    fn keep_new_filters_known_urls() {
        let previous = {
            let mut s = HashSet::new();
            s.insert(normalize("https://example.com/a"));
            s
        };
        let records = vec![
            make_record("https://example.com/a"), // known
            make_record("https://example.com/b"), // new
        ];
        let kept = keep_new(records, &previous);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].url, "https://example.com/b");
    }

    #[test]
    fn keep_new_keeps_all_when_previous_is_empty() {
        let previous = HashSet::new();
        let records = vec![
            make_record("https://example.com/a"),
            make_record("https://example.com/b"),
        ];
        let kept = keep_new(records, &previous);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn keep_new_filters_normalized_variants() {
        // The previous set uses the normalized form; a URL with default port
        // should be recognized as the same.
        let previous = {
            let mut s = HashSet::new();
            s.insert(normalize("http://example.com/page"));
            s
        };
        let records = vec![
            make_record("http://example.com:80/page"), // normalizes to same
            make_record("http://example.com/other"),
        ];
        let kept = keep_new(records, &previous);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].url, "http://example.com/other");
    }

    #[test]
    fn keep_new_empty_records_returns_empty() {
        let previous = {
            let mut s = HashSet::new();
            s.insert(normalize("https://example.com/a"));
            s
        };
        let kept = keep_new(vec![], &previous);
        assert!(kept.is_empty());
    }
}
