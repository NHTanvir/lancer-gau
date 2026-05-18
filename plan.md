# RESEARCH DOSSIER: gau

> Part of the Lancer ecosystem. A Rust clone of lc's gau (Get All URLs) — historical URL extraction from Wayback Machine / Common Crawl / OTX / URLScan. Build with Claude Code.
> Upstream: github.com/lc/gau (Go, latest v2.x). Descendant of tomnomnom's waybackurls. Standard recon-pipeline tool.

## Feature Parity Mandate

**`lancer-gau` MUST ship with 100% feature parity to the upstream `gau` tool — every flag, every subcommand, every output format, every probe / detector / module / payload / mode — plus a native Rust GUI on top.** This is not a "subset clone" — this is a full reimplementation in Rust with a GUI wrapper.

### Parity contract

- **Every CLI flag** documented in upstream `gau --help` (and the long-form man page / docs) must work in `lancer-gau --cli`.
- **Every output format** the upstream tool emits (text, JSON, XML, CSV, SARIF, custom formats) must round-trip through `lancer-gau` byte-equivalent where the format allows.
- **Every probe / detector / module / category / payload class / built-in script** must be ported. None silently dropped.
- **Every supported target type / input format / protocol / file format / language** must work.
- **Every configuration option** (config files, env vars, registry / settings entries) must be honoured.
- **Companion binaries / sub-tools** in the upstream suite (if any) must each get their own binary in this workspace.

### GUI on top

Every feature reachable via CLI is also reachable from the GUI (see `## GUI (Lancer-standard pattern)` below). The GUI is not a subset — it is an alternate frontend over the same `core` library that the CLI uses. The `core` crate is GUI-agnostic.

### Verification

- The `## Granular feature catalog` section below enumerates everything that must be supported.
- Reference: `tests/upstream_parity/` will pin upstream flag combinations + canned outputs as parity tests. CI fails if any flag is unsupported or output diverges.
- A `lancer-gau --self-test` flag runs the full parity matrix against a bundled reference target and prints pass/fail per upstream feature group.

### What is allowed to diverge

- **Performance** — Rust-native, async-first; we are allowed to be faster.
- **Memory footprint** — we are allowed to be smaller.
- **GUI** — entirely new (no upstream GUI to copy except where the upstream ships its own GUI like Zenmap / Wireshark UI / Ghidra GUI / jadx-gui — in which case our GUI must cover every panel the upstream GUI has, plus the Lancer-standard layout from `## GUI` below).
- **AI augmentation** — net-new (see `## AI augmentation ideas`).

### What is NOT allowed to diverge

- Any flag silently dropped, renamed, or behavior-changed compared to upstream.
- Any output format / record schema diverging in field names, types, or semantics.
- Any built-in script / detector / module not ported (mark explicitly with `[N/A — see note]` if genuinely impossible in Rust, e.g. NSE Lua scripts requiring features not in `mlua`).

## What it actually does (1-paragraph)

gau ("Get All URLs") fans a target apex domain out to four passive URL aggregators — Wayback Machine CDX API, Common Crawl index, AlienVault OTX, URLScan.io — pulls every URL ever indexed for that host (and optionally its subdomains), deduplicates across sources, optionally filters by extension/status code/regex, and emits one URL per line on stdout. It is **purely passive** — never sends a packet to the target — which makes it scope-safe and stealthy. It sits between subfinder/dnsx and ffuf/nuclei in the standard recon pipe: `gau target.com | uro | httpx -mc 200 | nuclei`. The output is a goldmine for "forgotten endpoint" bug bounty hunting — admin paths devs thought they took down, deprecated `/v1/` API endpoints with looser auth, file upload endpoints with weak validation, and parameter names you'd never guess otherwise.

## Internal architecture (Go upstream, file-level)

```
gau/
├── cmd/gau/
│   └── main.go              # entrypoint; flag parsing, runner construction
├── pkg/
│   ├── config.go            # Config struct, defaults
│   ├── output/
│   │   ├── json.go          # JSON output writer
│   │   ├── text.go          # plain text writer
│   │   └── output.go        # OutputWriter interface
│   ├── providers/
│   │   ├── provider.go      # Provider interface
│   │   ├── wayback.go       # web.archive.org CDX query + pagination
│   │   ├── commoncrawl.go   # index.commoncrawl.org query
│   │   ├── otx.go           # otx.alienvault.com query
│   │   └── urlscan.go       # urlscan.io API query
│   └── runner/
│       └── runner.go        # orchestrates providers, dedup, filtering, output
├── go.mod
├── go.sum
└── .gau.toml.example        # default config file template
```

