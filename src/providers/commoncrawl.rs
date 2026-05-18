//! Common Crawl CDX provider with dynamic index discovery.
//!
//! The latest index name is discovered at runtime from
//! `https://index.commoncrawl.org/collinfo.json` so this provider never
//! needs updating when a new crawl is published.

use anyhow::Context;
use async_trait::async_trait;
use std::time::Duration;
use tracing::{debug, warn};

use super::{Provider, UrlRecord};

const COLLINFO_URL: &str = "https://index.commoncrawl.org/collinfo.json";
const CC_INDEX_BASE: &str = "https://index.commoncrawl.org";

/// Entry in the collinfo.json array.
#[derive(Debug, serde::Deserialize)]
struct CollInfoEntry {
    id: String,
}

pub struct CommonCrawlProvider {
    retries: u32,
    random_agent: bool,
}

impl CommonCrawlProvider {
    pub fn new(retries: u32, random_agent: bool) -> Self {
        Self {
            retries,
            random_agent,
        }
    }

    /// Perform one HTTP GET with exponential-backoff retries.
    /// Returns the raw response text.
    async fn fetch_text(
        &self,
        client: &reqwest::Client,
        url: &str,
        params: &[(&str, String)],
    ) -> anyhow::Result<String> {
        let mut last_err: Option<anyhow::Error> = None;

        for attempt in 0..=self.retries {
            if attempt > 0 {
                let shift = (attempt - 1).min(62);
                let backoff = Duration::from_millis(100 * (1u64 << shift));
                let backoff = backoff.min(Duration::from_secs(30));
                tokio::time::sleep(backoff).await;
            }

            let ua = if self.random_agent {
                let mut rng = rand::thread_rng();
                Some(super::random_ua(&mut rng))
            } else {
                None
            };
            let mut request = client.get(url).query(params);

            if let Some(agent) = ua {
                request = request.header(reqwest::header::USER_AGENT, agent);
            }

            match request.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if !status.is_success() {
                        last_err = Some(anyhow::anyhow!("HTTP {status}"));
                        continue;
                    }
                    match resp.text().await {
                        Ok(text) => return Ok(text),
                        Err(e) => {
                            last_err =
                                Some(anyhow::Error::from(e).context("response text error"));
                            continue;
                        }
                    }
                }
                Err(e) => {
                    last_err = Some(anyhow::Error::from(e).context("request error"));
                    continue;
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("unknown error")))
            .context(format!("fetch failed after retries: {url}"))
    }

    /// Discover the latest Common Crawl index ID from collinfo.json.
    ///
    /// The JSON is an array of objects each containing at minimum an `"id"`
    /// field like `"CC-MAIN-2026-04"`.  We return the last (most recent) id.
    pub async fn latest_index(
        &self,
        client: &reqwest::Client,
    ) -> anyhow::Result<String> {
        let text = self
            .fetch_text(client, COLLINFO_URL, &[])
            .await
            .context("fetching collinfo.json")?;

        let entries: Vec<CollInfoEntry> =
            serde_json::from_str(&text).context("parsing collinfo.json")?;

        // The list is ordered newest-first in practice, but to be safe we
        // take the entry with the lexicographically greatest id — the
        // "CC-MAIN-YYYY-WW" scheme sorts correctly this way.
        entries
            .into_iter()
            .map(|e| e.id)
            .max()
            .ok_or_else(|| anyhow::anyhow!("collinfo.json returned an empty list"))
    }

    /// Build the CDX index URL for a given index id.
    fn index_url(index_id: &str) -> String {
        format!("{}/{}-index", CC_INDEX_BASE, index_id)
    }

    /// Parse a single NDJSON page returned by the Common Crawl CDX API.
    ///
    /// Each line is a JSON object with at least `url`, `status`, `mime`, and
    /// `timestamp` fields.
    pub fn parse_ndjson_page(&self, text: &str) -> Vec<UrlRecord> {
        let mut records = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<serde_json::Value>(line) {
                Ok(obj) => {
                    let url = obj
                        .get("url")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if url.is_empty() {
                        continue;
                    }
                    let status_code = obj
                        .get("status")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<u16>().ok());
                    let mime_type = obj
                        .get("mime")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    let timestamp = obj
                        .get("timestamp")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);

                    records.push(UrlRecord {
                        url,
                        source: "commoncrawl".to_string(),
                        status_code,
                        mime_type,
                        timestamp,
                    });
                }
                Err(e) => {
                    warn!(line = %line, error = %e, "Common Crawl: failed to parse NDJSON line");
                }
            }
        }
        records
    }

    /// Fetch a single CDX page by number (0-based).
    /// Returns `None` if the server returns a non-200 response (so the caller
    /// can skip that page without aborting the whole provider).
    async fn fetch_page(
        &self,
        client: &reqwest::Client,
        index_url: &str,
        url_param: &str,
        page: u64,
    ) -> Option<String> {
        let params: Vec<(&str, String)> = vec![
            ("url", url_param.to_string()),
            ("output", "json".to_string()),
            ("page", page.to_string()),
            ("pageSize", "15000".to_string()),
        ];

        // We intentionally do NOT use the retry helper here so we can
        // distinguish a non-200 (skip page) from a network error (retry).
        let mut last_err: Option<String> = None;

        for attempt in 0..=self.retries {
            if attempt > 0 {
                let shift = (attempt - 1).min(62);
                let backoff = Duration::from_millis(100 * (1u64 << shift));
                let backoff = backoff.min(Duration::from_secs(30));
                tokio::time::sleep(backoff).await;
            }

            let ua = if self.random_agent {
                let mut rng = rand::thread_rng();
                Some(super::random_ua(&mut rng))
            } else {
                None
            };
            let mut request = client.get(index_url).query(&params);
            if let Some(agent) = ua {
                request = request.header(reqwest::header::USER_AGENT, agent);
            }

            match request.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if !status.is_success() {
                        warn!(
                            page = page,
                            status = %status,
                            "Common Crawl: non-200 response for page; skipping"
                        );
                        return None;
                    }
                    match resp.text().await {
                        Ok(text) => return Some(text),
                        Err(e) => {
                            last_err = Some(format!("response text error: {e}"));
                            continue;
                        }
                    }
                }
                Err(e) => {
                    last_err = Some(format!("request error: {e}"));
                    continue;
                }
            }
        }

        warn!(
            page = page,
            error = last_err.as_deref().unwrap_or("unknown"),
            "Common Crawl: page fetch failed after retries; skipping"
        );
        None
    }

    /// Fetch the total number of pages for a query.
    async fn fetch_num_pages(
        &self,
        client: &reqwest::Client,
        index_url: &str,
        url_param: &str,
    ) -> anyhow::Result<u64> {
        let params: Vec<(&str, String)> = vec![
            ("url", url_param.to_string()),
            ("output", "json".to_string()),
            ("showNumPages", "true".to_string()),
            ("pageSize", "15000".to_string()),
        ];

        let text = self
            .fetch_text(client, index_url, &params)
            .await
            .context("fetching Common Crawl page count")?;

        // The response is a JSON object like `{"pages": 5, "pageSize": 15000}`
        let value: serde_json::Value =
            serde_json::from_str(text.trim()).context("parsing Common Crawl page-count JSON")?;

        // Some versions return `{"pages": N}`, others a top-level integer.
        if let Some(n) = value.get("pages").and_then(|v| v.as_u64()) {
            return Ok(n);
        }
        if let Some(n) = value.as_u64() {
            return Ok(n);
        }
        anyhow::bail!(
            "Unexpected Common Crawl page-count response: {}",
            text.trim()
        )
    }
}

