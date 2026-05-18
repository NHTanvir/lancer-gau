//! Wayback CDX provider with full resumeKey pagination.

use anyhow::Context;
use async_trait::async_trait;
use rand::seq::SliceRandom;
use std::time::Duration;

use super::{Provider, UrlRecord, USER_AGENTS};

const CDX_ENDPOINT: &str = "https://web.archive.org/cdx/search/cdx";

pub struct WaybackProvider {
    retries: u32,
    random_agent: bool,
    from: Option<String>,
    to: Option<String>,
}

impl WaybackProvider {
    pub fn new(
        retries: u32,
        random_agent: bool,
        from: Option<String>,
        to: Option<String>,
    ) -> Self {
        Self {
            retries,
            random_agent,
            from,
            to,
        }
    }

    /// Return a random user-agent string from the hardcoded list.
    fn random_ua(&self) -> &'static str {
        let mut rng = rand::thread_rng();
        USER_AGENTS.choose(&mut rng).copied().unwrap_or(USER_AGENTS[0])
    }

    /// Perform one CDX request with exponential-backoff retries.
    /// Returns the raw JSON value.
    async fn fetch_page(
        &self,
        client: &reqwest::Client,
        url_param: &str,
        resume_key: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
    ) -> anyhow::Result<serde_json::Value> {
        let mut params: Vec<(&str, String)> = vec![
            ("url", url_param.to_string()),
            ("output", "json".to_string()),
            ("fl", "original,statuscode,timestamp,mimetype".to_string()),
            ("collapse", "urlkey".to_string()),
            ("showResumeKey", "true".to_string()),
            ("limit", "100000".to_string()),
        ];

        if let Some(key) = resume_key {
            params.push(("resumeKey", key.to_string()));
        }
        if let Some(f) = from {
            params.push(("from", f.to_string()));
        }
        if let Some(t) = to {
            params.push(("to", t.to_string()));
        }

        let mut last_err: Option<anyhow::Error> = None;

        for attempt in 0..=self.retries {
            if attempt > 0 {
                let shift = (attempt - 1).min(62);
                let backoff = Duration::from_millis(100 * (1u64 << shift));
                let backoff = backoff.min(Duration::from_secs(30));
                tokio::time::sleep(backoff).await;
            }

            let mut request = client.get(CDX_ENDPOINT).query(&params);

            if self.random_agent {
                request = request.header(reqwest::header::USER_AGENT, self.random_ua());
            }

            match request.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if !status.is_success() {
                        last_err = Some(anyhow::anyhow!("HTTP {status}"));
                        continue;
                    }
                    match resp.json::<serde_json::Value>().await {
                        Ok(val) => return Ok(val),
                        Err(e) => {
                            last_err = Some(anyhow::Error::from(e).context("JSON parse error"));
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
            .context("Wayback CDX fetch failed after retries")
    }

    /// Parse a CDX JSON response array into `UrlRecord`s.
    /// The first element of the array is the field-header row; skip it.
    /// The last element *may* be a 1-element array containing the resumeKey string.
    ///
    /// Returns `(records, Option<resume_key_string>)`.
    fn parse_response(
        &self,
        value: serde_json::Value,
    ) -> anyhow::Result<(Vec<UrlRecord>, Option<String>)> {
        let arr = match value {
            serde_json::Value::Array(a) => a,
            // Empty body / null means no results
            serde_json::Value::Null => return Ok((vec![], None)),
            other => anyhow::bail!("Unexpected CDX response type: {other:?}"),
        };

        if arr.is_empty() {
            return Ok((vec![], None));
        }

        let mut resume_key: Option<String> = None;
        let mut rows = arr;

        // Check the last element: if it's a single-element array containing a
        // string (not a URL-shaped object), it's the resumeKey sentinel.
        if let Some(last) = rows.last() {
            if let serde_json::Value::Array(inner) = last {
                if inner.len() == 1 {
                    if let Some(serde_json::Value::String(key)) = inner.first() {
                        resume_key = Some(key.clone());
                        rows.pop(); // remove the sentinel from data rows
                    }
                }
            }
        }

        // Skip the header row (index 0).
        // Header: ["original","statuscode","timestamp","mimetype"]
        let data_rows = rows.into_iter().skip(1);

        // fl=original,statuscode,timestamp,mimetype  → indices 0,1,2,3
        let mut records = Vec::new();
        for row in data_rows {
            if let serde_json::Value::Array(fields) = row {
                let url = fields
                    .first()
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                if url.is_empty() {
                    continue;
                }

                let status_code = fields
                    .get(1)
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<u16>().ok());

                let timestamp = fields.get(2).and_then(|v| v.as_str()).map(str::to_string);

                let mime_type = fields.get(3).and_then(|v| v.as_str()).map(str::to_string);

                records.push(UrlRecord {
                    url,
                    source: "wayback".to_string(),
                    status_code,
                    mime_type,
                    timestamp,
                });
            }
        }

        Ok((records, resume_key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_provider() -> WaybackProvider {
        WaybackProvider::new(3, false, None, None)
    }

    #[test]
    fn parse_response_normal() {
        let provider = make_provider();
        let value = json!([
            ["original", "statuscode", "timestamp", "mimetype"],
            ["https://example.com/page", "200", "20230101120000", "text/html"]
        ]);
        let (records, resume_key) = provider.parse_response(value).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].url, "https://example.com/page");
        assert_eq!(records[0].status_code, Some(200));
        assert_eq!(records[0].mime_type.as_deref(), Some("text/html"));
        assert_eq!(records[0].timestamp.as_deref(), Some("20230101120000"));
        assert!(resume_key.is_none());
    }

    #[test]
    fn parse_response_with_resume_key() {
        let provider = make_provider();
        let value = json!([
            ["original", "statuscode", "timestamp", "mimetype"],
            ["https://example.com/page", "200", "20230101120000", "text/html"],
            ["abc123resumekey"]
        ]);
        let (records, resume_key) = provider.parse_response(value).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(resume_key.as_deref(), Some("abc123resumekey"));
    }

    #[test]
    fn parse_response_empty_array() {
        let provider = make_provider();
        let (records, resume_key) = provider.parse_response(json!([])).unwrap();
        assert!(records.is_empty());
        assert!(resume_key.is_none());
    }

    #[test]
    fn parse_response_null() {
        let provider = make_provider();
        let (records, resume_key) = provider.parse_response(serde_json::Value::Null).unwrap();
        assert!(records.is_empty());
        assert!(resume_key.is_none());
    }
}

#[async_trait]
impl Provider for WaybackProvider {
    fn name(&self) -> &'static str {
        "wayback"
    }

    async fn fetch_urls(
        &self,
        domain: &str,
        include_subs: bool,
        client: &reqwest::Client,
    ) -> anyhow::Result<Vec<UrlRecord>> {
        let url_param = if include_subs {
            format!("*.{}/*", domain)
        } else {
            format!("{}/*", domain)
        };

        let from = self.from.as_deref();
        let to = self.to.as_deref();

        let mut all_records: Vec<UrlRecord> = Vec::new();
        let mut resume_key: Option<String> = None;
        // Tracks whether we've already retried an empty resumeKey once.
        let mut empty_key_retried = false;

        loop {
            let key_ref = resume_key.as_deref();

            let value = self
                .fetch_page(client, &url_param, key_ref, from, to)
                .await
                .context("Wayback CDX page fetch")?;

            let (records, new_key) = self.parse_response(value)?;
            all_records.extend(records);

            match new_key {
                // No resume key → we're done.
                None => break,

                Some(ref key) if !key.is_empty() => {
                    // Normal pagination: set key and fetch next page.
                    empty_key_retried = false;
                    resume_key = new_key;
                }

                Some(_) => {
                    // Empty resume key — edge case.
                    if empty_key_retried {
                        // Second empty key in a row → declare done.
                        break;
                    }
                    // Retry once without a resumeKey.
                    empty_key_retried = true;
                    resume_key = None;
                }
            }
        }

        Ok(all_records)
    }
}
