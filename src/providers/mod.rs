//! Provider trait and shared types for lancer-gau.

use async_trait::async_trait;

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

/// Every URL source implements this trait.
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;

    async fn fetch_urls(
        &self,
        domain: &str,
        include_subs: bool,
        client: &reqwest::Client,
    ) -> anyhow::Result<Vec<UrlRecord>>;
}

pub mod wayback;
