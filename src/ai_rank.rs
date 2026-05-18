//! AI ranking module — scores URLs by bug-bounty exploit potential
//! using the Anthropic Claude API.
//!
//! Batches URLs in groups of 100, sends a prompt to Claude, parses the
//! returned JSON array of integers, and sorts results highest-score-first.

use std::time::Duration;

use anyhow::Context;
use tracing::warn;

use crate::providers::UrlRecord;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A `UrlRecord` wrapped with an AI-assigned exploit-potential score (1-10).
pub struct ScoredRecord {
    pub record: UrlRecord,
    pub score: u8,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MODEL: &str = "claude-haiku-4-5-20251001";
const MAX_TOKENS: u32 = 4096;
const BATCH_SIZE: usize = 100;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Rank a list of URL records by exploit potential using the Anthropic API.
///
/// URLs are processed in batches of 100. If a batch fails, a warning is
/// logged and all records in that batch receive a score of 0.
///
/// Returns the records sorted by score descending (highest first).
///
/// # Errors
///
/// Returns an error only if `ANTHROPIC_API_KEY` is not set. All API
/// communication failures are handled per-batch with score-0 fallback.
pub async fn rank_urls(
    records: Vec<UrlRecord>,
    client: &reqwest::Client,
) -> anyhow::Result<Vec<ScoredRecord>> {
    let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
        anyhow::anyhow!("--ai-rank requires ANTHROPIC_API_KEY environment variable")
    })?;

    let mut scored: Vec<ScoredRecord> = Vec::with_capacity(records.len());

    for (batch_idx, chunk) in records.chunks(BATCH_SIZE).enumerate() {
        let scores = rank_batch(client, &api_key, chunk, batch_idx).await;
        for (i, record) in chunk.iter().enumerate() {
            let score = *scores.get(i).unwrap_or(&0);
            scored.push(ScoredRecord {
                record: record.clone(),
                score,
            });
        }
    }

    // Sort highest score first.
    scored.sort_by(|a, b| b.score.cmp(&a.score));

    Ok(scored)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Rank a single batch of URLs. Returns a Vec of scores aligned to `batch`.
/// On any error, logs a warning and returns `vec![0; batch.len()]`.
async fn rank_batch(
    client: &reqwest::Client,
    api_key: &str,
    batch: &[UrlRecord],
    batch_idx: usize,
) -> Vec<u8> {
    let prompt = build_prompt(batch);

    match call_anthropic(client, api_key, &prompt).await {
        Ok(scores) => {
            if scores.len() != batch.len() {
                warn!(
                    batch = batch_idx,
                    expected = batch.len(),
                    got = scores.len(),
                    "ai_rank: score count mismatch — truncating/padding with 0"
                );
                let mut padded = scores;
                padded.resize(batch.len(), 0);
                padded
            } else {
                scores
            }
        }
        Err(e) => {
            warn!(batch = batch_idx, error = %e, "ai_rank: batch failed — assigning score 0");
            vec![0; batch.len()]
        }
    }
}

/// Build the ranking prompt for a batch of URLs.
fn build_prompt(batch: &[UrlRecord]) -> String {
    let mut prompt = String::from(
        "Rank these URLs by bug bounty exploit potential (1-10). Score factors:\n\
        - 10: admin/debug/internal paths, file upload endpoints, deprecated API versions\n\
        - 7-9: auth flows, ID-based params (id=, user=, account=), redirect params\n\
        - 4-6: standard app endpoints with params\n\
        - 1-3: static assets, marketing pages\n\
        \n\
        Return ONLY a JSON array of integers (one per URL, in order). No prose.\n\
        \n\
        URLs:\n",
    );

    for (i, record) in batch.iter().enumerate() {
        prompt.push_str(&format!("{}. {}\n", i + 1, record.url));
    }

    prompt
}

/// Call the Anthropic Messages API and parse the returned JSON array of integers.
///
/// Handles 429 Retry-After with a single retry.
async fn call_anthropic(
    client: &reqwest::Client,
    api_key: &str,
    prompt: &str,
) -> anyhow::Result<Vec<u8>> {
    let body = serde_json::json!({
        "model": MODEL,
        "max_tokens": MAX_TOKENS,
        "messages": [{"role": "user", "content": prompt}]
    });

    let response = send_with_retry(client, api_key, &body).await?;

    // Parse the response envelope.
    let envelope: serde_json::Value =
        response.json().await.context("failed to parse Anthropic response JSON")?;

    let text = envelope
        .get("content")
        .and_then(|c| c.get(0))
        .and_then(|first| first.get("text"))
        .and_then(|t| t.as_str())
        .context("Anthropic response missing content[0].text")?;

    parse_score_array(text)
}

/// Send the request, respecting a single 429 Retry-After.
async fn send_with_retry(
    client: &reqwest::Client,
    api_key: &str,
    body: &serde_json::Value,
) -> anyhow::Result<reqwest::Response> {
    let resp = do_request(client, api_key, body).await?;

    if resp.status().as_u16() == 429 {
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(5);

        warn!(retry_after, "ai_rank: 429 from Anthropic — waiting before retry");
        tokio::time::sleep(Duration::from_secs(retry_after)).await;

        let retry_resp = do_request(client, api_key, body).await?;
        return check_success(retry_resp).await;
    }

    check_success(resp).await
}

