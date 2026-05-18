//! lancer-gau — Rust clone of gau (Get All URLs).
//!
//! Entry point: parse CLI, load config, run providers, print results.

mod config;
mod dedup;
mod providers;
mod runner;

use std::io::{self, BufRead};
use std::time::Duration;

use clap::Parser;
use tracing::debug;

use config::{load_config, parse_codes, split_csv, Config};
use providers::UrlRecord;

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

    // Warn about flags that are accepted but not yet fully implemented.
    if cfg.parameters {
        eprintln!("[warn] --fp (file path parameters) is not yet implemented");
    }
    if cfg.output_file.is_some() {
        eprintln!("[warn] -o/--output file writing is not yet implemented; outputting to stdout");
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

    // ---- set up output writer ----
    // (Full file-output support comes in later tasks; for now we always write
    //  to stdout but honour the --json flag format.)
    let use_json = cfg.json;

    // ---- run providers concurrently via runner ----
    debug!("Starting concurrent runner for {} domain(s)", domains.len());
    let result = runner::run(&domains, &cfg, &client).await?;

    // ---- output records ----
    for rec in &result.records {
        if should_output(rec, &cfg) {
            print_record(rec, use_json);
        }
    }

    // ---- verbose stats ----
    if cfg.verbose {
        let mut sorted: Vec<(&String, &usize)> = result.stats.by_provider.iter().collect();
        sorted.sort_by_key(|(k, _)| k.as_str());
        for (name, n) in &sorted {
            tracing::info!(provider = %name, count = n, "provider stats");
        }
        let total = result.stats.total();
        tracing::info!(total = total, "total URLs");
        tracing::info!("dedup: {} unique URLs ({} duplicates removed)", result.unique_count, result.duplicate_count);
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

/// Determine whether a `UrlRecord` passes the blacklist / match / filter
/// criteria defined in `cfg`.
fn should_output(rec: &UrlRecord, cfg: &Config) -> bool {
    // Extension blacklist
    if !cfg.blacklist.is_empty() {
        // Strip query string before checking extension
        let path = rec.url.split('?').next().unwrap_or(&rec.url);
        let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
        if cfg.blacklist.iter().any(|b| b.eq_ignore_ascii_case(&ext)) {
            return false;
        }
    }

    // Match status codes (if any specified — URL must match at least one)
    if !cfg.match_codes.is_empty() {
        match rec.status_code {
            Some(code) if cfg.match_codes.contains(&code) => {}
            _ => return false,
        }
    }

    // Filter (exclude) status codes
    if !cfg.filter_codes.is_empty() {
        if let Some(code) = rec.status_code {
            if cfg.filter_codes.contains(&code) {
                return false;
            }
        }
    }

    // Match MIME types
    if !cfg.match_mime.is_empty() {
        match &rec.mime_type {
            Some(mt) if cfg.match_mime.iter().any(|m| mt.contains(m.as_str())) => {}
            _ => return false,
        }
    }

    // Filter MIME types
    if !cfg.filter_mime.is_empty() {
        if let Some(ref mt) = rec.mime_type {
            if cfg.filter_mime.iter().any(|m| mt.contains(m.as_str())) {
                return false;
            }
        }
    }

    true
}

/// Print a single `UrlRecord` to stdout (plain text or JSON-lines).
fn print_record(rec: &UrlRecord, use_json: bool) {
    if use_json {
        // Minimal JSON-lines format
        let obj = serde_json::json!({
            "url": rec.url,
            "source": rec.source,
            "status": rec.status_code,
            "mime": rec.mime_type,
            "timestamp": rec.timestamp,
        });
        println!("{}", obj);
    } else {
        println!("{}", rec.url);
    }
}
