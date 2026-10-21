# Library API

Add `rup` as a dependency:

```toml
[dependencies]
rup = { git = "https://github.com/svart/rup" }
```

## High-Level Builder

Use `Pinger` for simple sessions:

```rust
use std::time::Duration;
use rup::{Pinger, Protocol, TrafficClass};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let report = Pinger::new("127.0.0.1:5000".parse().unwrap(), Protocol::Udp)
        .count(5)
        .traffic_class(TrafficClass::new(184))
        .interval(Duration::from_secs(1))
        .run()
        .await?;

    println!(
        "sent={}, received={}, loss={:.0}%",
        report.sent(),
        report.received(),
        report.loss_pct(),
    );

    Ok(())
}
```

Available builder methods:

| Method | Effect |
|--------|--------|
| `count(n)` | Stop after `n` requests |
| `interval(duration)` | Send at a fixed interval |
| `adaptive()` | Send the next request after a response or timeout |
| `timeout(duration)` | Set response timeout |
| `request_size(PacketSize)` | Set request payload size |
| `response_size(PacketSize)` | Ask UDP/TCP server for a response size |
| `traffic_class(TrafficClass)` | Set outgoing IP TOS / IPv6 traffic class byte |
| `local(addr)` | Bind to a local socket address |
| `run_time(duration)` | Stop after a duration |
| `http_target(HttpTarget)` | URL for `Protocol::Http1`/`Http2` [default: `http://<remote>/`] |

## Structured Sessions

Use `PingConfig` when callers already have a resolved socket address, typed
protocol, and duration values:

```rust
use rup::{PingConfig, Protocol, TrafficClass, run_ping_session};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let config = PingConfig::new("127.0.0.1:5000".parse().unwrap(), Protocol::Udp)
        .count(5)
        .traffic_class(TrafficClass::new(184));

    let report = run_ping_session(config).await?;
    println!("received {} replies", report.received());

    Ok(())
}
```

`run_ping_session()` returns a quiet `PingReport`.

Use `start_ping_session()` when callers need live results:

```rust
use rup::{PingConfig, PingEvent, Protocol, start_ping_session};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let config = PingConfig::new("127.0.0.1:5000".parse().unwrap(), Protocol::Udp)
        .count(5);

    let mut session = start_ping_session(config).await?;
    while let Some(event) = session.next().await {
        match event {
            PingEvent::Reply(result) => println!(
                "seq={} size={} ttl={:?} rtt={:?}",
                result.seq,
                result.size,
                result.ttl,
                result.rtt,
            ),
            PingEvent::Timeout { seq } => println!("seq={seq} timed out"),
            PingEvent::ReorderOrLoss { seq } => println!("seq={seq} lost or reordered"),
        }
    }

    let report = session.report().await?;
    println!("received {} replies", report.received());

    Ok(())
}
```

Call `PingSession::stop()` to stop generating new requests while allowing
already-sent requests to reply or time out. Continue consuming events until
`next()` returns `None`, then call `report()`. Calling `report()` also requests
a stop, so an unbounded session does not need a separate cancellation task.

`PingConfig::traffic_class()` accepts the full TOS / traffic class byte. UDP
servers reflect the received byte on echo responses when available from the
operating system.

High-level TCP and UDP sessions automatically accept terminal network replies.
Linux UDP matches remote ICMP errors through the socket error queue. TCP uses
rup application echo when its first connection succeeds and otherwise switches
to per-request connect probes. These outcomes arrive as ordinary
`PingEvent::Reply` values with `size == 0` and `ttl == None`.

For `Protocol::Http1` and `Protocol::Http2`, pass the resolved address of the
URL host as `remote` and the URL as `http_target`:

```rust,no_run
use rup::{HttpTarget, Pinger, Protocol, ensure_port};

# async fn example() -> std::io::Result<()> {
let url = "https://example.com/status";
let target: HttpTarget = url.parse().map_err(std::io::Error::other)?;
let remote = tokio::net::lookup_host(ensure_port(url, Protocol::Http2)?)
    .await?
    .next()
    .unwrap();
let report = Pinger::new(remote, Protocol::Http2)
    .http_target(target)
    .count(5)
    .run()
    .await?;
# Ok(())
# }
```

`http1` (alias `http`) and `http2` send `GET` to a URL and measure the time
until the full response body is read. HTTP/1.1 opens a new connection per
request, so its RTT includes TCP connect and the TLS handshake. HTTP/2 keeps
one persistent connection, multiplexes requests over it, and reconnects when
the server closes it. Before measuring, both send one unmeasured `GET`, so DNS,
TLS, and missing HTTP/2 support fail immediately with an error. Any HTTP status
counts as a reply; redirects are not followed. HTTPS verifies certificates
against the bundled Mozilla root store (`webpki-roots`).

`PingResult::received_at` is the monotonic `Instant` at which the transport
received the reply; the RTT ends at that instant. Subtract the `received_at`
of consecutive replies to get the inter-packet (inter-arrival) interval.

## Report Values

`PingReport` keeps its representation private so `received` cannot disagree
with the RTT samples. Use `sent()`, `received()`, and `rtts()` to inspect it.
Convenience methods compute `loss_pct()`, `min()`, `max()`, `mean()`,
`median()`, and `std_dev()`.

## Building Blocks

The public API intentionally exposes session-level operations and the minimum
needed to implement a custom transport:

- `Transport`
- `UdpTransport`, `TcpTransport`, `IcmpTransport`, and `HttpTransport` with the
  `http` feature
- `HttpTarget`, `HttpScheme`, `HttpVersion`
- `PingSession` and `PingEvent`
- `Request`, `Response`, `PacketSize`
- `has_port()` and `ensure_port()`

Generator, actor-message, statistics-tracker, and wire-header types are private
implementation details. Library functions return `std::io::Result`; they do
not terminate the process.
