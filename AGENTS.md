# Agent guide for rup

## Project overview

`rup` is a universal pinger library and CLI tool — measures RTT over UDP, TCP,
ICMP, and HTTP(S). Written in Rust (edition 2024) with the tokio async runtime.

The project is a single crate with two targets:

- **Library** (`src/lib.rs`) — re-exports the public API: `Transport`,
  transport implementations, core types, `Pinger`, `SessionConfig`, and the
  session building blocks.
- **Binary** (`src/main.rs`) — thin entry point; the CLI lives in `src/app.rs`,
  `src/cli.rs`, and `src/output.rs`.

Canonical documentation lives in `README.md` and `docs/`. This file is a working
summary for agents; when behavior changes, update the relevant `docs/` page and
only then adjust the summary here.

## Source map

| File | Target | Purpose |
|------|--------|---------|
| `src/lib.rs` | lib | Crate docs, `missing_docs` lint, module declarations, public re-exports |
| `src/session.rs` | lib | `Pinger`/`SessionConfig`/`PingReport`/`PingEvent`, task supervision, address helpers, server dispatch |
| `src/pinger.rs` | lib | Domain types (`Request`, `Response`, `PacketSize`, `Entry`, `StatEntry`, `Echo`, `SendMode`) + `generator` task |
| `src/protocol.rs` | lib | `Protocol` enum, CLI spelling parser, display names, port rules |
| `src/statistics/mod.rs` | lib | Statistics actor, live event emission, aggregate functions |
| `src/statistics/tracker.rs` | lib | Deterministic request/response matching and timeout state |
| `src/echo_codec.rs` | lib | Shared UDP/TCP echo header encoding and response padding |
| `src/tos.rs` | lib | Outgoing/incoming TOS / traffic-class socket helpers, per-platform |
| `src/unix.rs` | lib | Reviewed unsafe boundary for ancillary data and socket-address decoding |
| `src/http_target.rs` | lib | Dependency-free URL parsing: `HttpTarget`, `HttpScheme`, `HttpVersion` |
| `src/transport/mod.rs` | lib | `Transport` trait + `transmitter()`/`receiver()` adapters |
| `src/transport/async_udp.rs` | lib | UDP client transport + UDP echo server |
| `src/transport/async_tcp.rs` | lib | TCP client transport (echo or connect probes) + TCP echo server |
| `src/transport/async_icmp.rs` | lib | ICMP client transport (Linux ping socket, no root needed) |
| `src/transport/async_http.rs` | lib | HTTP/1.1 and HTTP/2 client transport, `http` feature |
| `src/transport/disabled/async_http.rs` | lib | Signature-compatible stub used without the `http` feature |
| `src/main.rs` | bin | Process entry point and module wiring |
| `src/app.rs` | bin | Runtime, DNS resolution, session wiring, Ctrl+C handling |
| `src/cli.rs` | bin | Clap definitions, parameter extraction, client/server validation |
| `src/output.rs` | bin | Human-readable and JSON Lines rendering of events and reports |
| `src/output_template.rs` | bin | `--format` parser and reply-field rendering |
| `tests/cli.rs` | test | CLI process behavior (help, errors, end-to-end JSONL session) |
| `tests/public_api.rs` | test | Public API compilation plus custom `Transport` sessions |

## Library public API

External consumers add `rup` as a dependency:

```toml
[dependencies]
rup = { git = "https://github.com/svart/rup" }
```

### High-level API (easiest integration)

```rust
use rup::{Pinger, SessionConfig, Target};

let session = SessionConfig::default()
    .count(5)
    .interval(std::time::Duration::from_secs(1));
let report = Pinger::new(Target::Udp("127.0.0.1:5000".parse().unwrap()))
    .session(session)
    .run()
    .await?;

println!("min/avg/max = {:?}/{:?}/{:?}", report.min(), report.mean(), report.max());
```

### Raw building blocks (custom pipelines)

- `rup::Transport` — implement for a custom protocol
- `rup::run_with_transport()`, `rup::start_with_transport()` — run a session
  over any `Transport` with a `rup::SessionConfig`