**Concurrency model.** One goroutine per (domain, provider) pair, all writing into a single result channel. A receiver goroutine drains the channel, applies the bloom-filter dedup, runs include/exclude filters, and writes to the output writer. Pagination per provider runs synchronously within the provider goroutine (pages are sequential by API design).

**Per-provider rate-limit.** Each provider self-manages — Wayback CDX naturally rate-limits via slow responses on large queries; OTX requires API key + has documented per-key rate limits; URLScan has API quotas. There is no central rate limiter (a known weakness; gauplus and other forks added one).

**Configuration.** `.gau.toml` in CWD or `~/.gau.toml`:
```toml
threads = 5
verbose = false
retries = 5
subdomains = false
parameters = false
providers = ["wayback", "commoncrawl", "otx", "urlscan"]
blacklist = ["png", "jpg", "gif", "css", "ico", "svg", "woff", "woff2", "ttf", "eot"]
json = false

[urlscan]
apikey = "..."

[otx]
apikey = "..."
```

## Granular feature list

1. `-providers wayback,commoncrawl,otx,urlscan` — comma-separated source list (default: all four)
2. `-blacklist png,jpg,gif,css,ico,svg,woff,woff2,ttf,eot,jpeg,bmp,webp,mp3,mp4,avi,mov,pdf,zip` — extension blacklist applied to URL path
3. `-mc 200,301,302` — match status code (from Wayback CDX metadata; not a live probe)
4. `-fc 404,500` — filter (exclude) status code
5. `-mt text/html,application/json` — match MIME type
6. `-ft image/png` — filter MIME type
7. `--subs` — include subdomains (`*.target.com`)
8. `--threads 5` — concurrent providers per domain
9. `--retries 5` — per-provider retry on transient errors
10. `--timeout 60` — per-request timeout (seconds)
11. `--proxy http://127.0.0.1:8080` — HTTP/HTTPS proxy (useful for inspecting traffic in Burp/Lancer)
12. `--random-agent` — randomize User-Agent header per request
13. `--o output.txt` — output file (default: stdout)
14. `--json` — JSON-lines output with source attribution + timestamp + status
15. `--verbose` — log every provider start/end, page count, dedup ratio
16. `--config /path/to/.gau.toml` — explicit config path
17. `--version`
18. `--from 200601` / `--to 202612` — Wayback CDX date range (YYYYMM); other providers ignore
19. `--fp` — **dedup URLs that differ only in parameter values** (collapses `/foo?id=1` and `/foo?id=2` into one) — NOT "include URLs with file path parameters" as previously documented; upstream behavior is endpoint-variant collapse
20. STDIN input — one domain per line, no flag needed if no positional arg given (v2.2.3 fixed stdin provider handling)

**Verified upstream flag set (v2.2.4, Oct 2024):**
`--blacklist`, `--config`, `--fc`, `--from`, `--ft`, `--fp`, `--json`, `--mc`, `--mt`, `--o`, `--providers`, `--proxy`, `--retries`, `--timeout`, `--subs`, `--threads`, `--to`, `--verbose`, `--version`. **Note:** `--random-agent` is NOT in upstream v2.2.4 (it exists in `gauplus` fork). Lancer ships it as a value-add but must mark it `[Lancer-extension]` in `--help` to keep parity audit honest.

**Lancer-extension flags (clearly marked, off by default if behavior changes parity):**
- `--random-agent` (from `gauplus`) — rotate User-Agent per request
- `--ai-rank` — Lancer-only Claude-assisted ranking
- `--diff` — re-run mode, alert on new URLs
- `--resume` — checkpoint resume for very long Wayback queries
- `--sort-params` — alternate normalization that sorts query params before dedup (off by default — matches gau)
- `--provider virustotal` — extension provider (see below)
- `--provider crtsh` — cert-transparency-derived hostnames + URLs
- `--provider github-codesearch` — code-search-derived URLs (requires GitHub token)

**Hidden features (from reading source):**
- Wayback CDX `resumeKey` pagination is undocumented edge-case-prone — gau handles the "empty resumeKey but more pages" case via a retry-once heuristic.
- Common Crawl picks the *latest* index dynamically by listing `s3://commoncrawl/cc-index/collections/` (LIST without auth via the HTTP listing).
- OTX paginates with `page=N&limit=500` — gau iterates until response has `<500` URLs.
- URLScan uses `search?q=domain:target.com&size=10000` — single page max; if you need more, paginate via `search_after`.
- URLs are normalized before dedup: scheme lowercased, default ports stripped (`http://example.com:80/` → `http://example.com/`), trailing slash kept as-is, fragment dropped, query params NOT sorted (preserves variant param orders which may matter for tests).

## Engineering hard parts

