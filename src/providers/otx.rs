//! AlienVault OTX provider — fetches URL data via the OTX Indicators API.
//!
//! Endpoint: `https://otx.alienvault.com/api/v1/indicators/hostname/{domain}/url_list`
//!
//! Pagination is 1-based: iterate page=1, 2, 3, ... until the returned
//! `url_list` has fewer than 500 entries (the configured page limit).

use anyhow::Context;
use async_trait::async_trait;
use std::time::Duration;
use tracing::{debug, warn};

use super::{Provider, UrlRecord};

const OTX_BASE: &str = "https://otx.alienvault.com/api/v1/indicators/hostname";
const PAGE_LIMIT: usize = 500;

pub struct OtxProvider {
    retries: u32,
    random_agent: bool,
    /// Optional OTX API key — empty string means unauthenticated.
    apikey: String,
}

impl OtxProvider {
    pub fn new(retries: u32, random_agent: bool, apikey: String) -> Self {
        Self {
            retries,
            random_agent,
            apikey,
        }
    }

    /// Build the endpoint URL for a given domain.
    fn endpoint(domain: &str) -> String {
        format!("{}/{}/url_list", OTX_BASE, domain)
    }

    /// Fetch one page from OTX with exponential-backoff retry.
    /// Handles HTTP 429 by sleeping the `Retry-After` header duration.
    async fn fetch_page(
        &self,
        client: &reqwest::Client,
        url: &str,
        page: u32,
    ) -> anyhow::Result<serde_json::Value> {
        let params: &[(&str, String)] = &[
            ("limit", PAGE_LIMIT.to_string()),
            ("page", page.to_string()),
        ];

        let mut attempt = 0u32;
        let mut rate_limit_count = 0u32;

        loop {
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

            if !self.apikey.is_empty() {
                request = request.header("X-OTX-API-KEY", self.apikey.as_str());
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
                                "OTX rate limited more than 10 times in a row; giving up"
                            ));
                        }
                        warn!(
                            page = page,
                            retry_after_secs = retry_after,
                            rate_limit_count = rate_limit_count,
                            "OTX: rate-limited (429); sleeping before retry"
                        );
                        tokio::time::sleep(Duration::from_secs(retry_after)).await;
                        continue;
                    }

                    if status.is_success() {
                        return resp
                            .json::<serde_json::Value>()
                            .await
                            .map_err(|e| anyhow::Error::from(e).context("JSON parse error"))
                            .context(format!("OTX fetch failed after retries: {url}?page={page}"));
                    }

                    // Non-200, non-429: count against retry budget.
                    attempt += 1;
                    if attempt > self.retries {
                        return Err(anyhow::anyhow!(
                            "OTX fetch failed after {} retries: HTTP {} — {url}?page={page}",
                            self.retries,
                            status
                        ));
                    }
                    let shift = (attempt - 1).min(62);
                    let backoff =
                        Duration::from_millis(100 * (1u64 << shift)).min(Duration::from_secs(30));
                    tokio::time::sleep(backoff).await;
                }
                Err(e) => {
                    // Network error: count against retry budget.
                    attempt += 1;
                    if attempt > self.retries {
                        return Err(anyhow::anyhow!(
                            "OTX fetch failed after {} retries: {} — {url}?page={page}",
                            self.retries,
                            e
                        ));
                    }
                    let shift = (attempt - 1).min(62);
                    let backoff =
                        Duration::from_millis(100 * (1u64 << shift)).min(Duration::from_secs(30));
                    tokio::time::sleep(backoff).await;
                }
            }
        }
    }

    /// Parse one OTX page response into `UrlRecord`s.
    /// Returns `(records, is_last_page)`.
    pub fn parse_page(value: &serde_json::Value) -> (Vec<UrlRecord>, bool) {
        let url_list = match value.get("url_list").and_then(|v| v.as_array()) {
            Some(arr) => arr,
            None => return (vec![], true),
        };

        let is_last_page = url_list.len() < PAGE_LIMIT;

        let mut records = Vec::with_capacity(url_list.len());

        for entry in url_list {
            let url = entry
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            if url.is_empty() {
                continue;
            }

            // Try `httpcode` first, then `result.urlworker.http_code`.
            let status_code = entry
                .get("httpcode")
                .and_then(|v| {
                    // OTX sometimes returns httpcode as a number, sometimes as string.
                    v.as_u64()
                        .map(|n| n as u16)
                        .or_else(|| v.as_str().and_then(|s| s.parse::<u16>().ok()))
                })
                .or_else(|| {
                    entry
                        .get("result")
                        .and_then(|r| r.get("urlworker"))
                        .and_then(|w| w.get("http_code"))
                        .and_then(|v| {
                            v.as_u64()
                                .map(|n| n as u16)
                                .or_else(|| v.as_str().and_then(|s| s.parse::<u16>().ok()))
                        })
                });

            let timestamp = entry
                .get("date")
                .and_then(|v| v.as_str())
                .map(str::to_string);

            records.push(UrlRecord {
                url,
                source: "otx".to_string(),
                status_code,
                mime_type: None,
                timestamp,
                score: None,
            });
        }

        (records, is_last_page)
    }
}

