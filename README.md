# tabbyapi-monitor

A terminal dashboard for LLM servers running
[TabbyAPI](https://github.com/theroyallab/tabbyAPI) with the
[ExLlamaV3](https://github.com/turboderp-org/exllamav3) backend. It shows live
concurrency, token throughput, cache/KV usage, engine latency, speculative-decoding
acceptance and GPU readings, for one server or a whole fleet. The layout and
controls follow [vllmstat](https://github.com/bryanvine/vllmstat), in a
Catppuccin Mocha palette.

It is a single native binary: no Python, Prometheus server, browser or daemon.
It only reads from your server and never sends inference requests.

```text
tabbyapi-monitor  DEMO · synthetic ExLlamaV3 @ http://localhost:8080  EXLLAMAV3  ● CONNECTED  up 30s  0.5s  [SYNTHETIC
DEMO]

╭─────────────────────────────────────────────────────────╮  ╭─────────────────────────────────────────────────────────╮
│ CONCURRENCY                                             │  │ THROUGHPUT                                              │
│ running 1 · waiting 0 · preempt — · max-seqs —          │  │ gen 72.0 tok/s · prompt EOS 0.0 tok/s · tok/iter — ·    │
│                                                         │  │ 0.0 jobs/s                                              │
│                             ⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿   │  │                             ⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿   │
│                             ⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿   │  │                             ⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿   │
│                             ⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿   │  │                             ⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿   │
│                             ⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿   │  │                             ⢸⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿   │
│ running jobs · last 29s                                 │  │ gen tok/s · last 29s                                    │
│                                                         │  │                                 ⢸   ⢸   ⢸   ⢸    ⡇      │
│                                                         │  │                                 ⢸   ⢸   ⢸   ⢸    ⡇      │
│                                                         │  │                                 ⢸   ⢸   ⢸   ⢸    ⡇      │
│                             ⢀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀   │  │                             ⢀⣀⣀⣀⣸⣀⣀⣀⣸⣀⣀⣀⣸⣀⣀⣀⣸⣀⣀⣀⣀⣇⣀⣀⣀   │
│ waiting jobs · last 29s                                 │  │ computed prompt tok/s · EOS · last 29s                  │
╰─────────────────────────────────────────────────────────╯  ╰─────────────────────────────────────────────────────────╯

╭──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╮
│ CACHE & KV MEMORY                                                                                                    │
│ reuse    prefix hit — · sources computed 2.2M / cached 5.1k tok · cached share 0.2% (EOS)                            │
│ memory   KV usage 12.2% (32.1k/262.1k tok) · reusable 260.6k tok (overlaps used)                                     │
╰──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯

╭──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╮
│ TEE                                                                                                                  │
│ Request feed unavailable · TabbyAPI metrics do not expose request text.                                              │
```

<sub>`tabbyapi-monitor --mock` (synthetic demo data)</sub>

## Requirements

- **A TabbyAPI server that exposes Prometheus metrics (schema v1).** Stock
  TabbyAPI does not have a `/metrics` endpoint. The monitor was built with
  [Qwen3.8-27b-turboderp-exl3-exllamav3-tabby](https://github.com/anstaendig/Qwen3.8-27b-turboderp-exl3-exllamav3-tabby),
  a pinned TabbyAPI + ExLlamaV3 deployment. It includes a patch that adds
  `GET /metrics` and documents the
  [metrics contract](https://github.com/anstaendig/Qwen3.8-27b-turboderp-exl3-exllamav3-tabby/blob/main/docs/metrics-contract.md).
  Any server exposing the same `tabbyapi_*` metrics works. Without them the
  monitor reports a clear schema/endpoint error.
- **Rust** stable toolchain with Cargo, to build (install via
  [rustup](https://rustup.rs)). Developed with Rust 1.98.
- macOS or Linux terminal with UTF-8 and true-color support.
- Optional: `nvidia-smi` on the local machine, or on a host reachable with
  non-interactive `ssh`, for GPU readings.

## Installation

Install from GitHub with Cargo:

```sh
cargo install --locked --git https://github.com/anstaendig/tabbyapi-monitor
```

This puts `tabbyapi-monitor` in `~/.cargo/bin` (make sure it is on your `PATH`).

Or build from a clone:

```sh
git clone https://github.com/anstaendig/tabbyapi-monitor
cd tabbyapi-monitor
cargo build --release --locked
./target/release/tabbyapi-monitor --mock   # try it without a server
```

`./scripts/monitor` is a small wrapper that runs the release build from a clone.

## Usage

```sh
# Local server on the default http://localhost:8080
tabbyapi-monitor

# Remote server; GPU readings via nvidia-smi over SSH
tabbyapi-monitor --url http://gpu-box:8080 --gpu ssh --gpu-host gpu-box

# Use the host/port from a TabbyAPI config.yml
tabbyapi-monitor --config /path/to/tabbyAPI/config.yml

# Fleet overview of several servers; Enter opens one, Esc returns
tabbyapi-monitor -u http://gpu-a:8080 -u http://gpu-b:8080

# One snapshot for scripts (JSON; null means unavailable)
tabbyapi-monitor --url http://gpu-box:8080 --once --json --no-gpu
```

Run `tabbyapi-monitor --help` for all options. Without a terminal (for example
when piped) it prints one plain-text snapshot and exits. `--once` exits nonzero
if any target's metrics could not be read.

| Setting | How |
| --- | --- |
| Server URL | `--url` / `-u` (alias `--endpoint`), or `TABBYAPI_ENDPOINT`; default `http://localhost:8080` |
| API key | `TABBYAPI_API_KEY` (or `VLLM_API_KEY`), `--api-key`, or `api_key` in the config file; sent as a bearer token |
| Refresh | `--interval` / `-i`, 0.1–60 s (default 1 s); `--timeout` per request, default 3 s |
| Metrics path | `--metrics-path`, default `/metrics` |
| GPU | `--gpu local` (default), `--gpu ssh --gpu-host HOST`, or `--no-gpu` |
| Colors | set `NO_COLOR` to disable |

Prefer the environment variable or a config file for API keys so they don't end
up in your shell history.

### Keys

| Key | Action |
| --- | --- |
| `q`, Ctrl-C | Quit |
| `p` | Pause/resume polling |
| `d` | Show/hide details: latency, session totals, outcomes, speculative decoding |
| `g` | Toggle GPU readings |
| `t` | Toggle the request-feed panel |
| `r` | Reset the selected server's session totals and rate baselines |
| `+` / `-` | Halve / double the refresh interval |
| `j`/`k`, ↓/↑ | Select fleet row; scroll the dashboard |
| PgDn/PgUp, Home | Scroll the dashboard / back to top |
| Enter / Esc | Open the selected server / back to the fleet |

### Fleet config file

Several servers can be listed in a TOML file. It is read from `--config`,
`TABBYAPI_MONITOR_CONFIG`, `./tabbyapi-monitor.toml`, or
`~/.config/tabbyapi-monitor/config.toml`, in that order. URLs given on the
command line are added; duplicates are removed. At most 32 servers.

```toml
interval = 1.0
gpu = true

[[instance]]
name = "local"
url = "http://localhost:8080"
gpus = [0]          # GPU indices to show for this server in the fleet view

[[instance]]
name = "remote"
url = "https://llm.example.com"
# api_key = "..."   # keep this file private: chmod 600
```

## How it works

Each server is polled on its own background thread with plain HTTP `GET`s:

- `/metrics` every interval: Prometheus text (schema v1) with counters for
  output/prompt tokens and finished jobs, gauges for active jobs and KV cache,
  and histograms for engine timings.
- `/v1/model` and `/health` about once a minute: model name, context length
  and health.

The monitor turns this into:

- **Rates** from counter deltas between polls. Counter resets (server restart)
  start a new baseline instead of producing negative rates.
- **Charts** of the last 120 seconds (at most 1,200 samples per server) drawn
  with braille characters. A single failed poll is bridged, and the next rate
  spans the missed interval. Sustained outages, restarts and pauses leave a
  visible gap, and the header shows `STALE` while polls fail.
- **Latency percentiles** (p50/p90/p99) interpolated from histogram buckets
  over a rolling 120-second window.
- **Session totals** accumulated locally since start or the last `r`.

GPU readings come from `nvidia-smi` run locally or through `ssh -o BatchMode=yes`
on the host you name. They describe that host, not the server, and each field
degrades on its own if unavailable.

### Reading the numbers

- Concurrency and outcomes count **engine jobs**, not HTTP requests.
- Prompt-token counters and speculative-decoding counts update when a job
  finishes (marked **EOS**), so prompt throughput is not live prefill speed.
- Latencies are **engine-side** estimates from histograms, not what a client
  measures end to end.
- Cached-token share is the fraction of prompt tokens served from cache. It is
  not a prefix-cache hit rate.
- Values the server doesn't report (preemptions, max sequences, tokens per
  iteration, prefix hit rate, request text) are shown as `—`. They are never
  invented. Measured zeros stay zero. In JSON, unavailable values are `null`.

## Security and privacy

- Read-only: only `GET /metrics`, `/v1/model` and `/health` are sent. Prompts,
  responses and chat templates are never requested or stored.
- Redirects are refused, so the API key is only sent to the URL you configured.
  Response bodies are capped at 1 MiB and never echoed in error messages.
- Text from the server or `nvidia-smi` is stripped of control characters before
  display.
- HTTPS uses rustls with the bundled Mozilla root certificates.
- SSH runs non-interactively (`BatchMode`) with a timeout. The host argument is
  validated and passed without a shell.

## Development

```sh
cargo test --locked                    # unit and rendering tests
cargo build --release --locked
python3 scripts/acceptance.py          # end-to-end checks against a local fake server
python3 scripts/acceptance.py --live-endpoint http://your-server:8080   # plus a passive live check
```

`scripts/acceptance.py` (Python 3, macOS/Linux) runs the release binary in a
pseudo-terminal against a local fake server. It checks JSON output,
auth/timeout/malformed-response handling, fleet isolation, key controls, theme
colors, terminal restore and idle CPU/memory. Reports go to
`.state/acceptance/<timestamp>/`.

## License

[MIT](LICENSE) © 2026 Marcel Holter. The dashboard layout and braille chart
design are adapted from vllmstat (Apache-2.0); see
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
