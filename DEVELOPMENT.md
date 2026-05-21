# Development Guide

`rup` is a single Rust crate with a library target and a CLI target.
Most behavior lives in the library; `src/main.rs` is intentionally thin.

## Source Map

| File | Purpose |
|------|---------|
| `src/lib.rs` | Public API, `Pinger`, `PingConfig`, session orchestration, server dispatch |
| `src/protocol.rs` | `Protocol` enum, parsing, display names, CLI value list |
| `src/echo_codec.rs` | Shared UDP/TCP echo payload encoding and decoding |
| `src/pinger.rs` | Request/response domain types, `Echo`, `SendMode`, request generator |
| `src/statistics.rs` | RTT matching, timeout handling, live presenter, shared statistics helpers |
| `src/transport/mod.rs` | `Transport` trait plus transmitter/receiver adapters |
| `src/transport/async_udp.rs` | UDP client transport and UDP echo server |
| `src/transport/async_tcp.rs` | TCP client transport and TCP echo server |
| `src/transport/async_icmp.rs` | ICMP client transport using Linux ping sockets |
| `src/cli.rs` | Clap command definition and CLI parameter extraction |
| `src/main.rs` | Runtime setup and calls into library entry points |

## Architecture

The client pipeline is actor-like. Each stage communicates over Tokio channels:

```text
generator -> transmitter -> statista -> presenter or collector
                 |              ^
                 v              |
              Transport -> receiver
```

The transport implementations only need to implement:

```rust
pub trait Transport: Send + Sync {
    async fn send(&self, req: &Request) -> io::Result<Instant>;
    async fn recv(&self) -> io::Result<Response>;
}
```

`run_ping_session()` builds the quiet library path and returns `PingReport`.
`run_ping_session_with_output()` adds the live CLI presenter. Both paths share
the same matcher and timeout logic.

## Public API Shape

The high-level API is:

```rust
let report = rup::Pinger::new("127.0.0.1:5000", "udp")
    .count(5)
    .run()
    .await?;
```

The structured API is:

```rust
let mut config = rup::PingConfig::new("127.0.0.1:5000".to_string(), rup::Protocol::Udp);
config.ping_number = Some(5);
let report = rup::run_ping_session(config).await?;
```

The library must not terminate the process. Invalid user input should return
`io::Result` errors from library functions and be printed by the CLI layer.

## Adding A Protocol

1. Add a transport implementation in `src/transport/async_<proto>.rs`.
2. Implement `Transport` for the client transport type.
3. Add the module to `src/transport/mod.rs`.
4. Add a variant to `Protocol` in `src/protocol.rs`.
5. Register client construction in `run_ping_session_inner()` in `src/lib.rs`.
6. Register server support in `run_server()` if the protocol needs a `rup`
   echo server.
7. Add parser, packet, and transport tests.

For protocols that share the existing echo payload, use `echo_codec`.

## Testing

Tests live next to the code they exercise in `#[cfg(test)]` modules.

Run the normal validation set before committing:

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

Current suite size is 143 library tests, 17 binary tests, and 1 doctest.

### Test Coverage By Area

| File | What the tests cover |
|------|----------------------|
| `src/pinger.rs` | `Echo` serialization, request generation, interval/adaptive modes, channel shutdown |
| `src/statistics.rs` | RTT statistics, formatting, entry matching, timeout helper behavior |
| `src/transport/mod.rs` | Transmitter/receiver channel adapters, mock transports, error paths |
| `src/transport/async_udp.rs` | Echo encoding/parsing and loopback UDP send/receive |
| `src/transport/async_tcp.rs` | Echo encoding/parsing and loopback TCP send/receive |
| `src/transport/async_icmp.rs` | ICMP packet assembly, checksums, response parsing, loopback ping when available |
| `src/cli.rs` | CLI defaults, validation, protocol parsing, subcommand arguments |
| `src/lib.rs` | Address helpers, high-level report statistics, unknown protocol errors |

Some tests bind loopback sockets. In restricted sandboxes they may fail with
`PermissionDenied`; run them outside the sandbox when validating real socket
behavior.

## Manual Testing

```sh
# Terminal 1
cargo run --release -- server 127.0.0.1:5000

# Terminal 2
cargo run --release -- client -A -n 5 127.0.0.1:5000

# TCP
cargo run --release -- -p tcp server 127.0.0.1:5000
cargo run --release -- -p tcp client 127.0.0.1:5000

# ICMP
cargo run --release -- -p icmp client 8.8.8.8
```

Remember that `-p` is a root CLI option and must appear before the subcommand.

## Coverage

Install:

```sh
cargo install cargo-llvm-cov
```

Run:

```sh
cargo llvm-cov
cargo llvm-cov --summary-only
cargo llvm-cov --open
```

Coverage numbers change as the test suite evolves, so avoid committing static
coverage percentages unless they were just regenerated.

## Notes

- TCP/UDP require `host:port`; ICMP accepts hosts without a port.
- Server transports return `io::Result<()>` for setup failures such as bind
  errors, but continue past per-client/per-packet errors where possible.
- TCP and UDP use the shared echo payload format. ICMP wraps that payload in an
  ICMP header and checksum handling.
