//! URLScan.io provider — fetches URL data via the URLScan search API.
//!
//! Endpoint: `https://urlscan.io/api/v1/search/`
//!
//! Pagination uses a `search_after` cursor derived from the `sort` array of
//! the last result in each response.  Iteration stops when the response
//! contains fewer than 10 000 results.

use anyhow::Context;
use async_trait::async_trait;
use std::time::Duration;
use tracing::{debug, warn};

use super::{Provider, UrlRecord};

const URLSCAN_ENDPOINT: &str = "https://urlscan.io/api/v1/search/";
const PAGE_SIZE: usize = 10_000;

pub struct UrlScanProvider {
    retries: u32,
    random_agent: bool,
    /// Optional URLScan API key — empty string means unauthenticated.
    apikey: String,
}

impl UrlScanProvider {
    pub fn new(retries: u32, random_agent: bool, apikey: String) -> Self {
        Self {
            retries,
            random_agent,
            apikey,
        }
    }

    /// Build the `q` parameter value for a domain search.
    fn query_param(domain: &str, include_subs: bool) -> String {
        if include_subs {
            format!("domain:*.{}", domain)
        } else {
            format!("domain:{}", domain)
        }
    }

    /// Extract the `search_after` cursor from the last result in the array.
    ///
    /// URLScan uses the `sort` array from the last result as the cursor:
    /// each value is joined with a comma.
    pub fn extract_cursor(results: &[serde_json::Value]) -> Option<String> {
        let last = results.last()?;
        let sort = last.get("sort")?.as_array()?;
        if sort.is_empty() {
            return None;
        }
        let parts: Vec<String> = sort
            .iter()
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                other => other.to_string(),
            })
            .collect();
        Some(parts.join(","))
    }

    /// Fetch one search page.  `search_after` is `None` on the first request.
    async fn fetch_page(
        &self,
        client: &reqwest::Client,
        q: &str,
        search_after: Option<&str>,
    ) -> anyhow::Result<serde_json::Value> {
        let mut last_err: Option<anyhow::Error> = None;
        let mut rate_limit_count = 0u32;
        let mut attempt = 0u32;

        loop {
            if attempt > self.retries {
                break;
            }

            if attempt > 0 {
                let shift = (attempt - 1).min(62);
                let backoff = Duration::from_millis(100 * (1u64 << shift));
                let backoff = backoff.min(Duration::from_secs(30));
                tokio::time::sleep(backoff).await;
            }

            let mut params: Vec<(&str, String)> = vec![
                ("q", q.to_string()),
                ("size", PAGE_SIZE.to_string()),
            ];

            if let Some(cursor) = search_after {
                params.push(("search_after", cursor.to_string()));
            }

            let ua = if self.random_agent {
                let mut rng = rand::thread_rng();
                Some(super::random_ua(&mut rng))
            } else {
                None
            };
            let mut request = client.get(URLSCAN_ENDPOINT).query(&params);

            if let Some(agent) = ua {
                request = request.header(reqwest::header::USER_AGENT, agent);
            }

            if !self.apikey.is_empty() {
                request = request.header("API-Key", self.apikey.as_str());
            }

            match request.send().await {
                Ok(resp) => {
                    let status = resp.status();

                    // Handle rate-limiting: sleep Retry-After seconds then retry.
                    // Do NOT count this against `attempt`.
                    if status.as_u16() == 429 {
                        let retry_after = resp
                            .headers()
                            .get(reqwest::header::RETRY_AFTER)
                            .and_then(|v| v.to_str().ok())
                            .and_then(|s| s.parse::<u64>().ok())
                            .unwrap_or(60);
                        // Drain body to allow socket reuse on keep-alive connections.
                        let _ = resp.bytes().await;
                        rate_limit_count += 1;
                        if rate_limit_count > 10 {
                            return Err(anyhow::anyhow!(
                                "URLScan rate limited more than 10 times in a row; giving up"
                            ));
                        }
                        warn!(
                            retry_after_secs = retry_after,
                            rate_limit_count = rate_limit_count,
                            "URLScan: rate-limited (429); sleeping before retry"
                        );
                        tokio::time::sleep(Duration::from_secs(retry_after)).await;
                        // Do NOT increment attempt — retry without consuming retry budget.
                        continue;
                    }

                    if !status.is_success() {
                        last_err = Some(anyhow::anyhow!("HTTP {status}"));
                        attempt += 1;
                        continue;
                    }
                    match resp.json::<serde_json::Value>().await {
                        Ok(val) => return Ok(val),
                        Err(e) => {
                            last_err = Some(anyhow::Error::from(e).context("JSON parse error"));
                            attempt += 1;
                            continue;
                        }
                    }
                }
                Err(e) => {
                    last_err = Some(anyhow::Error::from(e).context("request error"));
                    attempt += 1;
                    continue;
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("unknown error")))
            .context(format!(
                "URLScan fetch failed after retries (search_after={:?})",
                search_after
            ))
    }

    /// Parse a URLScan search response into `UrlRecord`s.
    /// Returns `(records, Option<next_cursor>)`.
    /// `next_cursor` is `None` when this is the last page.
    /// Malformed entries are silently skipped.
    pub fn parse_response(value: &serde_json::Value) -> (Vec<UrlRecord>, Option<String>) {
        let results = match value.get("results").and_then(|v| v.as_array()) {
            Some(arr) => arr,
            None => return (vec![], None),
        };

        let is_last_page = results.len() < PAGE_SIZE;

        let mut records = Vec::with_capacity(results.len());

        for result in results {
            let page_obj = result.get("page");
            let task_obj = result.get("task");

            let url = page_obj
                .and_then(|p| p.get("url"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if url.is_empty() {
                continue;
            }

            let status_code = page_obj
                .and_then(|p| p.get("status"))
                .and_then(|v| {
                    v.as_str()
                        .and_then(|s| s.parse::<u16>().ok())
                        .or_else(|| v.as_u64().map(|n| n as u16))
                });

            let mime_type = page_obj
                .and_then(|p| p.get("mimeType"))
                .and_then(|v| v.as_str())
                .map(str::to_string);

            let timestamp = task_obj
                .and_then(|t| t.get("time"))
                .and_then(|v| v.as_str())
                .map(str::to_string);

            records.push(UrlRecord {
                url,
                source: "urlscan".to_string(),
                status_code,
                mime_type,
                timestamp,
                score: None,
            });
        }

        let cursor = if is_last_page {
            None
        } else {
            Self::extract_cursor(results)
        };

        (records, cursor)
    }
}

#[async_trait]
impl Provider for UrlScanProvider {
    async fn fetch_urls(
        &self,
        domain: &str,
        include_subs: bool,
        client: &reqwest::Client,
    ) -> anyhow::Result<Vec<UrlRecord>> {
        let q = Self::query_param(domain, include_subs);
        debug!(domain = %domain, q = %q, "URLScan: starting fetch");

        let mut all_records: Vec<UrlRecord> = Vec::new();
        let mut search_after: Option<String> = None;

        loop {
            let cursor_ref = search_after.as_deref();

            let value = match self.fetch_page(client, &q, cursor_ref).await {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        domain = %domain,
                        error = %e,
                        "URLScan: page fetch failed; stopping pagination"
                    );
                    break;
                }
            };

            let (records, next_cursor) = UrlScanProvider::parse_response(&value);
            let count = records.len();
            all_records.extend(records);

            debug!(
                domain = %domain,
                count = count,
                has_next = next_cursor.is_some(),
                "URLScan: page fetched"
            );

            match next_cursor {
                None => break,
                Some(cursor) => {
                    search_after = Some(cursor);
                }
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
    use serde_json::json;

    // --- parse_response ---

    #[test]
    fn parse_response_normal() {
        let val = json!({
            "results": [
                {
                    "_id": "abc123",
                    "sort": [1234567890, "abc123"],
                    "page": {
                        "url": "https://example.com/path",
                        "status": "200",
                        "mimeType": "text/html"
                    },
                    "task": {
                        "time": "2021-01-15T12:00:00.000Z"
                    }
                }
            ],
            "total": 1,
            "has_more": false
        });

        let (records, cursor) = UrlScanProvider::parse_response(&val);
        assert_eq!(records.len(), 1);
        assert!(cursor.is_none(), "1 < 10000 => no cursor");

        let r = &records[0];
        assert_eq!(r.url, "https://example.com/path");
        assert_eq!(r.status_code, Some(200));
        assert_eq!(r.mime_type.as_deref(), Some("text/html"));
        assert_eq!(r.timestamp.as_deref(), Some("2021-01-15T12:00:00.000Z"));
        assert_eq!(r.source, "urlscan");
    }

    #[test]
    fn parse_response_missing_results_key() {
        let val = json!({ "total": 0 });
        let (records, cursor) = UrlScanProvider::parse_response(&val);
        assert!(records.is_empty());
        assert!(cursor.is_none());
    }

    #[test]
    fn parse_response_empty_results() {
        let val = json!({ "results": [] });
        let (records, cursor) = UrlScanProvider::parse_response(&val);
        assert!(records.is_empty());
        assert!(cursor.is_none());
    }

    #[test]
    fn parse_response_skips_empty_url() {
        let val = json!({
            "results": [
                {
                    "_id": "x",
                    "sort": [1, "x"],
                    "page": { "url": "", "status": "200", "mimeType": "text/html" },
                    "task": { "time": "2021-01-01T00:00:00Z" }
                },
                {
                    "_id": "y",
                    "sort": [2, "y"],
                    "page": { "url": "https://example.com/ok", "status": "200", "mimeType": "text/html" },
                    "task": { "time": "2021-01-01T00:00:00Z" }
                }
            ]
        });

        let (records, _) = UrlScanProvider::parse_response(&val);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].url, "https://example.com/ok");
    }

    #[test]
    fn parse_response_status_as_number() {
        let val = json!({
            "results": [
                {
                    "_id": "n1",
                    "sort": [1, "n1"],
                    "page": { "url": "https://example.com/num", "status": 404, "mimeType": "text/plain" },
                    "task": { "time": "2021-01-01T00:00:00Z" }
                }
            ]
        });

        let (records, _) = UrlScanProvider::parse_response(&val);
        assert_eq!(records[0].status_code, Some(404));
    }

    // --- extract_cursor ---

    #[test]
    fn extract_cursor_normal() {
        let results = vec![
            json!({ "sort": [1234567890u64, "abc123"] }),
            json!({ "sort": [9876543210u64, "xyz999"] }),
        ];
        // Cursor should be from the LAST element.
        let cursor = UrlScanProvider::extract_cursor(&results).unwrap();
        assert_eq!(cursor, "9876543210,xyz999");
    }

    #[test]
    fn extract_cursor_single_element() {
        let results = vec![json!({ "sort": [42u64, "only"] })];
        let cursor = UrlScanProvider::extract_cursor(&results).unwrap();
        assert_eq!(cursor, "42,only");
    }

    #[test]
    fn extract_cursor_empty_results() {
        let results: Vec<serde_json::Value> = vec![];
        assert!(UrlScanProvider::extract_cursor(&results).is_none());
    }

    #[test]
    fn extract_cursor_no_sort_field() {
        let results = vec![json!({ "_id": "no-sort" })];
        assert!(UrlScanProvider::extract_cursor(&results).is_none());
    }

    #[test]
    fn extract_cursor_empty_sort_array() {
        let results = vec![json!({ "sort": [] })];
        assert!(UrlScanProvider::extract_cursor(&results).is_none());
    }

    // --- query_param ---

    #[test]
    fn query_param_no_subs() {
        assert_eq!(
            UrlScanProvider::query_param("example.com", false),
            "domain:example.com"
        );
    }

    #[test]
    fn query_param_with_subs() {
        assert_eq!(
            UrlScanProvider::query_param("example.com", true),
            "domain:*.example.com"
        );
    }

    // --- pagination stops at last page ---

    #[test]
    fn pagination_stop_when_less_than_page_size() {
        // Build a response with 1 result — must signal last page (no cursor).
        let val = json!({
            "results": [
                {
                    "_id": "single",
                    "sort": [1u64, "single"],
                    "page": { "url": "https://example.com/only", "status": "200", "mimeType": "text/html" },
                    "task": { "time": "2021-01-01T00:00:00Z" }
                }
            ]
        });

        let (records, cursor) = UrlScanProvider::parse_response(&val);
        assert_eq!(records.len(), 1);
        assert!(cursor.is_none(), "1 < 10000 means last page, cursor must be None");
    }
}