- `rup::Target` — built-in transport plus its protocol-specific data
- `rup::UdpTransport`, `rup::TcpTransport`, `rup::IcmpTransport`,
  `rup::HttpTransport` (`http` feature)
- `rup::HttpTarget`, `rup::HttpScheme`, `rup::HttpVersion`, `rup::Protocol`
- `rup::Request`, `rup::Response`, `rup::PacketSize`, `rup::TrafficClass`
- `rup::PingSession`, `rup::PingEvent` — consume live results with `next().await`
- `rup::PingReport` — `sent`/`received`/`rtts` plus `loss_pct`, `min`, `max`,
  `mean`, `median`, `std_dev`
- `rup::PingResult` — individual RTT measurement
- `rup::has_port()`, `rup::ensure_port()` — address helpers
- `rup::run_server()`, `rup::run_server_until()` — UDP/TCP echo servers

The crate sets `#![warn(missing_docs)]` and CI denies warnings, so every new
public item needs a `///` doc comment. Intra-doc links are checked by
`cargo doc --no-deps` with `RUSTDOCFLAGS=-D warnings`.

## Architecture

The client path is an actor-style pipeline. See `docs/architecture.md` for the
full description, timing rules, and protocol-specific behavior.

```text
generator ──Request──> transmitter ──StatEntry::Open──> statistics ──PingEvent──> caller
                          │                            (tracker)         ^
                    send()│                                  ^            │
                          v                                  │            │
                     Transport ──StatEntry::Close───────────┘────────────┘
```

### Data flow

1. **Generator** produces `Request` values at a fixed interval or adaptively
   after a reply/timeout signal.
2. **Transmitter** calls `Transport::send()`, records the send timestamp, and
   sends `StatEntry::Open`.
3. **Receiver** calls `Transport::recv()` and sends `StatEntry::Close`.
4. **Statistics actor** passes entries to the synchronous `MeasurementTracker`,
   which matches IDs, computes RTTs, expires timeouts, and emits `PingEvent`s.
5. **Caller/CLI** consumes events and renders human or JSON Lines output.

A close can arrive before its open; the tracker buffers it. Linux UDP reports a
matching remote ICMP error as a zero-size `Response`. High-level TCP selects
persistent rup echo when the first connection succeeds, otherwise it measures
one connect outcome per request.

`PingSession::stop()` closes request generation without discarding pending
results. The CLI maps Ctrl+C to this call and still prints the final summary;
library building blocks never install process-global signal handlers.

### Transport trait

```rust
pub trait Transport: Send + Sync {
    fn send(&self, req: &Request) -> impl Future<Output = io::Result<Instant>> + Send;
    fn recv(&self) -> impl Future<Output = io::Result<Response>> + Send;
}
```

`send` and `recv` run concurrently in separate tasks, and session dispatch
clones the transport into both, so implementations use `Arc` for shared state.
Transports must not panic on network errors: return `io::Error`.

To add a new protocol:

1. Create `src/transport/async_<proto>.rs` and implement `Transport`.
2. Register the module in `src/transport/mod.rs`.
3. Add a `Protocol` variant plus parser/display support in `src/protocol.rs`.
4. Add a `Target` variant and its arm in `spawn_ping_session` (`session.rs`).
5. Map `Protocol` to the new `Target` in `app.rs`.
6. Register server support, or add the protocol to the "no server" arm of
   `run_server_until` (`session.rs`).
7. Re-export the transport from `lib.rs`.
8. Update `--protocol` help in `cli.rs`, `docs/cli.md`, and
   `docs/architecture.md`; add parser, packet, transport, and session tests.

## Key patterns

### Error handling

All fallible operations return `io::Result`. The transmitter/receiver adapters
propagate transport errors; the statistics actor returns a report and the
session converts task failures into an `io::Error`. Server loops log per-client
errors with `tracing::warn!` and keep serving; library code does not use
`eprintln!`.

### Actor model

Components run as `tokio::spawn`ed tasks and communicate over
`tokio::sync::mpsc` channels. `tokio::select!` multiplexes channels, timers, and
signals.

### Debugging

