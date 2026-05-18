//! Runner — orchestrates all providers concurrently using tokio.
//!
//! The runner spawns one tokio task per `(domain, provider)` pair, caps
//! concurrent tasks with a `Semaphore`, collects all `UrlRecord`s through an
//! `mpsc` channel, and returns them together with per-provider URL counts.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, Semaphore};
use tracing::warn;

use crate::config::Config;
use crate::dedup::Dedup;
use crate::providers::commoncrawl::CommonCrawlProvider;
use crate::providers::otx::OtxProvider;
use crate::providers::urlscan::UrlScanProvider;
use crate::providers::wayback::WaybackProvider;
use crate::providers::{Provider, UrlRecord};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Per-provider URL counts after deduplication. Numbers reflect unique
/// URLs accepted from each provider, not the total fetched.
pub struct ProviderStats {
    /// Provider name → number of URLs returned across all domains.
    pub by_provider: HashMap<String, usize>,
}

impl ProviderStats {
    /// Sum of URL counts across all providers.
    pub fn total(&self) -> usize {
        self.by_provider.values().sum()
    }
}

/// The combined result returned by [`run`].
pub struct RunnerResult {
    /// Deduplicated URL records.
    pub records: Vec<UrlRecord>,
    pub stats: ProviderStats,
    /// Number of unique URLs kept after deduplication.
    pub unique_count: usize,
    /// Number of duplicate URLs dropped during deduplication.
    pub duplicate_count: usize,
}

// ---------------------------------------------------------------------------
// Internal channel message
// ---------------------------------------------------------------------------

/// Wraps a `UrlRecord` together with its provider name so that stats can be
/// accumulated from a single receive loop without re-inspecting `rec.source`.
struct Message {
    /// `Arc<str>` so that cloning per record is a cheap refcount bump, not a heap allocation.
    provider: Arc<str>,
    record: UrlRecord,
}

// ---------------------------------------------------------------------------
// run()
// ---------------------------------------------------------------------------

/// Fetch URLs for every `(domain, provider)` pair concurrently.
///
/// Concurrency is capped at `config.threads` simultaneous tasks.  Each task
/// sends its records into a shared `mpsc` channel; the main task drains that
/// channel and builds the final [`RunnerResult`].
///
/// Currently infallible — errors are per-task and logged as warnings.
/// Returns Ok unconditionally; the Result type is kept for future error conditions.
pub async fn run(
    domains: &[String],
    config: &Config,
    client: &reqwest::Client,
) -> anyhow::Result<RunnerResult> {
    let concurrency = config.threads.max(1);
    let semaphore = Arc::new(Semaphore::new(concurrency));

    // Back-pressure at 10k queued records; tasks block rather than OOM on large sites.
    let (tx, mut rx) = mpsc::channel::<Message>(10_000);

    let mut task_handles = Vec::new();

    for domain in domains {
        for provider_name in &config.providers {
            // Build the concrete provider — skip unknown names.
            let provider: Box<dyn Provider> = match provider_name.as_str() {
                "wayback" => Box::new(WaybackProvider::new(
                    config.retries,
                    config.random_agent,
                    config.from.clone(),
                    config.to.clone(),
                )),
                "commoncrawl" => Box::new(CommonCrawlProvider::new(
                    config.retries,
                    config.random_agent,
                )),
                "otx" => Box::new(OtxProvider::new(
                    config.retries,
                    config.random_agent,
                    config.otx.apikey.clone(),
                )),
                "urlscan" => Box::new(UrlScanProvider::new(
                    config.retries,
                    config.random_agent,
                    config.urlscan.apikey.clone(),
                )),
                other => {
                    warn!("Unknown provider: {other}; skipping");
                    continue;
                }
            };

            // Use the config provider name as the label; it matches the
            // canonical identifier since unknown names are skipped above.
            let canonical_name: Arc<str> = Arc::from(provider_name.as_str());

            let sem = Arc::clone(&semaphore);
            let tx = tx.clone();
            let domain = domain.clone();
            let provider_name = provider_name.clone();
            let include_subs = config.subdomains;
            let client = client.clone();

            let handle = tokio::spawn(async move {
                // Acquire permit — this limits total concurrent tasks.
                let _permit = sem.acquire().await.expect("semaphore closed");

                match provider.fetch_urls(&domain, include_subs, &client).await {
                    Ok(records) => {
                        for record in records {
                            let msg = Message {
                                // Arc::clone is a cheap refcount bump — no heap allocation per record.
                                provider: Arc::clone(&canonical_name),
                                record,
                            };
                            // If the receiver is gone we stop quietly.
                            if tx.send(msg).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            domain = %domain,
                            provider = %provider_name,
                            error = %e,
                            "Provider fetch failed"
                        );
                    }
                }
            });

            task_handles.push(handle);
        }
    }

    // Drop the original sender so the channel closes after all tasks finish.
    drop(tx);

    // Drain the channel while tasks run in the background, deduplicating as we go.
    let mut dedup = Dedup::new();
    let mut records: Vec<UrlRecord> = Vec::new();
    let mut by_provider: HashMap<String, usize> = HashMap::new();

    while let Some(msg) = rx.recv().await {
        if dedup.check_and_insert(&msg.record.url) {
            *by_provider.entry(msg.provider.to_string()).or_insert(0) += 1;
            records.push(msg.record);
        }
    }

    // Wait for all spawned tasks to finish (they should already be done since
    // the channel is drained, but we join to propagate any panics).
    for handle in task_handles {
        if let Err(e) = handle.await {
            warn!("Runner task panicked: {e:?}");
        }
    }

    Ok(RunnerResult {
        unique_count: dedup.seen_count(),
        duplicate_count: dedup.duplicate_count(),
        records,
        stats: ProviderStats { by_provider },
    })
}
