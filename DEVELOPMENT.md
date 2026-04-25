# Development guide for rup

## Testing harness

Tests are written as `#[cfg(test)] mod tests` blocks inside each source file
(not in a separate `tests/` directory). This follows Rust convention and keeps
tests close to the code they exercise.

### Test infrastructure

**MockTransport** (`src/transport/mod.rs` tests) — a `Transport` trait
implementation that simulates network send/recv without real sockets:

```rust
struct MockTransport {
    send_delay: Duration,
    recv_responses: Vec<Response>,
    recv_index: Arc<Mutex<usize>>,
}
```

- `send()` always succeeds after `send_delay` and returns `Instant::now()`
- `recv()` returns the next response from `recv_responses` in order, or
  `io::Error` when exhausted
- `Clone` is implemented (shared `recv_index` via `Arc`)

**ErrorTransport** (`src/transport/mod.rs` tests) — always returns errors on
both `send()` and `recv()`, used to test error propagation in transmitter and
receiver adapters.

### Running tests

```sh
cargo test                # all 104+ tests
cargo test -- --nocapture # show stdout (timeout/reorder messages, stats)
cargo test <name>         # single test, e.g. cargo test generator_adaptive_mode
cargo clippy              # zero warnings required
```

### Test organization by module

| File | Tests | What they cover |
|------|-------|-----------------|
| `src/pinger.rs` | 17 | `Echo` serialization (zero/max/field-order/invalid sizes), `Request` channel rounds trip, `generator` (interval/adaptive modes, ping-number limit, channel-close exit, adaptive signal-flow), `SendMode` creation |
| `src/statistics.rs` | 24 | `RttSequence` (mean/median/std-dev/loss/edge-cases), `fmt_duration` boundaries, entry-matching algorithm (exact/reorder/skip-lost/put-back/empty/idempotent), `receive_timeout` (entry removal/up-to-index/signal-generator/empty-queue) |
| `src/transport/mod.rs` | 11 | `transmitter` (single/multiple/sizes/send-error/channel-close/backpressure), `receiver` (single/multiple/recv-error/channel-close) |
| `src/transport/async_icmp.rs` | 14 | Checksum (`csum16_add` wrap/no-wrap/both-max, `csum16_slice` even/odd/empty/single-byte), ICMP packet assembly (v4 header/v4 checksum-verify/v6 type/payload-offset/min-size/variable-size), `is_ipv6` detection |
| `src/main.rs` | 17 | `has_port` (v4/v6/hostname/empty/multi-colons/non-numeric), `ensure_port` (preserves existing, appends `:0` for ICMP) |
| `src/cli.rs` | 12 | CLI argument parsing: defaults, all-options, adaptive-mode, interval-vs-adaptive conflict, req-size validation (>12), server, protocol flags (udp/tcp/icmp/default/invalid), missing-address error |

### Key testing patterns

**Async generator tests** use `#[tokio::test]` and channel-based interaction:

```rust
#[tokio::test]
async fn generator_incrementing_ids() {
    let (tx, mut rx) = mpsc::channel(8);
    tokio::spawn(generator(tx, SendMode::Interval(1), Some(3), None, None, None));
    let mut ids = Vec::new();
    for _ in 0..3 { ids.push(rx.recv().await.unwrap().id); }
    assert_eq!(ids, vec![0, 1, 2]);
}
```

**Entry-matching tests** directly manipulate `VecDeque<Entry>` to validate the
statista matching algorithm without spawning tasks:

```rust
let mut requests = VecDeque::new();
requests.push_back(Entry { id: 0, ts: Instant::now() });
// ... simulate Close handling with Ordering::cmp ...
```

**Timeout tests** spawn `receive_timeout()` with minimal wait durations
(1 ms) so they complete quickly without `test-util` feature.

**ICMP packet tests** build raw packets manually and verify header bytes,
checksum correctness, and payload deserialization — no socket needed.

## Code coverage

### Prerequisites

```sh
cargo install cargo-llvm-cov
```

### Measuring coverage

```sh
# Run tests with coverage instrumentation
cargo llvm-cov

# Generate HTML report (opens in browser)
cargo llvm-cov --open

# View coverage summary per file
cargo llvm-cov --summary-only
```

`cargo-llvm-cov` uses LLVM's source-based code coverage
(`-Cinstrument-coverage`) which is the official Rust coverage tool. It
tracks which lines and branches are exercised by tests, including
condition/decision coverage.

### Current coverage

| File | Lines | Regions | Notes |
|------|-------|---------|-------|
| `src/pinger.rs` | 97.74% | 98.21% | Echo serialization + generator paths |
| `src/statistics.rs` | 82.54% | 83.76% | Matching logic, RttSequence stats, timeouts |
| `src/transport/mod.rs` | 93.03% | 94.52% | Transmitter/receiver adapters, mocks |
| `src/transport/async_icmp.rs` | 64.91% | 65.98% | Packet assembly + checksum tested; I/O paths need ICMP socket |
| `src/transport/async_tcp.rs` | 65.68% | 68.98% | Client send/recv tested with real sockets; server loop untested |
| `src/transport/async_udp.rs` | 69.90% | 70.03% | Client send/recv tested with real sockets; server loop untested |
| `src/cli.rs` | 89.50% | 87.64% | Argument parsing; `get_cli_params()` uses `std::process::exit` |
| `src/main.rs` | 46.30% | 49.00% | `has_port`/`ensure_port`/`spawn_tasks` tested; DNS + wiring not tested |
| **Total** | **77.52%** | **77.89%** | |

### Uncovered areas

- **ICMP I/O paths** — `IcmpClientTransport::send()`/`recv()` require
  `CAP_NET_RAW` or `ping_group_range` sysctl. The packet assembly and
  checksum logic IS tested via packet-level unit tests.
- **TCP/UDP server loops** — `server_transport()` functions are infinite
  loops with `tokio::select!` (listening + ctrl-c). Exercised manually.
- **main.rs wiring** — DNS resolution (`tokio::net::lookup_host`) and
  protocol dispatch (`match protocol.as_str()`) require real network or
  are tightly coupled to `main()`.
- **CLI `get_cli_params()`** — calls `std::process::exit(1)` for invalid
  input, which terminates the process and cannot be caught in tests.