#[async_trait]
impl Provider for CommonCrawlProvider {
    async fn fetch_urls(
        &self,
        domain: &str,
        _include_subs: bool,
        client: &reqwest::Client,
    ) -> anyhow::Result<Vec<UrlRecord>> {
        // Common Crawl always uses *.domain — subdomains are implicit.
        let url_param = format!("*.{}", domain);

        if !_include_subs {
            debug!(
                "Common Crawl always queries *.{domain} — subdomains included regardless of --subs flag"
            );
        }

        // 1. Discover latest index.
        let index_id = self
            .latest_index(client)
            .await
            .context("Common Crawl index discovery")?;

        let idx_url = Self::index_url(&index_id);

        // 2. Find out how many pages there are.
        let num_pages = match self
            .fetch_num_pages(client, &idx_url, &url_param)
            .await
        {
            Ok(n) => n,
            Err(e) => {
                warn!(error = %e, "Common Crawl: could not determine page count; skipping provider");
                return Ok(Vec::new());
            }
        };

        // 3. Nothing indexed for this domain — return early.
        if num_pages == 0 {
            return Ok(Vec::new());
        }

        // 4. Fetch all pages (0-based).
        let mut all_records: Vec<UrlRecord> = Vec::new();

        for page in 0..num_pages {
            if let Some(text) = self.fetch_page(client, &idx_url, &url_param, page).await {
                all_records.extend(self.parse_ndjson_page(&text));
            }
        }

        Ok(all_records)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_provider() -> CommonCrawlProvider {
        CommonCrawlProvider::new(3, false)
    }

    // --- collinfo.json parsing ---

    #[test]
    fn latest_index_picks_lexicographically_greatest() {
        // Simulate what latest_index() does without network I/O.
        let json = r#"[
            {"id": "CC-MAIN-2024-10", "name": "March 2024 Index"},
            {"id": "CC-MAIN-2025-18", "name": "April 2025 Index"},
            {"id": "CC-MAIN-2026-04", "name": "January 2026 Index"}
        ]"#;
        let entries: Vec<CollInfoEntry> = serde_json::from_str(json).unwrap();
        let latest = entries.into_iter().map(|e| e.id).max().unwrap();
        assert_eq!(latest, "CC-MAIN-2026-04");
    }