1. **Wayback CDX pagination edge cases.** The `resumeKey` field is documented but unreliable: empty resumeKey can mean "done" or "transient gap"; gau retries once before declaring done. Large queries (1M+ URLs) may timeout mid-page; need streaming JSON parser, not loading whole response.
2. **Common Crawl index discovery.** No stable URL — must list `s3://commoncrawl/cc-index/collections/` and parse to find the latest `cc-index-collection`. Then the index format itself paginates differently per year (early indexes are gzipped JSONL; recent are CDX-format with their own pagination).
3. **OTX rate limits.** Soft rate limits (HTTP 429 with retry-after). Without API key, very low limits. Need exponential backoff per (key, IP) pair.
4. **URLScan large-domain handling.** Search caps at 10k results per query; if a domain has >10k indexed scans, must paginate via `search_after` cursor. Sort order matters (newest-first by default; for full coverage need to iterate).
5. **Dedup at scale.** A scan of `microsoft.com` can yield 50M+ raw URLs. In-memory `HashSet<String>` blows up RAM. Use a Cuckoo filter (false-positive ~1%) or a `BTreeSet` on `xxhash` of normalized URL (collision risk negligible for the volumes seen).
6. **URL normalization for dedup.** Decisions: lowercase scheme/host, strip default ports, decode percent-encoded ASCII (`%2E` → `.`), but preserve case in query params (some apps are case-sensitive there). Drop fragment. Sort query params or not? gau doesn't sort; this preserves variant orders but inflates output. Lancer can offer both modes.
7. **Memory streaming.** For 50M-URL outputs, the dedup set must spill to disk above a threshold (~5GB). Use a hybrid in-memory + on-disk Cuckoo filter (e.g. spill to `redb` once RAM threshold hit).
8. **Resume-from-checkpoint.** Long Wayback queries take hours; if interrupted, restart should resume from last `resumeKey`. gau doesn't do this; Lancer's clone should.
9. **Per-source attribution in JSON output.** Useful for triage ("this URL appeared in OTX only — likely from a malicious scanner; this URL appeared in 4 sources — high-confidence real URL").

## Rust library mapping (with versions, as of 2026)

| Need | Crate | Notes |
|---|---|---|
| HTTP client | `reqwest = "0.12"` (features: `json`, `rustls-tls`, `gzip`, `brotli`, `stream`) | Standard |
| Async runtime | `tokio = "1.40"` (full) | Standard |
| CLI | `clap = "4"` (derive) | Standard |
| Config TOML | `serde + toml = "0.8"` | Standard |
| JSON | `serde_json = "1"` | Standard |
| Output JSON-lines | `serde_jsonlines = "0.5"` or hand-roll | Easy |
| URL parsing/normalization | `url = "2.5"` | RFC 3986 compliant |
| Hashing for dedup | `xxhash-rust = "0.8"` features: `xxh3` | Fast non-crypto hash |
| Cuckoo filter | `cuckoofilter = "0.5"` | Memory-efficient probabilistic dedup |
| On-disk KV (for spill) | `redb = "2"` | Pure Rust ACID |
| Streaming JSON parse | `serde_json::StreamDeserializer` or `simd-json` | For large Wayback responses |
| HTTP date parsing | `chrono = "0.4"` (serde feature) | Wayback timestamps |
| Random User-Agent | `rand = "0.8"` + hardcoded UA list | Trivial |
| Errors | `anyhow = "1"` + `thiserror = "1"` | Standard |
| Tracing | `tracing = "0.1"` + `tracing-subscriber = "0.3"` | Standard |
| Async pagination | `futures = "0.3"` (`Stream` trait) | For per-provider streams |
| Async semaphore | `tokio::sync::Semaphore` | Concurrency cap |

## Data source endpoints (verified 2026)

| Provider | Endpoint | Auth | Pagination |
|---|---|---|---|
| Wayback CDX | `https://web.archive.org/cdx/search/cdx?url=<domain>/*&output=json&fl=original,statuscode,timestamp,mimetype&collapse=urlkey` | None | `resumeKey` field; `&showResumeKey=true&limit=100000` |
| Wayback CDX (subdomains) | Same with `url=*.<domain>/*` | None | Same |
| Common Crawl | `https://index.commoncrawl.org/CC-MAIN-<index>-index?url=*.<domain>&output=json` | None | `page=N&pageSize=15000`; available indexes via `https://index.commoncrawl.org/collinfo.json` |
| AlienVault OTX | `https://otx.alienvault.com/api/v1/indicators/hostname/<domain>/url_list?limit=500&page=N` | Optional API key (header `X-OTX-API-KEY`) | `page=N` until response has `<limit` URLs |
| URLScan | `https://urlscan.io/api/v1/search/?q=domain:<domain>&size=10000` | Optional API key (header `API-Key`) | `search_after=<cursor>` from previous response's last result |

