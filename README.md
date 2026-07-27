# rup

Universal RTT measurement over UDP, TCP, and ICMP. `rup` is both a CLI tool and
a Rust library.

## Quick Start

```sh
# ICMP is the default client protocol and does not need a rup server.
rup -n 5 8.8.8.8

# UDP uses a rup echo server when available. On Linux, a matching ICMP error
# (for example, port unreachable) is also measured as a reply.
rup server 127.0.0.1:5000
rup -p udp -n 5 127.0.0.1:5000

# Emit rup.ping JSON Lines version 3 with absolute UTC timestamps.
rup --output jsonl -p udp -n 5 127.0.0.1:5000

# Customize human-readable reply records.
rup 127.0.0.1 --format '{ip}: {seq} => {rtt}'

# Set outgoing IP TOS / IPv6 traffic class.
rup -p udp --tos 184 -n 5 127.0.0.1:5000

# TCP uses rup echo when the first connection succeeds. Otherwise it measures
# each TCP connect outcome (including RST and ICMP-derived errors).
rup -p tcp server 127.0.0.1:5000
rup -p tcp -n 5 127.0.0.1:5000
```

UDP/TCP addresses require `host:port`. ICMP accepts `host` or `host:port`; the
port is ignored.

Terminal network responses are included in received packets and RTT statistics.
Human output labels them as terminal responses; JSONL uses `terminal_reply`
records with a payload size of zero.

Press Ctrl+C during a client session to stop sending new requests and print the
final packet-loss and RTT summary.

`--tos <0-255>` sets the full outgoing TOS / traffic class byte for client
packets. UDP servers reflect the received byte on echo responses when the
platform exposes it.

## Library

```toml
[dependencies]
rup = { git = "https://github.com/svart/rup" }
```

```rust
use rup::{Pinger, Protocol};

# async fn example() -> std::io::Result<()> {
let report = Pinger::new("127.0.0.1:5000".parse().unwrap(), Protocol::Udp)
    .count(5)
    .tos(184)
    .interval(1000)
    .run()
    .await?;

println!("sent={}, received={}, loss={:.0}%", report.sent, report.received, report.loss_pct());
# Ok(())
# }
```

## Commands

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

## Docs

- [CLI usage](docs/cli.md)
- [Library API](docs/library.md)
- [Architecture](docs/architecture.md)
- [Development guide](docs/development.md)

## Requirements

Rust edition 2024. ICMP currently targets Linux ping sockets
(`SOCK_DGRAM | IPPROTO_ICMP`); if ICMP returns `EACCES`, allow ping sockets with
`sudo sysctl -w net.ipv4.ping_group_range='0 2147483647'`.

## License

Licensed under either [Apache 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT).
