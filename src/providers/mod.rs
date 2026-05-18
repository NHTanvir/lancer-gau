//! Provider trait and shared types for lancer-gau.

use async_trait::async_trait;
use rand::seq::SliceRandom;

/// Shared browser User-Agent strings used when `--random-agent` is set.
/// Both the Wayback and Common Crawl providers use this list.
pub const USER_AGENTS: &[&str] = &[
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:125.0) Gecko/20100101 Firefox/125.0",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_4_1) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4.1 Safari/605.1.15",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Safari/537.36 Edg/123.0.0.0",
    "Mozilla/5.0 (X11; Ubuntu; Linux x86_64; rv:125.0) Gecko/20100101 Firefox/125.0",
    "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4_1 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4.1 Mobile/15E148 Safari/604.1",
    "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Mobile Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36 OPR/110.0.0.0",
];

/// A single URL record yielded by any provider.
#[derive(Debug, Clone)]
pub struct UrlRecord {
    pub url: String,
    /// Which provider produced this record: "wayback", "commoncrawl", "otx", "urlscan"
    pub source: String,
    pub status_code: Option<u16>,
    pub mime_type: Option<String>,
    /// ISO-ish timestamp string from provider metadata.
    pub timestamp: Option<String>,
}

/// Return a random user-agent string from the shared list.
pub fn random_ua(rng: &mut impl rand::Rng) -> &'static str {
    USER_AGENTS.choose(rng).copied().unwrap_or("Mozilla/5.0")
}

/// Every URL source implements this trait.
#[async_trait]
pub trait Provider: Send + Sync {
    async fn fetch_urls(
        &self,
        domain: &str,
        include_subs: bool,
        client: &reqwest::Client,
    ) -> anyhow::Result<Vec<UrlRecord>>;
}

pub mod commoncrawl;
pub mod otx;
pub mod urlscan;
pub mod wayback;
