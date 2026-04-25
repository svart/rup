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
cargo test                # all 133+ tests
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
| `src/transport/async_icmp.rs` | 93.07% | 92.70% | Extracted `build_icmp_packet`/`try_parse_icmp_response` + loopback integration |
| `src/transport/async_tcp.rs` | 69.16% | 71.90% | Extracted `build_tcp_echo`/`parse_tcp_header` + real socket tests; server loop untested |
| `src/transport/async_udp.rs` | 76.86% | 78.32% | Extracted `build_udp_echo`/`parse_udp_response` + real socket tests; server loop untested |
| `src/cli.rs` | 89.50% | 87.64% | Argument parsing; `get_cli_params()` uses `std::process::exit` |
| `src/main.rs` | 46.30% | 49.00% | `has_port`/`ensure_port`/`spawn_tasks` tested; DNS + wiring not tested |
| **Total** | **81.81%** | **82.52%** | |

### Design for testability

Each transport's I/O and protocol logic is cleanly separated:

| Transport | Packet builder | Response parser | Test coverage |
|-----------|---------------|-----------------|---------------|
| ICMP | `build_icmp_packet(req, is_v6)` | `try_parse_icmp_response(buf, n, reply_type)` | ✓ unit tests for all branches + real ICMP ping to `127.0.0.1` |
| UDP | `build_udp_echo(req)` | `parse_udp_response(buf)` | ✓ unit tests for sizes/error/edge cases + real loopback socket |
| TCP | `build_tcp_echo(req)` | `parse_tcp_header(hdr)` | ✓ unit tests for sizes/edge cases + real loopback socket |

All three production `send()`/`recv()` methods call these extracted functions,
so the same logic is exercised by both unit tests and real I/O.

### Uncovered areas

- **TCP/UDP server loops** — `server_transport()` functions are infinite
  loops with `tokio::select!` (listening + ctrl-c). Exercised manually.
- **TCP/UDP I/O error paths** — `WouldBlock` branches (hard to trigger on
  loopback), timeout branches (need artificial delay).
- **main.rs wiring** — DNS resolution (`tokio::net::lookup_host`) and
  protocol dispatch (`match protocol.as_str()`) require real network or
  are tightly coupled to `main()`.
- **CLI `get_cli_params()`** — calls `std::process::exit(1)` for invalid
  input, which terminates the process and cannot be caught in tests.
