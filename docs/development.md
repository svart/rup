# Development Guide

`rup` is a single Rust crate with a library target and a CLI target. Most
behavior lives in the library; `src/main.rs` is intentionally thin.

## Source Map

| File | Purpose |
|------|---------|
| `src/lib.rs` | Public API, `Pinger`, `PingConfig`, session orchestration, server dispatch |
| `src/protocol.rs` | `Protocol` enum, parsing, display names, CLI value list |
| `src/echo_codec.rs` | Shared UDP/TCP echo payload encoding and decoding |
| `src/pinger.rs` | Request/response domain types, `Echo`, `SendMode`, request generator |
| `src/statistics.rs` | RTT matching, timeout handling, live event emission, shared statistics helpers |
| `src/transport/mod.rs` | `Transport` trait plus transmitter/receiver adapters |
| `src/transport/async_udp.rs` | UDP client transport and UDP echo server |
| `src/transport/async_tcp.rs` | TCP client transport and TCP echo server |
| `src/transport/async_icmp.rs` | ICMP client transport using Linux ping sockets |
| `src/cli.rs` | Clap command definition and CLI parameter extraction |
| `src/main.rs` | Runtime setup and calls into library entry points |

## Testing

Tests live next to the code they exercise in `#[cfg(test)]` modules.

Run the normal validation set before committing:

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

Current suite size is 145 library tests, 22 binary tests, and 1 doctest.

| File | What the tests cover |
|------|----------------------|
| `src/pinger.rs` | `Echo` serialization, request generation, interval/adaptive modes, channel shutdown |
| `src/statistics.rs` | RTT statistics, formatting, entry matching, timeout helper behavior |
| `src/transport/mod.rs` | Transmitter/receiver channel adapters, mock transports, error paths |
| `src/transport/async_udp.rs` | Echo encoding/parsing and loopback UDP send/receive |
| `src/transport/async_tcp.rs` | Echo encoding/parsing and loopback TCP send/receive |
| `src/transport/async_icmp.rs` | ICMP packet assembly, checksums, response parsing, loopback ping when available |
| `src/cli.rs` | CLI defaults, validation, protocol parsing, server subcommand arguments |
| `src/lib.rs` | Address helpers, high-level report statistics, session orchestration |

Some tests bind loopback sockets. In restricted sandboxes they may fail with
`PermissionDenied`; run them outside the sandbox when validating real socket
behavior.

## Manual Testing

```sh
# Terminal 1
cargo run --release -- server 127.0.0.1:5000

# Terminal 2
cargo run --release -- -p udp -A -n 5 127.0.0.1:5000

# TCP
cargo run --release -- -p tcp server 127.0.0.1:5000
cargo run --release -- -p tcp 127.0.0.1:5000

# Serverless terminal replies (choose currently unused local ports)
cargo run --release -- -p udp -n 3 127.0.0.1:59998
cargo run --release -- -p tcp -n 3 127.0.0.1:59999

# ICMP
cargo run --release -- 8.8.8.8
```

Remember that `-p` is a root CLI option. For server mode, it must appear before
the `server` subcommand.

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
- Linux UDP can match remote ICMP errors through `MSG_ERRQUEUE`; TCP high-level
  sessions fall back to per-request connect probes after an initial connection
  error.
