# rup

Universal RTT measurement over UDP, TCP, and ICMP. `rup` is both a CLI tool and
a Rust library.

## Quick Start

```sh
# UDP needs a rup echo server.
rup server 127.0.0.1:5000
rup client -n 5 127.0.0.1:5000

# TCP uses the same command shape. -p is a root option.
rup -p tcp server 127.0.0.1:5000
rup -p tcp client -n 5 127.0.0.1:5000

# ICMP does not need a rup server.
rup -p icmp client -n 5 8.8.8.8
```

UDP/TCP addresses require `host:port`. ICMP accepts `host` or `host:port`; the
port is ignored.

## Library

```toml
[dependencies]
rup = { git = "https://github.com/svart/rup" }
```

```rust
use rup::Pinger;

# async fn example() -> std::io::Result<()> {
let report = Pinger::new("127.0.0.1:5000", "udp")
    .count(5)
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
