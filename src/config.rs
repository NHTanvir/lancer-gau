//! Configuration loading: TOML file + CLI override.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Per-provider sub-configs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct UrlScanConfig {
    #[serde(default)]
    pub apikey: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct OtxConfig {
    #[serde(default)]
    pub apikey: String,
}

// ---------------------------------------------------------------------------
// Top-level Config
// ---------------------------------------------------------------------------

/// Application configuration loaded from TOML and merged with CLI flags.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    #[serde(default = "default_threads")]
    pub threads: usize,

    #[serde(default)]
    pub verbose: bool,

    #[serde(default = "default_retries")]
    pub retries: u32,

    #[serde(default)]
    pub subdomains: bool,

    #[serde(default)]
    pub parameters: bool,

    #[serde(default = "default_providers")]
    pub providers: Vec<String>,

    #[serde(default = "default_blacklist")]
    pub blacklist: Vec<String>,

    #[serde(default)]
    pub json: bool,

    // Optional: match / filter status codes and MIME types (CLI-only for now,
    // but stored here so the runner can read them from one place).
    #[serde(default)]
    pub match_codes: Vec<u16>,

    #[serde(default)]
    pub filter_codes: Vec<u16>,

    #[serde(default)]
    pub match_mime: Vec<String>,

    #[serde(default)]
    pub filter_mime: Vec<String>,

    #[serde(default)]
    pub timeout: u64,

    #[serde(default)]
    pub proxy: Option<String>,

    #[serde(default)]
    pub random_agent: bool,

    #[serde(default)]
    pub output_file: Option<String>,

    #[serde(default)]
    pub from: Option<String>,

    #[serde(default)]
    pub to: Option<String>,

    #[serde(default)]
    pub match_regex: Option<String>,

    #[serde(default)]
    pub filter_regex: Option<String>,

    /// AI-rank URLs by exploit potential (requires ANTHROPIC_API_KEY env var).
    #[serde(default)]
    pub ai_rank: bool,

    /// Diff mode: emit only URLs not present in the previous run file.
    #[serde(default)]
    pub diff: Option<String>,

    // Sub-configs
    #[serde(default)]
    pub urlscan: UrlScanConfig,

    #[serde(default)]
    pub otx: OtxConfig,
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

fn default_threads() -> usize {
    5
}

fn default_retries() -> u32 {
    5
}

fn default_providers() -> Vec<String> {
    vec![
        "wayback".to_string(),
        "commoncrawl".to_string(),
        "otx".to_string(),
        "urlscan".to_string(),
    ]
}

fn default_blacklist() -> Vec<String> {
    [
        "png", "jpg", "jpeg", "gif", "svg", "ico", "woff", "woff2", "ttf", "eot", "css", "scss",
        "js", "map", "mp3", "mp4", "avi", "mov", "pdf", "zip", "bmp", "webp",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            threads: default_threads(),
            verbose: false,
            retries: default_retries(),
            subdomains: false,
            parameters: false,
            providers: default_providers(),
            blacklist: default_blacklist(),
            json: false,
            match_codes: vec![],
            filter_codes: vec![],
            match_mime: vec![],
            filter_mime: vec![],
            timeout: 60,
            proxy: None,
            random_agent: false,
            output_file: None,
            from: None,
            to: None,
            match_regex: None,
            filter_regex: None,
            ai_rank: false,
            diff: None,
            urlscan: UrlScanConfig::default(),
            otx: OtxConfig::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Attempt to read and deserialize a TOML config file.
/// Returns `None` if the file doesn't exist (errors on malformed TOML).
fn try_load_file(path: &Path) -> anyhow::Result<Option<Config>> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)?;
    let cfg: Config = toml::from_str(&text)?;
    Ok(Some(cfg))
}

/// Load config using the priority order:
/// 1. `--config <path>` (explicit)
/// 2. `./.gau.toml` (CWD)
/// 3. `~/.gau.toml` (home)
/// 4. Hard-coded defaults
pub fn load_config(explicit_path: Option<&str>) -> anyhow::Result<Config> {
    // 1. Explicit path
    if let Some(p) = explicit_path {
        let path = PathBuf::from(p);
        let cfg = try_load_file(&path)?.ok_or_else(|| {
            anyhow::anyhow!("Config file not found: {}", path.display())
        })?;
        return Ok(cfg);
    }

    // 2. CWD
    let cwd_config = PathBuf::from(".gau.toml");
    if let Some(cfg) = try_load_file(&cwd_config)? {
        return Ok(cfg);
    }

    // 3. Home dir
    if let Some(home) = dirs_home() {
        let home_config = home.join(".gau.toml");
        if let Some(cfg) = try_load_file(&home_config)? {
            return Ok(cfg);
        }
    }

    // 4. Defaults
    Ok(Config::default())
}

/// Cross-platform home directory helper (avoids an extra dep).
fn dirs_home() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOMEDRIVE").and_then(|d| std::env::var("HOMEPATH").map(|p| d + &p)))
            .ok()
            .map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
}

// ---------------------------------------------------------------------------
// Parse helper utilities
// ---------------------------------------------------------------------------

/// Split a comma-separated string into a `Vec<String>`.
pub fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(|p| p.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Parse comma-separated u16 status codes.
pub fn parse_codes(s: &str) -> anyhow::Result<Vec<u16>> {
    s.split(',')
        .map(|p| {
            p.trim()
                .parse::<u16>()
                .map_err(|e| anyhow::anyhow!("Invalid status code '{}': {}", p.trim(), e))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_csv_normal() {
        assert_eq!(split_csv("a,b,c"), vec!["a", "b", "c"]);
    }

    #[test]
    fn split_csv_trims_spaces() {
        assert_eq!(split_csv(" foo , bar , baz "), vec!["foo", "bar", "baz"]);
    }

    #[test]
    fn split_csv_filters_empty() {
        assert_eq!(split_csv("a,,b"), vec!["a", "b"]);
    }

    #[test]
    fn split_csv_empty_string() {
        let result = split_csv("");
        assert!(result.is_empty());
    }

    #[test]
    fn parse_codes_valid() {
        assert_eq!(parse_codes("200,301,404").unwrap(), vec![200, 301, 404]);
    }

    #[test]
    fn parse_codes_with_spaces() {
        assert_eq!(parse_codes("200, 404").unwrap(), vec![200, 404]);
    }

    #[test]
    fn parse_codes_invalid_returns_error() {
        assert!(parse_codes("200,abc").is_err());
    }

    #[test]
    fn parse_codes_single() {
        assert_eq!(parse_codes("200").unwrap(), vec![200u16]);
    }
}
