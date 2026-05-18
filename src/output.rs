//! Output module — centralises URL emission to stdout or a file.
//!
//! Two output formats are supported:
//! - Plain text (one URL per line)
//! - JSON-lines (one JSON object per line, with source attribution)

use std::fs::File;
use std::io::{self, BufWriter, Write};

use anyhow::Context;

use crate::providers::UrlRecord;
use crate::runner::ProviderStats;

// ---------------------------------------------------------------------------
// Public trait
// ---------------------------------------------------------------------------

/// Abstraction over where and how URL records are written.
pub trait OutputWriter: Send {
    fn write_record(&mut self, record: &UrlRecord) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
}

// ---------------------------------------------------------------------------
// Text writer
// ---------------------------------------------------------------------------

struct TextWriter<W: Write> {
    inner: W,
}

impl<W: Write + Send> OutputWriter for TextWriter<W> {
    fn write_record(&mut self, record: &UrlRecord) -> io::Result<()> {
        writeln!(self.inner, "{}", record.url)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

// ---------------------------------------------------------------------------
// JSON-lines writer
// ---------------------------------------------------------------------------

struct JsonWriter<W: Write> {
    inner: W,
}

impl<W: Write + Send> OutputWriter for JsonWriter<W> {
    fn write_record(&mut self, record: &UrlRecord) -> io::Result<()> {
        // serde_json honours #[serde(skip_serializing_if = "Option::is_none")]
        // so null fields are omitted automatically.
        let line = serde_json::to_string(record)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        writeln!(self.inner, "{}", line)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Build the appropriate [`OutputWriter`].
///
/// - `file_path = None`  → write to stdout (buffered)
/// - `file_path = Some`  → create (or truncate) the named file (buffered)
/// - `json = true`       → JSON-lines format
/// - `json = false`      → plain text (one URL per line)
pub fn make_writer(
    file_path: Option<&str>,
    json: bool,
) -> anyhow::Result<Box<dyn OutputWriter>> {
    match file_path {
        None => {
            let sink = BufWriter::new(io::stdout());
            if json {
                Ok(Box::new(JsonWriter { inner: sink }))
            } else {
                Ok(Box::new(TextWriter { inner: sink }))
            }
        }
        Some(path) => {
            let file =
                File::create(path).with_context(|| format!("failed to create output file: {path}"))?;
            let sink = BufWriter::new(file);
            if json {
                Ok(Box::new(JsonWriter { inner: sink }))
            } else {
                Ok(Box::new(TextWriter { inner: sink }))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Verbose stats
// ---------------------------------------------------------------------------

/// Log per-provider, dedup, and filter statistics via `tracing::info!`.
///
/// Provider entries are sorted alphabetically for deterministic output.
pub fn report_stats(
    stats: &ProviderStats,
    unique_count: usize,
    duplicate_count: usize,
    output_count: usize,
) {
    let mut sorted: Vec<_> = stats.by_provider.iter().collect();
    sorted.sort_by_key(|(k, _)| k.as_str());
    for (provider, count) in sorted {
        tracing::info!("provider {}: {} URLs", provider, count);
    }
    tracing::info!("raw: {} URLs fetched total", stats.total());
    tracing::info!(
        "dedup: {} unique URLs ({} duplicates removed)",
        unique_count,
        duplicate_count
    );
    tracing::info!(
        "filters: {} records after filter ({} dropped)",
        output_count,
        unique_count.saturating_sub(output_count)
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::UrlRecord;

    fn make_record_minimal() -> UrlRecord {
        UrlRecord {
            url: "https://example.com/page".to_string(),
            source: "wayback".to_string(),
            status_code: None,
            mime_type: None,
            timestamp: None,
        }
    }

    fn make_record_full() -> UrlRecord {
        UrlRecord {
            url: "https://example.com/full".to_string(),
            source: "commoncrawl".to_string(),
            status_code: Some(200),
            mime_type: Some("text/html".to_string()),
            timestamp: Some("20210115120000".to_string()),
        }
    }

    // ---- TextWriter --------------------------------------------------------

    #[test]
    fn text_writer_single_record() {
        let mut buf: Vec<u8> = Vec::new();
        let mut writer = TextWriter { inner: &mut buf };
        writer.write_record(&make_record_minimal()).unwrap();
        writer.flush().unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "https://example.com/page\n");
    }

    #[test]
    fn text_writer_multiple_records() {
        let mut buf: Vec<u8> = Vec::new();
        let mut writer = TextWriter { inner: &mut buf };
        writer.write_record(&make_record_minimal()).unwrap();
        writer.write_record(&make_record_full()).unwrap();
        writer.flush().unwrap();
        let out = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "https://example.com/page");
        assert_eq!(lines[1], "https://example.com/full");
    }

    // ---- JsonWriter --------------------------------------------------------

    #[test]
    fn json_writer_produces_valid_json_lines() {
        let mut buf: Vec<u8> = Vec::new();
        let mut writer = JsonWriter { inner: &mut buf };
        writer.write_record(&make_record_minimal()).unwrap();
        writer.write_record(&make_record_full()).unwrap();
        writer.flush().unwrap();

        let out = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);

        // Each line must be valid JSON
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line).expect("valid JSON line");
            assert!(v.is_object());
        }
    }

    #[test]
    fn json_writer_omits_null_optional_fields() {
        let mut buf: Vec<u8> = Vec::new();
        let mut writer = JsonWriter { inner: &mut buf };
        writer.write_record(&make_record_minimal()).unwrap();
        writer.flush().unwrap();

        let out = String::from_utf8(buf).unwrap();
        let line = out.lines().next().unwrap();
        let v: serde_json::Value = serde_json::from_str(line).unwrap();

        // Optional fields must not be present when None
        assert!(v.get("status_code").is_none(), "status_code should be absent");
        assert!(v.get("mime_type").is_none(), "mime_type should be absent");
        assert!(v.get("timestamp").is_none(), "timestamp should be absent");
        // Required fields must be present
        assert_eq!(v["url"], "https://example.com/page");
        assert_eq!(v["source"], "wayback");
    }

    #[test]
    fn json_writer_includes_optional_fields_when_present() {
        let mut buf: Vec<u8> = Vec::new();
        let mut writer = JsonWriter { inner: &mut buf };
        writer.write_record(&make_record_full()).unwrap();
        writer.flush().unwrap();

        let out = String::from_utf8(buf).unwrap();
        let line = out.lines().next().unwrap();
        let v: serde_json::Value = serde_json::from_str(line).unwrap();

        assert_eq!(v["url"], "https://example.com/full");
        assert_eq!(v["source"], "commoncrawl");
        assert_eq!(v["status_code"], 200);
        assert_eq!(v["mime_type"], "text/html");
        assert_eq!(v["timestamp"], "20210115120000");
    }

    // ---- make_writer -------------------------------------------------------

    fn make_test_record() -> UrlRecord {
        make_record_minimal()
    }

    #[test]
    fn make_writer_stdout_accepts_records() {
        let mut w = make_writer(None, false).unwrap();
        let rec = make_test_record();
        // Just verify no panic when writing to stdout-backed writer
        // (output goes to the test runner's stdout)
        let _ = w.write_record(&rec);
        let _ = w.flush();
    }

    #[test]
    fn make_writer_unwritable_path_returns_error() {
        // Use a path with an obviously-invalid component to force File::create failure
        let bad_path = "/this/path/does/not/exist/output.txt";
        let result = make_writer(Some(bad_path), false);
        assert!(result.is_err(), "expected error for unwritable path");
    }

    #[test]
    fn make_writer_some_path_creates_file() {
        use std::env;
        let tmp = env::temp_dir().join("lancer_gau_test_output.txt");
        let path_str = tmp.to_str().unwrap().to_string();

        let result = make_writer(Some(&path_str), false);
        assert!(result.is_ok(), "make_writer should succeed for a valid path");

        // File must have been created
        assert!(tmp.exists(), "output file should exist after make_writer");

        // Clean up
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn make_writer_some_path_creates_file_json() {
        use std::env;
        let tmp = env::temp_dir().join("lancer_gau_test_output_json.json");
        let path_str = tmp.to_str().unwrap().to_string();

        let result = make_writer(Some(&path_str), true);
        assert!(result.is_ok(), "make_writer should succeed for a valid path");
        assert!(tmp.exists(), "output file should exist after make_writer");

        let _ = std::fs::remove_file(&tmp);
    }
}