**Lancer extension providers (not in upstream — add behind `--providers`):**

| Provider | Endpoint | Auth | Notes |
|---|---|---|---|
| VirusTotal | `https://www.virustotal.com/api/v3/domains/<domain>/urls?limit=40` | Required API key (header `x-apikey`) | Returns malicious/suspicious URL detections + first-seen timestamps; useful for spotting compromised endpoints |
| crt.sh | `https://crt.sh/?q=%25.<domain>&output=json` | None | Cert-transparency hostnames; pipes into per-host Wayback queries |
| GitHub code-search | `https://api.github.com/search/code?q=<domain>` | Required PAT (header `Authorization`) | URLs scraped from public code references; aggressive rate-limit |
| HackerOne disclosed | `https://hackerone.com/<program>/disclosed?` | None | Disclosed report URLs containing target (manual scope opt-in) |
| Robots.txt history | `https://web.archive.org/web/timemap/link/https://<domain>/robots.txt` | None | Every historical robots.txt — disallowed paths are *gold* |

Rate limits (verified May 2026):
- Wayback: soft, ~10 req/s before throttling; large queries naturally slow.
- Common Crawl: very generous (S3-backed); CDX server is slower (~1 req/s safe).
- OTX: ~10 req/min unauth, ~10k req/day with API key.
- URLScan: 1k req/day unauth, configurable with paid account.
- VirusTotal (extension): 4 req/min, 500/day on free tier; 1k+/min on paid.
- crt.sh (extension): unlimited but slow (1-3s response time per query).
- GitHub code-search (extension): 30 req/min authenticated, 10/min unauth.

## Filter / output / dedup catalog (full feature surface)

### Filter operators (CLI + GUI)

| Filter | CLI flag | Semantics | Notes |
|---|---|---|---|
| Extension blacklist | `--blacklist` | comma-separated extensions stripped from path | applies to last `.ext` token; default list = png/jpg/jpeg/gif/svg/ico/woff/woff2/ttf/eot/css/scss/js/map/mp3/mp4/avi/mov/pdf/zip/bmp/webp |
| Extension whitelist | `--ext` `[Lancer-extension]` | only include these extensions | inverse of `--blacklist`; mutually exclusive |
| Status match | `--mc 200,301,302` | include only these CDX statuscode values | values from Wayback CDX metadata; not a live probe |
| Status filter | `--fc 404,500` | exclude these statuscodes | |
| MIME match | `--mt text/html,application/json` | CDX mimetype prefix match | |
| MIME filter | `--ft image/png` | exclude MIME prefixes | |
| Regex match | `--match-regex '<re>'` `[Lancer-extension]` | URL must match regex | RE2 syntax |
| Regex filter | `--filter-regex '<re>'` `[Lancer-extension]` | URL must NOT match regex | RE2 syntax |
| Path prefix match | `--match-path /api,/v1` `[Lancer-extension]` | URL path must start with one of these | |
| Path filter | `--filter-path /static,/dist` `[Lancer-extension]` | URL path must NOT start with these | |
| Date range from | `--from YYYYMM` | Wayback CDX `from` parameter | other providers ignore |
| Date range to | `--to YYYYMM` | Wayback CDX `to` parameter | other providers ignore |
| Param dedup | `--fp` | dedup `/foo?a=1` and `/foo?a=2` → one | upstream confirmed |
| Host scope | `--subs` | include subdomains via `*.<domain>` wildcard | |

### Dedup pipeline

- **Pass 1 — normalization**: lowercase scheme/host, strip default ports (80/443), drop fragment, decode safe percent-encodings (`%2E`→`.`, `%2F` stays as `/`), preserve case in path + query.
- **Pass 2 — hash**: `xxh3_64` of normalized URL string.
- **Pass 3 — Cuckoo filter**: insert hash; reject duplicates (~1% FP rate accepted).
- **Pass 4 — spill**: when filter > 5GB RAM, migrate to `redb` on-disk store under `~/.cache/lancer-gau/dedup-<runid>.redb`; lookup is O(log n) via B-tree.
- **`--fp` mode**: drop query string entirely before hash (collapse all param-variants of same endpoint).
- **`--sort-params` mode** `[Lancer-extension]`: stable-sort query params alphabetically before hash.
- **`--strict-dedup` mode** `[Lancer-extension]`: switch to exact `BTreeSet<String>` (no false positives, higher RAM).

### Output formats