async fn do_request(
    client: &reqwest::Client,
    api_key: &str,
    body: &serde_json::Value,
) -> anyhow::Result<reqwest::Response> {
    let resp = client
        .post(ANTHROPIC_API_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("content-type", "application/json")
        .json(body)
        .send()
        .await
        .context("failed to send request to Anthropic API")?;
    Ok(resp)
}

/// Return `Ok(resp)` if the status is successful, otherwise surface the error.
async fn check_success(resp: reqwest::Response) -> anyhow::Result<reqwest::Response> {
    if !resp.status().is_success() {
        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Anthropic API error {}: {}", status, body_text);
    }
    Ok(resp)
}

/// Extract the first balanced `[...]` substring from `text` using bracket-depth
/// counting. This avoids the ambiguity when there are multiple `]` characters in
/// the surrounding prose.
fn extract_array_str(text: &str) -> Option<&str> {
    let bytes = text.as_bytes();
    let start = text.find('[')?;
    let mut depth = 0i32;
    for (i, &b) in bytes[start..].iter().enumerate() {
        match b {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=start + i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse a JSON array of integers from the model's text response.
/// Accepts both a bare array `[8, 6, 3]` and text that contains one.
fn parse_score_array(text: &str) -> anyhow::Result<Vec<u8>> {
    let trimmed = text.trim();

    let array_str =
        extract_array_str(trimmed).context("no JSON array found in response")?;

    let values: Vec<serde_json::Value> =
        serde_json::from_str(array_str).context("failed to parse score array as JSON")?;

    values
        .into_iter()
        .map(|v| {
            v.as_i64()
                .context("score is not an integer")
                .map(|n| n.clamp(0, 10) as u8)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::UrlRecord;

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

    // ---- build_prompt -------------------------------------------------------

    #[test]
    fn prompt_contains_all_urls_numbered() {
        let records = vec![
            make_record("https://example.com/admin"),
            make_record("https://example.com/login"),
            make_record("https://example.com/about"),
        ];
        let prompt = build_prompt(&records);

        assert!(prompt.contains("1. https://example.com/admin"), "missing url 1");
        assert!(prompt.contains("2. https://example.com/login"), "missing url 2");
        assert!(prompt.contains("3. https://example.com/about"), "missing url 3");
    }

    #[test]
    fn prompt_contains_scoring_instructions() {
        let records = vec![make_record("https://example.com/test")];
        let prompt = build_prompt(&records);

        assert!(prompt.contains("bug bounty exploit potential"));
        assert!(prompt.contains("JSON array of integers"));
    }

    #[test]
    fn prompt_numbering_is_one_based() {
        let records = vec![
            make_record("https://a.com/1"),
            make_record("https://a.com/2"),
        ];
        let prompt = build_prompt(&records);
        assert!(prompt.contains("1. https://a.com/1"));
        assert!(prompt.contains("2. https://a.com/2"));
        // Must NOT contain "0." (zero-based numbering would be wrong)
        assert!(!prompt.contains("0. "));
    }

    // ---- parse_score_array --------------------------------------------------

    #[test]
    fn parse_bare_array() {
        let scores = parse_score_array("[8, 6, 3]").unwrap();
        assert_eq!(scores, vec![8, 6, 3]);
    }

    #[test]
    fn parse_array_with_surrounding_whitespace() {
        let scores = parse_score_array("  [10, 1, 5]  ").unwrap();
        assert_eq!(scores, vec![10, 1, 5]);
    }

    #[test]
    fn parse_array_embedded_in_prose() {
        // Model might return extra text despite instructions.
        let text = "Here are the scores: [7, 2, 9] as requested.";
        let scores = parse_score_array(text).unwrap();
        assert_eq!(scores, vec![7, 2, 9]);
    }

    #[test]
    fn parse_single_element_array() {
        let scores = parse_score_array("[5]").unwrap();
        assert_eq!(scores, vec![5]);
    }

    #[test]
    fn parse_empty_array() {
        let scores = parse_score_array("[]").unwrap();
        assert_eq!(scores, Vec::<u8>::new());
    }

    #[test]
    fn parse_clamps_scores_to_0_10() {
        // Values outside 1-10 are clamped to [0, 10].
        let scores = parse_score_array("[15, -3, 7]").unwrap();
        assert_eq!(scores, vec![10, 0, 7]);
    }

    #[test]
    fn parse_no_array_returns_error() {
        assert!(parse_score_array("no array here").is_err());
    }

    #[test]
    fn parse_malformed_json_returns_error() {
        assert!(parse_score_array("[1, 2, abc]").is_err());
    }

    #[test]
    fn parse_prose_with_extra_brackets_extracts_first_array() {
        // The model returns a second bracket group in the trailing prose;
        // only the first balanced array should be extracted.
        let text = "Here are the scores [8, 6, 3] for the URLs [listed above].";
        let scores = parse_score_array(text).unwrap();
        assert_eq!(scores, vec![8, 6, 3]);
    }
}
