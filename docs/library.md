# Library API

Add `rup` as a dependency:

```toml
[dependencies]
rup = { git = "https://github.com/svart/rup" }
```

## High-Level Builder

Use `Pinger` for simple sessions:

```rust
use rup::Pinger;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let report = Pinger::new("127.0.0.1:5000", "udp")
        .count(5)
        .tos(184)
        .interval(1000)
        .run()
        .await?;

    println!(
        "sent={}, received={}, loss={:.0}%",
        report.sent,
        report.received,
        report.loss_pct(),
    );

    Ok(())
}
```

Available builder methods:

| Method | Effect |
|--------|--------|
| `count(n)` | Stop after `n` requests |
| `interval(ms)` | Send at a fixed millisecond interval |
| `adaptive()` | Send the next request after a response or timeout |
| `wait_time(ms)` | Set response timeout |
| `request_size(bytes)` | Set request payload size |
| `response_size(bytes)` | Ask UDP/TCP server for a response size |
| `tos(byte)` | Set outgoing IP TOS / IPv6 traffic class byte |
| `local(addr)` | Bind to a local socket address |
| `run_time(duration)` | Stop after a duration |

## Structured Sessions

Use `PingConfig` when callers already have typed protocol and duration values:

```rust
use rup::{PingConfig, Protocol, run_ping_session};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut config = PingConfig::new("127.0.0.1:5000".to_string(), Protocol::Udp);
    config.ping_number = Some(5);
    config.tos = Some(184);

    let report = run_ping_session(config).await?;
    println!("received {} replies", report.received);

    Ok(())
}
```

`run_ping_session()` returns a quiet `PingReport`. `run_ping_session_with_output()`
uses the same session logic and prints live CLI-style output.

`PingConfig::tos` is the full TOS / traffic class byte. UDP servers reflect the
received byte on echo responses when available from the operating system.

## Report Values

`PingReport` contains raw RTTs and counters:

```rust
pub struct PingReport {
    pub rtts: Vec<Duration>,
    pub sent: u64,
    pub received: u64,
}
```

Convenience methods compute `loss_pct()`, `min()`, `max()`, `mean()`,
`median()`, and `std_dev()`.

## Building Blocks

The library also exposes the pipeline parts used by the CLI:

- `Transport`
- `UdpClientTransport`, `TcpClientTransport`, `IcmpClientTransport`
- `generator()`
- `transmitter()`
- `receiver()`
- `statista()` and `statista_with_collector()`
- `Request`, `Response`, `Entry`, `StatEntry`, `SendMode`
- `Echo`, `PING_HDR_LEN`
- `RttSequence`
- `has_port()` and `ensure_port()`

Library functions return `std::io::Result`; they do not terminate the process.