| Format | Flag | Schema |
|---|---|---|
| Plain text (default) | none | one URL per line; stdout or `--o file` |
| JSON-lines | `--json` | `{"url":"…","source":"wayback","timestamp":"20210712140532","status":200,"mime":"text/html"}` per line |
| CSV | `--csv` `[Lancer-extension]` | header: `url,source,timestamp,status,mime` |
| SARIF | `--sarif` `[Lancer-extension]` | for SIEM ingestion; one `result` per URL with `kind:"informational"` |
| Markdown table | `--md` `[Lancer-extension]` | GUI export friendly |
| Newline-delimited NDJSON with full provider metadata | `--ndjson-full` `[Lancer-extension]` | every original CDX/OTX/URLScan field passed through |

### Network / transport options

| Option | CLI flag | Notes |
|---|---|---|
| HTTP proxy | `--proxy http://127.0.0.1:8080` | upstream supports; for Burp/Lancer-proxy inspection |
| SOCKS5 proxy | `--proxy socks5://…` `[Lancer-extension]` | `reqwest` `socks` feature |
| Per-provider proxy | `proxies.wayback = "…"` in TOML `[Lancer-extension]` | route OTX through Tor while keeping Wayback direct |
| Timeout | `--timeout 60` | per-request |
| Retries | `--retries 5` | per-provider on transient errors |
| Retry backoff | `--retry-backoff exp,2,30` `[Lancer-extension]` | exp/linear, base, max-seconds |
| Custom headers | `--header 'X-Foo: bar'` `[Lancer-extension]` | repeatable |
| TLS verify off | `--insecure` `[Lancer-extension]` | for proxies with bad certs |
| Disable HTTP/2 | `--http1` `[Lancer-extension]` | some proxies break HTTP/2 |
| Cert pinning | `--ca-bundle /path/ca.pem` `[Lancer-extension]` | for corporate MITM proxies |
| Bind address | `--bind 10.0.0.5:0` `[Lancer-extension]` | source-IP selection for multi-NIC hosts |

### Config file (full schema)

```toml
threads = 5
verbose = false
retries = 5
timeout = 60
subdomains = false
parameters = false  # corresponds to --fp
providers = ["wayback", "commoncrawl", "otx", "urlscan"]
blacklist = ["png", "jpg", ...]
json = false
output = ""  # empty = stdout

# Lancer-extension keys
random_agent = false
ai_rank = false
sort_params = false
strict_dedup = false
match_regex = ""
filter_regex = ""

[urlscan]
apikey = ""

[otx]
apikey = ""

[virustotal]  # Lancer-extension
apikey = ""

[github]  # Lancer-extension
token = ""

[proxies]  # Lancer-extension
wayback = ""
commoncrawl = ""
otx = ""
urlscan = ""
default = ""
```

Config search order (matches upstream v2.2.4):
1. Path from `--config` flag
2. `$XDG_CONFIG_HOME/gau/.gau.toml` (Linux)
3. `~/.gau.toml` (Linux/macOS)
4. `%USERPROFILE%\.gau.toml` (Windows)
5. Built-in defaults

## Real-world recon pipeline patterns

```bash
# Standard recon pipe (Jhaddix-style)
subfinder -d target.com | dnsx -resp-only | httpx -silent > live-hosts.txt
gau --subs target.com | uro | tee gau-urls.txt | httpx -silent -mc 200 -o live-urls.txt
nuclei -list live-urls.txt -tags exposure,misconfig

# Find old / forgotten paths
gau target.com | grep -E '/(admin|debug|test|staging|dev|backup|old|tmp)' | httpx -silent -mc 200

# Param mining for fuzzing
gau --subs target.com | unfurl --unique keys | sort -u > params.txt
ffuf -u "https://target.com/api?PARAM=test" -w params.txt:PARAM -fc 404

# Wayback-only secret hunting
gau --providers wayback target.com | grep -E '\.(json|env|conf|yml|yaml|xml|sql|bak|git)$' | httpx -silent -mc 200

# Combine with jsluice for endpoint discovery from old JS bundles
gau target.com | grep -E '\.js$' | httpx -silent -mc 200 -o old-js-files.txt
xargs -a old-js-files.txt -I{} curl -s {} | jsluice urls -
```

## Bug bounty value + AI augmentation ideas

**Why bug bounty hunters care:**
- Historical URLs reveal endpoints that no longer appear in the current site map but may still be live (devs took the link down, kept the endpoint).
- Old parameter names show validation/auth weaknesses devs forgot.
- Old file uploads (`/uploads/2018/...`) often have weaker MIME-type checks than current paths.
- Old `/v1/`, `/v2/`, `/api-old/` paths often have different auth — sometimes none.
- Test/staging endpoints accidentally indexed (`dev.target.com/internal-debug`).
- Parameters indexed in URLScan from real users — leaks session tokens, API keys.