#[async_trait]
impl Provider for OtxProvider {
    async fn fetch_urls(
        &self,
        domain: &str,
        _include_subs: bool,
        client: &reqwest::Client,
    ) -> anyhow::Result<Vec<UrlRecord>> {
        if domain.contains('/') || domain.contains('\\') || domain.contains("..") || domain.contains(' ') {
            return Err(anyhow::anyhow!("Invalid domain: {}", domain));
        }

        // OTX hostname search returns results for the domain and its subdomains
        // naturally, so no URL change is needed for the --subs flag.
        let endpoint = Self::endpoint(domain);
        debug!(domain = %domain, endpoint = %endpoint, "OTX: starting fetch");

        let mut all_records: Vec<UrlRecord> = Vec::new();
        let mut page: u32 = 1;

        loop {
            let value = match self.fetch_page(client, &endpoint, page).await {
                Ok(v) => v,
                Err(e) => {
                    warn!(domain = %domain, page = page, error = %e, "OTX: page fetch failed; stopping pagination");
                    break;
                }
            };

            let (records, is_last) = Self::parse_page(&value);
            let count = records.len();
            all_records.extend(records);

            debug!(domain = %domain, page = page, count = count, is_last = is_last, "OTX: page fetched");

            if is_last {
                break;
            }

            page += 1;
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

    // --- parse_page ---

    #[test]
    fn parse_page_normal() {
        let val = json!({
            "url_list": [
                {
                    "url": "https://example.com/path",
                    "httpcode": 200,
                    "date": "2021-01-15T12:00:00"
                },
                {
                    "url": "https://example.com/other",
                    "httpcode": 301,
                    "date": "2021-02-01T00:00:00"
                }
            ],
            "count": 2
        });

        let (records, is_last) = OtxProvider::parse_page(&val);
        assert_eq!(records.len(), 2);
        assert!(is_last, "2 < 500 => last page");

        assert_eq!(records[0].url, "https://example.com/path");
        assert_eq!(records[0].status_code, Some(200));
        assert_eq!(records[0].timestamp.as_deref(), Some("2021-01-15T12:00:00"));
        assert_eq!(records[0].source, "otx");
        assert!(records[0].mime_type.is_none());

        assert_eq!(records[1].status_code, Some(301));
    }

    #[test]
    fn parse_page_httpcode_fallback_to_urlworker() {
        let val = json!({
            "url_list": [
                {
                    "url": "https://example.com/fallback",
                    "result": {
                        "urlworker": {
                            "http_code": 404
                        }
                    },
                    "date": "2021-03-01T00:00:00"
                }
            ]
        });

        let (records, _) = OtxProvider::parse_page(&val);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status_code, Some(404));
    }

    #[test]
    fn parse_page_httpcode_as_string() {
        let val = json!({
            "url_list": [
                {
                    "url": "https://example.com/strcode",
                    "httpcode": "200",
                    "date": "2021-04-01T00:00:00"
                }
            ]
        });

        let (records, _) = OtxProvider::parse_page(&val);
        assert_eq!(records[0].status_code, Some(200u16));
    }

    #[test]
    fn parse_page_skips_empty_url() {
        let val = json!({
            "url_list": [
                { "url": "", "httpcode": 200, "date": "2021-01-01T00:00:00" },
                { "url": "https://example.com/ok", "httpcode": 200, "date": "2021-01-01T00:00:00" }
            ]
        });

        let (records, _) = OtxProvider::parse_page(&val);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].url, "https://example.com/ok");
    }

    #[test]
    fn parse_page_empty_list() {
        let val = json!({ "url_list": [] });
        let (records, is_last) = OtxProvider::parse_page(&val);
        assert!(records.is_empty());
        assert!(is_last);
    }

    #[test]
    fn parse_page_missing_url_list() {
        let val = json!({ "count": 0 });
        let (records, is_last) = OtxProvider::parse_page(&val);
        assert!(records.is_empty());
        assert!(is_last);
    }

    #[test]
    fn pagination_stop_when_less_than_limit() {
        // Build a response with exactly 1 entry — must be flagged as last page.
        let mut url_list = Vec::new();
        url_list.push(json!({
            "url": "https://example.com/only",
            "httpcode": 200,
            "date": "2021-01-01T00:00:00"
        }));
        let val = json!({ "url_list": url_list });

        let (_, is_last) = OtxProvider::parse_page(&val);
        assert!(is_last, "1 < 500 must be last page");
    }

    #[test]
    fn pagination_continues_when_full_page() {
        // Build a response with exactly 500 entries — must NOT be last page.
        let url_list: Vec<serde_json::Value> = (0..500)
            .map(|i| {
                json!({
                    "url": format!("https://example.com/{}", i),
                    "httpcode": 200,
                    "date": "2021-01-01T00:00:00"
                })
            })
            .collect();
        let val = json!({ "url_list": url_list });

        let (records, is_last) = OtxProvider::parse_page(&val);
        assert_eq!(records.len(), 500);
        assert!(!is_last, "500 == PAGE_LIMIT must not be last page");
    }

    #[test]
    fn endpoint_format() {
        let url = OtxProvider::endpoint("example.com");
        assert_eq!(
            url,
            "https://otx.alienvault.com/api/v1/indicators/hostname/example.com/url_list"
        );
    }
}