    #[test]
    fn latest_index_single_entry() {
        let json = r#"[{"id": "CC-MAIN-2023-06", "name": "February 2023 Index"}]"#;
        let entries: Vec<CollInfoEntry> = serde_json::from_str(json).unwrap();
        let latest = entries.into_iter().map(|e| e.id).max().unwrap();
        assert_eq!(latest, "CC-MAIN-2023-06");
    }

    #[test]
    fn latest_index_empty_returns_none() {
        let entries: Vec<CollInfoEntry> = vec![];
        let latest = entries.into_iter().map(|e| e.id).max();
        assert!(latest.is_none());
    }

    // --- NDJSON page parsing ---

    #[test]
    fn parse_ndjson_page_normal() {
        let provider = make_provider();
        let ndjson = r#"{"url":"https://example.com/page","status":"200","mime":"text/html","timestamp":"20240101120000"}
{"url":"https://example.com/other","status":"301","mime":"text/html","timestamp":"20240201000000"}"#;
        let records = provider.parse_ndjson_page(ndjson);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].url, "https://example.com/page");
        assert_eq!(records[0].status_code, Some(200));
        assert_eq!(records[0].mime_type.as_deref(), Some("text/html"));
        assert_eq!(records[0].timestamp.as_deref(), Some("20240101120000"));
        assert_eq!(records[0].source, "commoncrawl");
        assert_eq!(records[1].status_code, Some(301));
    }

    #[test]
    fn parse_ndjson_page_skips_empty_url() {
        let provider = make_provider();
        let ndjson = r#"{"url":"","status":"200","mime":"text/html","timestamp":"20240101"}
{"url":"https://example.com/ok","status":"200","mime":"text/html","timestamp":"20240101"}"#;
        let records = provider.parse_ndjson_page(ndjson);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].url, "https://example.com/ok");
    }

    #[test]
    fn parse_ndjson_page_skips_invalid_json_line() {
        let provider = make_provider();
        let ndjson = r#"not-valid-json
{"url":"https://example.com/ok","status":"200","mime":"text/html","timestamp":"20240101"}"#;
        let records = provider.parse_ndjson_page(ndjson);
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn parse_ndjson_page_empty_input() {
        let provider = make_provider();
        let records = provider.parse_ndjson_page("");
        assert!(records.is_empty());
    }

    #[test]
    fn parse_ndjson_page_missing_optional_fields() {
        let provider = make_provider();
        // Only url is mandatory; status/mime/timestamp may be absent.
        let ndjson = r#"{"url":"https://example.com/nofields"}"#;
        let records = provider.parse_ndjson_page(ndjson);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].url, "https://example.com/nofields");
        assert!(records[0].status_code.is_none());
        assert!(records[0].mime_type.is_none());
        assert!(records[0].timestamp.is_none());
    }

    #[test]
    fn index_url_format() {
        let url = CommonCrawlProvider::index_url("CC-MAIN-2026-04");
        assert_eq!(url, "https://index.commoncrawl.org/CC-MAIN-2026-04-index");
    }
}