**Lancer differentiator (AI-augmented gau):**

1. **AI URL prioritization (`--ai-rank`).** Pipe URL batches (100 at a time) through Claude with prompt: "rank these URLs by exploit potential (1-10). Look for: admin/debug/internal paths, params suggesting IDOR (`id=`, `user=`, `account=`), file-upload endpoints, deprecated API versions, oddly-formed paths." Reorder output by AI score.
2. **AI category clustering.** Group URLs by AI-inferred function: "admin panels", "file uploads", "auth flows", "API endpoints", "static assets". Triage faster.
3. **AI parameter extraction with semantics.** Beyond `unfurl --unique keys` (syntactic), have AI infer parameter purpose: `?u=` → username, `?token=` → auth, `?redirect=` → open-redirect candidate.
4. **Diff mode.** Re-run `gau` weekly, alert on NEW URLs (newly indexed) — Lancer publishes notifications to Discord/Telegram. New URLs on mature targets are gold (just-deployed endpoints with bugs).
5. **OAST correlation.** When `gau` reveals a URL like `https://target.com/img?url=...`, automatically queue a Lancer SSRF probe with OAST collaborator domain — chain recon → exploitation.

## 2024–2026 ecosystem state

- **gau itself**: low-maintenance project, last meaningful release 2023 but still works fine. `lc` (author) is active but on other tools.
- **gauplus** (bp0lr/gauplus): fork adding random User-Agent rotation, better retries, per-provider rate-limit; partial overlap with current gau.
- **katana** (ProjectDiscovery, 2023+): active crawler — not a gau replacement; complementary (gau is passive history, katana is live crawl).
- **waybackurls** (tomnomnom): the original; superseded by gau (which queries more sources) but still in many old pipelines.
- **chaos-client** (ProjectDiscovery): different purpose — pre-indexed bug bounty subdomain corpus, not URLs.
- **getJS / linkfinder / jsluice**: complementary — extract URLs from live JS bundles; gau extracts from historical indexes.
- **No major arch changes 2024-2026.** The category is mature; differentiation now comes from output processing (uro, gf patterns, AI ranking), not from raw fetching.

**Notable forks/competitors in 2026:**
- `gauplus` — actively maintained fork
- `kr` (kentron) — broader OSINT URL aggregator (less focused on Wayback)
- Custom in-house tools at every consultancy

### 2026-05-30 — deep-research note (upstream snapshot vs Lancer plan)

