//! lancer-gau — Rust clone of gau (Get All URLs).
//!
//! Entry point: parse CLI, load config, run providers, print results.

mod config;
mod dedup;
mod filters;
mod output;
mod providers;
mod runner;

use std::io::{self, BufRead};
use std::time::Duration;

use clap::Parser;
use tracing::debug;

use config::{load_config, parse_codes, split_csv, Config};
use filters::Filter;

// ---------------------------------------------------------------------------
// CLI definition — all 20 flags
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(
    name = "lancer-gau",
    version,
    about = "Fetch known URLs for domains from multiple sources (Wayback, CommonCrawl, OTX, URLScan)"
)]
struct Cli {
    /// One or more target domains (reads from stdin if not provided)
    #[arg(value_name = "DOMAIN")]
    domains: Vec<String>,

    /// Comma-separated providers: wayback,commoncrawl,otx,urlscan
    #[arg(long, value_name = "list")]
    providers: Option<String>,

    /// Extension blacklist (comma-separated)
    #[arg(long, value_name = "list")]
    blacklist: Option<String>,

    /// Match only these HTTP status codes (comma-separated)
    #[arg(long, value_name = "codes")]
    mc: Option<String>,

    /// Filter (exclude) these HTTP status codes (comma-separated)
    #[arg(long, value_name = "codes")]
    fc: Option<String>,

    /// Match only these MIME types (comma-separated)
    #[arg(long, value_name = "types")]
    mt: Option<String>,

    /// Filter (exclude) these MIME types (comma-separated)
    #[arg(long, value_name = "types")]
    ft: Option<String>,

    /// Include subdomains (*.domain)
    #[arg(long)]
    subs: bool,

    /// Concurrent providers
    #[arg(long, value_name = "n")]
    threads: Option<usize>,

    /// Per-provider retries
    #[arg(long, value_name = "n")]
    retries: Option<u32>,

    /// Per-request timeout in seconds
    #[arg(long, value_name = "secs")]
    timeout: Option<u64>,

    /// HTTP/HTTPS proxy URL
    #[arg(long, value_name = "url")]
    proxy: Option<String>,

    /// Randomize User-Agent per request
    #[arg(long)]
    random_agent: bool,

    /// Output file (default: stdout)
    #[arg(short = 'o', value_name = "file")]
    output: Option<String>,

    /// JSON-lines output
    #[arg(long)]
    json: bool,

    /// Verbose logging (debug level)
    #[arg(long)]
    verbose: bool,

    /// Explicit config file path
    #[arg(long, value_name = "path")]
    config: Option<String>,

    /// Wayback date range start (YYYYMM)
    #[arg(long, value_name = "YYYYMM")]
    from: Option<String>,

    /// Wayback date range end (YYYYMM)
    #[arg(long, value_name = "YYYYMM")]
    to: Option<String>,

    /// Include file path parameters in output
    #[arg(long)]
    fp: bool,

    /// Match URLs against regex (only emit matches)
    #[arg(long)]
    mr: Option<String>,

    /// Filter URLs against regex (exclude matches)
    #[arg(long)]
    fr: Option<String>,
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // ---- tracing setup ----
    let log_level = if cli.verbose { "debug" } else { "warn" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level)),
        )
        .init();

    // ---- load + merge config ----
    let mut cfg = load_config(cli.config.as_deref())?;

    // CLI overrides config file
    if let Some(ref p) = cli.providers {
        cfg.providers = split_csv(p);
    }
    if let Some(ref b) = cli.blacklist {
        cfg.blacklist = split_csv(b);
    }
    if cli.subs {
        cfg.subdomains = true;
    }
    if let Some(n) = cli.threads {
        cfg.threads = n;
    }
    if let Some(r) = cli.retries {
        cfg.retries = r;
    }
    if let Some(t) = cli.timeout {
        cfg.timeout = t;
    }
    if let Some(ref p) = cli.proxy {
        cfg.proxy = Some(p.clone());
    }
    if cli.random_agent {
        cfg.random_agent = true;
    }
    if let Some(ref f) = cli.output {
        cfg.output_file = Some(f.clone());
    }
    if cli.json {
        cfg.json = true;
    }
    if cli.verbose {
        cfg.verbose = true;
    }
    if let Some(ref f) = cli.from {
        cfg.from = Some(f.clone());
    }
    if let Some(ref t) = cli.to {
        cfg.to = Some(t.clone());
    }
    if cli.fp {
        cfg.parameters = true;
    }
    if let Some(ref mc) = cli.mc {
        cfg.match_codes = parse_codes(mc)?;
    }
    if let Some(ref fc) = cli.fc {
        cfg.filter_codes = parse_codes(fc)?;
    }
    if let Some(ref mt) = cli.mt {
        cfg.match_mime = split_csv(mt);
    }
    if let Some(ref ft) = cli.ft {
        cfg.filter_mime = split_csv(ft);
    }
    if let Some(ref mr) = cli.mr {
        cfg.match_regex = Some(mr.clone());
    }
    if let Some(ref fr) = cli.fr {
        cfg.filter_regex = Some(fr.clone());
    }

    // Warn about flags that are accepted but not yet fully implemented.
    if cfg.parameters {
        eprintln!("[warn] --fp (file path parameters) is not yet implemented");
    }

    debug!(config = ?cfg, "Resolved configuration");

    // ---- gather domains ----
    let domains: Vec<String> = if !cli.domains.is_empty() {
        cli.domains.clone()
    } else {
        // Read from stdin, one domain per line
        let stdin = io::stdin();
        stdin
            .lock()
            .lines()
            .filter_map(|l| l.ok())
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    };

    if domains.is_empty() {
        eprintln!("No domains provided. Pass as arguments or pipe via stdin.");
        std::process::exit(1);
    }

    // ---- build reqwest client ----
    let client = build_client(&cfg)?;

    // ---- build filter early (fail fast on bad regex / invalid date args) ----
    let filter = Filter::from_config(&cfg)?;

    // ---- run providers concurrently via runner ----
    debug!("Starting concurrent runner for {} domain(s)", domains.len());
    let result = runner::run(&domains, &cfg, &client).await?;

    // ---- apply filters ----
    let filtered: Vec<_> = result
        .records
        .into_iter()
        .filter(|r| filter.accept(r))
        .collect();

    // ---- output records ----
    let mut writer = output::make_writer(cfg.output_file.as_deref(), cfg.json)?;
    let write_result: std::io::Result<()> = (|| {
        for record in &filtered {
            writer.write_record(record)?;
        }
        Ok(())
    })();
    let flush_result = writer.flush();
    write_result?;
    flush_result?;

    // ---- verbose stats ----
    if cfg.verbose {
        output::report_stats(
            &result.stats,
            result.unique_count,
            result.duplicate_count,
            filtered.len(),
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a `reqwest::Client` from the resolved config.
fn build_client(cfg: &Config) -> anyhow::Result<reqwest::Client> {
    let timeout = Duration::from_secs(if cfg.timeout == 0 { 60 } else { cfg.timeout });

    let mut builder = reqwest::Client::builder()
        .timeout(timeout)
        .use_rustls_tls()
        .gzip(true)
        .brotli(true);

    if let Some(ref proxy_url) = cfg.proxy {
        let proxy = reqwest::Proxy::all(proxy_url.as_str())?;
        builder = builder.proxy(proxy);
    }

    Ok(builder.build()?)
}