- The CLI always initializes a `tracing-subscriber` writing to **stderr**,
  filtered to the `rup` target: `WARN` by default, `TRACE` with `-v` /
  `--verbose`. Stdout stays clean for human/JSONL output.
- Spans are `session` with children `generator`, `transmitter`, `receiver`, and
  `statistics`. Trace events are emitted per request/response/event.
- `src/statistics/tracker.rs` is pure and synchronous, so matching bugs are
  reproducible in unit tests without sockets.
- `src/echo_codec.rs` isolates wire encoding from transport I/O.

### ICMP (no root)

Uses `SOCK_DGRAM | IPPROTO_ICMP` (Linux ping socket) instead of `SOCK_RAW`.
The kernel assigns and matches the ICMP identifier. `rup` builds the ICMP
header (type 8, checksum, placeholder identifier, sequence) followed by the
Echo payload. On receive the kernel strips the IP header.

### Packet format (wire)

```text
[ICMP header (8)] [Echo payload (12+)]
```

The echo header is encoded manually in little-endian field order:
`{id: u64, len: u16, resp_size: u16}`. `PING_HDR_LEN = 12` is the sum of the
field sizes, not `size_of::<Echo>()` (16, due to alignment padding).

UDP and TCP carry the Echo payload directly. The server swaps `resp_size` into
`len`, zeroes `resp_size`, and returns `len` bytes padded with zeros.

### CLI (`-p` is a root option)

```text
rup [global opts] [client opts] <remote-address>
rup [global opts] server <local-address>
```

`-p`/`--protocol` is defined at the root level, so for server mode it must come
before the subcommand: `rup -p tcp server 127.0.0.1:5000`. Client mode defaults
to ICMP; server mode defaults to UDP. The parsed values live in
`PingerParams`/`ServerParams`.

### Optional features

`http` (default) gates hyper, rustls, and webpki-roots. The feature-only `cfg`
attributes are the `#[cfg_attr(not(feature = "http"), path =
"disabled/async_http.rs")]` module selection in `transport/mod.rs` and the
`HttpTransport` re-export in `lib.rs`; the code also uses platform cfgs
(`target_os = "linux"`, `unix`) around socket extensions. Keep the disabled
stub's signatures in sync, and validate both feature sets:

```sh
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
```

### HTTP

`-p http1` (alias `http`) and `-p http2` take a URL. A request is a `GET`, and
the RTT ends when the full body is read. HTTP/1.1 opens a new connection per
request (RTT includes connect + TLS); HTTP/2 shares one connection and
reconnects after GOAWAY. `connect()` sends one unmeasured warm-up `GET` so
configuration errors fail fast. Later failures count as loss. Sockets set
`TCP_NODELAY`.

### Port handling

- TCP/UDP require `host:port`.
- ICMP ignores the port; `ensure_port()` appends `:0` before DNS resolution.
- HTTP takes a URL; `ensure_port()` returns its `host:port` using the scheme
  default.
- DNS resolution uses `tokio::net::lookup_host()`.

UDP/TCP do not always need a running rup server: Linux UDP accepts matching
ICMP errors, and TCP connect-probe mode accepts completed connection outcomes.

## Build & test

```sh
cargo build                       # debug build
cargo build --release             # release build
cargo test                        # unit + integration + doctests
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo test --no-default-features
cargo doc --no-deps               # with RUSTDOCFLAGS=-D warnings in CI
```

## Testing manually

```sh
# Terminal 1: start UDP echo server
cargo run --release -- server 127.0.0.1:5000

# Terminal 2: run client, optionally with pipeline diagnostics on stderr
cargo run --release -- -p udp -A -n 5 127.0.0.1:5000
cargo run --release -- -v -p udp -n 5 127.0.0.1:5000

# ICMP (no server needed)
cargo run --release -- 8.8.8.8

# TCP
cargo run --release -- -p tcp 127.0.0.1:5000

# HTTPS
cargo run --release -- --protocol=http https://example.com
```

## Known issues / TODOs

- The statistics actor still exits promptly when the receiver is aborted, so
  pending timeout events may be skipped after the transport tasks end.
- No jitter metric; `std_dev` is the only dispersion value (printed as `mdev`).