**Verified upstream**: `lc/gau` v2.2.4 released 2024-10-28; only 4 patch releases in 2024 (v2.2.2/v2.2.3 on 2024-05-14 fixed Wayback API + stdin handling; v2.2.4 made `.gau.toml` location configurable, resolved issue #74). **No new providers, no new flags, no new output formats since v2.2.0 (Nov 2023).** No 2025 or 2026 releases visible — confirmed the upstream is in mature/low-maintenance mode.

**Upstream flag set (audited 2026-05-30)**: `--blacklist`, `--config`, `--fc`, `--from`, `--ft`, `--fp`, `--json`, `--mc`, `--mt`, `--o`, `--providers`, `--proxy`, `--retries`, `--timeout`, `--subs`, `--threads`, `--to`, `--verbose`, `--version`. Total: 19 flags. **`--random-agent` is NOT in upstream gau v2.2.4** — exists only in `gauplus` fork; Lancer keeps it but tags `[Lancer-extension]` in `--help` to keep parity-audit honest.

**Upstream provider set**: `wayback`, `commoncrawl`, `otx`, `urlscan`. Total: 4. **No VirusTotal, no crt.sh, no GitHub code-search, no robots.txt-history in upstream** — Lancer adds these 5 as opt-in extension providers (see provider table).

**`--fp` semantic correction**: previously plan listed as "include URLs with file path parameters"; upstream docs confirm it means "Remove different parameters in the same endpoint" — i.e. dedup endpoint variants. Plan corrected at item 19.

**Gaps closed in this revision**:
- Filter operators table (12 operators) — added Lancer extensions clearly tagged.
- Output formats — added CSV, SARIF, Markdown, NDJSON-full as Lancer extensions.
- Network/transport — added SOCKS5, per-provider proxy, custom headers, TLS controls, bind address.
- Dedup pipeline — explicit 4-pass spec + `--strict-dedup` mode for paranoid users.
- Config schema — full TOML reference with `[proxies]`, `[virustotal]`, `[github]` sections.
- Provider extensions — 5 new sources documented with endpoints + auth + rate limits.

**Net result**: plan now covers 19 upstream flags + 15 Lancer-extension flags, 4 upstream providers + 5 Lancer extensions, 2 upstream output formats + 4 Lancer extensions. Parity audit surface unchanged; differentiation surface clearly tagged.

## Validation test corpus

For your Rust clone, validate against:
1. **Known-large target**: `microsoft.com` — gau should return >10M URLs. Test memory behavior + dedup correctness.
2. **Known-small target**: a personal blog with `<100` URLs in Wayback. Test for missed URLs.
3. **Wildcard cert targets**: `*.github.io` — many subdomains; test subdomain handling.
4. **Known-misindexed target**: any old defunct startup — test that 404'd-then-removed URLs still surface from Wayback.
5. **Cross-provider overlap**: a popular target like `apple.com` — measure dedup ratio (should be 30-60% overlap across providers).

**Pass criteria**: ≤5% URL count delta vs upstream gau on identical inputs, with explicit list of differences (e.g. "we normalize differently — we strip trailing slash, gau doesn't").

## Implementation phases (8-week clone)

| Week | Deliverable |
|---|---|
| 1 | Cargo project skeleton; CLI (clap); Config TOML loader; Provider trait + Wayback CDX provider with pagination (resumeKey edge cases). |
| 2 | Common Crawl provider — dynamic index discovery via `collinfo.json`; CDX pagination. |
| 3 | OTX + URLScan providers with API-key handling; per-provider retry/backoff. |
| 4 | Runner: tokio orchestration; mpsc channel; semaphore concurrency cap; tokio task per (domain, provider). |
| 5 | Dedup engine: in-memory Cuckoo filter + xxh3-hashed normalized URLs; on-disk spill via `redb` above 5GB. |
| 6 | Filters: extension blacklist (default list matches gau), status code match/exclude, MIME match/exclude, regex match/exclude, date range. |
| 7 | Output: stdout, file, JSON-lines (with source attribution + timestamp + status). Subdomain support. Verbose mode with per-provider stats. |
| 8 | **AI ranking** mode (`--ai-rank`) — Lancer differentiator; batch URLs to Claude via `lancer-ai` shared crate, reorder output. Diff mode for re-runs. Validation against gau-upstream on `microsoft.com` and `apple.com`; tune to within 5% delta. |

## CLAUDE.md section to add to the clone repo

```
This project = a gau (Get All URLs) clone in Rust.
- Providers implement Provider trait (src/providers/mod.rs).
- Wayback CDX pagination via resumeKey — handle the empty-resumeKey-with-more-pages edge case (retry once, then declare done).
- Common Crawl: discover latest index from index.commoncrawl.org/collinfo.json — don't hardcode an index name (rotates monthly).
- Default extension blacklist: png, jpg, jpeg, gif, svg, ico, woff, woff2, ttf, eot, css, scss, js, map, mp3, mp4, avi, mov, pdf, zip, bmp, webp.
- Dedup uses xxh3-hashed normalized URLs in a Cuckoo filter (allow ~1% false-positive rate; spill to redb above 5GB).
- URL normalization for dedup: lowercase scheme/host, strip default ports (80/443), decode safe percent-encodings, drop fragment. Do NOT sort query params (matches gau, preserves variant orders).
- AI ranking mode: batch 100 URLs at a time to Claude (lancer-ai); 1-10 exploit-potential score; reorder output.
- Never send live probes — purely passive.
- Provider concurrency capped by tokio::sync::Semaphore (default 5).
```

---

## GUI (Lancer-standard pattern)

`lancer-gau` ships as a **native GUI application** by default. The CLI surface listed in `## Granular feature catalog` is still available behind a `--cli` flag (or via the `lancer-gau-cli` companion binary), but the **plain `lancer-gau` invocation opens a native desktop window**, matching every other tool in the Lancer ecosystem. No terminal-only mode.

### Framework

Pure-Rust GUI stack — no JavaScript, no Node, single static binary:

| Crate | Role |
|-------|------|
| `eframe` | Cross-platform window + event loop wrapper around `egui` |
| `egui` | Immediate-mode UI widgets (panels, tables, forms, menus, drag-and-drop) |
| `egui_extras` | Sortable tables, image loaders, syntax highlighting, date pickers |
| `egui_plot` | Charts, graphs, timelines (histograms, activity meters, progress) |
| `rfd` | Native file/folder pickers (open file, save dialog) |
| `directories` | Per-OS config dir (`~/.config/lancer-gau/`, `%APPDATA%\lancer-gau\`, etc.) |
| `image` | Screenshot / preview rendering inside egui |
| `arboard` | Clipboard read/write for "copy result" actions |
| `muda` | Native menu bar (Linux / macOS / Windows) |
| `egui-notify` | Toast notifications |

Optional dark/light theme persisted to `lancer-gau-config.json` via `serde_json`.

### Window layout

Three-pane layout matching the Lancer mothership GUI:

```
┌─────────────────────────────────────────────────────────────────────┐
│  File   Tools   Settings   Help                              [≡]    │
├──────────┬────────────────────────────────────────┬─────────────────┤
│          │                                        │                 │
│  Inputs  │           Main view                    │  Detail pane    │
│  / jobs  │  (table of results / source tree /     │  (full record / │
│  / scope │   findings list / packet list / etc.)  │   bytes / src / │
│          │                                        │   remediation)  │
│          │                                        │                 │
├──────────┴────────────────────────────────────────┴─────────────────┤
│  Status: idle           Items: 0          [Open]  [Run]  [Export]   │
└─────────────────────────────────────────────────────────────────────┘
```

- **Left sidebar (200-300px)** — inputs / queued jobs / scope / target list. Drag-and-drop accepted.
- **Center pane (flexible)** — primary result view appropriate to the tool (table for findings, tree for hierarchical data, code editor for source).
- **Right pane (300-400px, collapsible)** — full detail of the currently-selected item.
- **Top menu bar** — File (Open / Save / Export / Recent / Quit), Tools (per-tool actions), Settings (theme, fonts, integrations), Help (About / Docs).
- **Bottom status bar** — current activity, item counts, primary action buttons.

### Cross-tool integration with Lancer + the other 29 clones

Every Lancer-clone GUI exposes a small local HTTP server on a port auto-allocated at launch (`127.0.0.1:<port>`, written to `%TEMP%/lancer-gau.port`):

- The main Lancer GUI discovers running tools by scanning `%TEMP%/lancer-*.port` and lists them in its sidebar.
- Right-clicking any item in any tool can "Send to <other Lancer tool>" — the receiving tool fetches the payload via its own HTTP endpoint.
- A future "Lancer Hub" tray shell can launch / focus any of the 30 GUIs from one menu.
- HTTP endpoints exposed:
  - `GET  /v1/state` — current jobs / progress
  - `POST /v1/import` — accept a target / file / record from another Lancer tool
  - `GET  /v1/export?format=json|csv|sarif|markdown` — pull the current result set in a portable format

### Async work + responsiveness

- Long-running scans run on a Tokio task pool; the GUI thread only renders.
- Progress propagates via `crossbeam::channel` from worker → UI.
- A `Stop` button cancels in-flight work via a `CancellationToken`.
- All file I/O / network I/O / subprocess work goes through async channels — the egui thread never blocks.

### Build output

The existing `build.bat` / `build-windows.bat` / `build-linux.sh` already produce a single binary. With `eframe` linked, the same binary is the GUI app.

- Windows: `build\lancer-gau.exe` (double-click opens the GUI window).
- Linux: `build/lancer-gau` (also installable as a `.desktop` entry via `build-linux.sh --install`).
- macOS: `build/lancer-gau` (bundled into `lancer-gau.app` via `cargo-bundle` in the `build-mac.sh` companion).

For CI / headless / scripting, pass `--cli` to fall through to the CLI surface from `## Granular feature catalog`.

## Build Setup (Lancer-standard pattern)

This clone follows the same build pattern as the main Lancer project — single `.exe` output via `build.bat` on Windows, `.so/elf` via `build-linux.sh` on Linux.

### Project layout

```
lancer-gau/
├── Cargo.toml              # single-crate Rust project
├── Cargo.lock              # committed (binary, not library)
├── src/
│   └── main.rs             # entry point (scaffold)
├── build.bat               # Windows build → build/lancer-gau.exe
├── build-windows.bat       # Windows packaged dist build → dist/lancer-gau-windows-x86_64/
├── build-linux.sh          # Linux build → build/lancer-gau
├── .gitignore              # plan.md (local-only) + target/ + build/ + dist/
├── README.md               # public-facing (pushed to GitHub)
└── plan.md                 # this file (gitignored — local blueprint, never pushed)
```

### Build commands

**Windows (double-click `build.bat`):**
```
build.bat
```
Produces `build\lancer-gau.exe`. Pauses at end so the window stays open.

**Windows (packaged dist):**
```
build-windows.bat
```
Produces `dist\lancer-gau-windows-x86_64\bin\lancer-gau.exe` plus LICENSE/README — ready to zip.

**Linux / macOS:**
```bash
./build-linux.sh
```
Produces `build/lancer-gau`.

### Requirements
- Rust toolchain (https://rustup.rs)
- Windows: Visual C++ Build Tools 2019/2022 (needed by some crates)

### CI

The build pattern is intentionally identical across Lancer and all 19 clones, so a single Jenkins/GitHub Actions matrix can build everything by calling `build-windows.bat` or `build-linux.sh` per repo.
