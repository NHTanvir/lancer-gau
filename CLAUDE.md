# CLAUDE.md

Guidance for Claude Code (and humans) working in this repository.

## What this is

`lancer-gau` — A Rust clone of `gau` (Get All URLs), part of the Lancer ecosystem — a complete 2026 AI-era bug bounty toolkit. It fetches known URLs for a target domain from multiple passive sources (Wayback Machine, Common Crawl, OTX, URLScan) and is currently in active development with scaffold and implementation in progress.

## Architecture

- `src/main.rs` — Binary entry point; CLI argument parsing via `clap`.
- `src/runner.rs` — Core orchestration logic; coordinates provider fetches and output.
- `src/providers/` — One module per passive URL source: `wayback.rs`, `commoncrawl.rs`, `otx.rs`, `urlscan.rs`.
- `src/filters.rs` — URL filtering logic (extensions, patterns, regex).
- `src/dedup.rs` — Deduplication using `cuckoofilter` for memory-efficient URL deduplication.
- `src/config.rs` — Configuration loading (TOML-based).
- `src/output.rs` — Output formatting and writing results.
- `src/ai_rank.rs` — AI-assisted URL ranking/prioritisation for bug bounty relevance.
- `src/diff.rs` — Diff support for comparing URL sets across runs.

## Conventions

- Error propagation uses `anyhow::Result`; no `unwrap`/`expect` on fallible I/O.
- GUI gated behind the default `gui` feature; `--no-default-features` builds headless CLI.
- Library code in `src/lib.rs`; binary entry point in `src/main.rs`.
- All HTTP requests go through `reqwest` with `rustls-tls` (no native-tls); gzip and brotli decompression enabled by default.
- Async runtime is Tokio (`features = ["full"]`); all provider fetches are async and should use `async-trait` where trait objects are needed.

## Build / test / run

```
build.bat                   # Windows → build\lancer-gau.exe
build-windows.bat           # Windows packaged dist → dist\lancer-gau-windows-x86_64\bin\lancer-gau.exe
./build-linux.sh            # Linux/macOS → build/lancer-gau
cargo build --release       # standard release build
cargo test                  # run tests
```

Requirements: Rust/Cargo via rustup; on Windows, Visual C++ Build Tools 2019/2022.

## Repo notes

- `plan.md` is local-only and must NOT be committed. It is gitignored.
- `.claude/` and `target/`/`build/`/`dist/` are local-only and gitignored.
