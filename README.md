# rup — universal pinger

`rup` measures round-trip time (RTT) over **UDP**, **TCP**, or **ICMP**.
It can be used as a CLI tool or as a Rust library.

## Quick Start

```sh
# Start a UDP echo server
rup server 0.0.0.0:5000

# Ping it from another terminal
rup client 127.0.0.1:5000

# Same over TCP. -p/--protocol is a root option, so place it before the subcommand.
rup -p tcp server 0.0.0.0:5000
rup -p tcp client 127.0.0.1:5000

# ICMP ping. No rup server is needed.
rup -p icmp client 8.8.8.8
```

## CLI Usage

```text
rup [OPTIONS] <COMMAND>

Commands:
  client  Send requests to the remote side and measure RTT
  server  Receive requests and send them back immediately

Options:
  -p, --protocol <protocol>  udp, tcp, or icmp [default: udp]
```

`-p` / `--protocol` is defined at the root level. Put it before `client` or
`server`.

### Client

```sh
rup [OPTIONS] client [CLIENT_OPTIONS] <remote-address>
```

| Option | Description |
|--------|-------------|
| `-i`, `--interval <ms>` | Interval between pings in milliseconds [default: 1000] |
| `-A`, `--adaptive-interval` | Send the next ping after a response or timeout |
| `-n`, `--ping-number <n>` | Number of pings to send |
| `-t`, `--run-time <sec>` | Run duration limit in seconds |
| `-W`, `--wait-time <ms>` | Response timeout in milliseconds [default: 1000] |
| `--request-size <bytes>` | Request packet size, minimum 12 bytes |
| `--response-size <bytes>` | Response packet size, minimum 12 bytes |
| `--local-address <addr>` | Local bind address [default: `0.0.0.0:0`] |

Examples:

```sh
# Adaptive mode, 10 pings over UDP
rup client -A -n 10 127.0.0.1:5000

# TCP with custom request size
rup -p tcp client example.com:5000 --request-size 64

# ICMP via hostname. Port is not required.
rup -p icmp client google.com

# 5-second burst with 50 ms interval
rup client -i 50 -t 5 127.0.0.1:5000
```

### Server

```sh
rup [OPTIONS] server <local-address>
```

UDP and TCP use a `rup` echo server. ICMP does not: the remote kernel responds
to echo requests directly.

```sh
rup server 0.0.0.0:5000
rup -p tcp server 0.0.0.0:5000
```

## Address Format

- **UDP/TCP** require `host:port`, for example `127.0.0.1:5000` or
  `example.com:5000`.
- **ICMP** accepts a hostname or IP address without a port, for example
  `8.8.8.8` or `google.com`.
- IPv6 socket addresses should use brackets when a port is present, for example
  `[::1]:5000`.

## Output

```text
seq=0 time=7.800 ms
seq=1 time=7.650 ms

--- statistics ---
2 requests sent, 2 received, 0% loss
min/med/avg/max = 7.650 ms / 7.800 ms / 7.725 ms / 7.800 ms
std_dev = 75.000 µs
```

## Library Usage

Add `rup` as a dependency:

```toml
[dependencies]
rup = { git = "https://github.com/svart/rup" }
```

Use the high-level builder for simple sessions:

```rust
use rup::Pinger;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let report = Pinger::new("127.0.0.1:5000", "udp")
        .count(5)
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

For lower-level integration, use `PingConfig` and `Protocol`:

```rust
use rup::{PingConfig, Protocol, run_ping_session};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut config = PingConfig::new("127.0.0.1:5000".to_string(), Protocol::Udp);
    config.ping_number = Some(5);

    let report = run_ping_session(config).await?;
    println!("received {} replies", report.received);

    Ok(())
}
```

The library also exposes the building blocks used by the CLI:
`Transport`, `UdpClientTransport`, `TcpClientTransport`, `IcmpClientTransport`,
`generator`, `transmitter`, `receiver`, and `statista`.

## Architecture

```text
generator ──Request──> transmitter ──StatEntry::Open──> statista ──PingRTT──> presenter
                          │                                    ^
                     send()│                              recv()
                          │                                    │
                     ┌────┴────┐   StatEntry::Close ───────────┘
                     │Transport│
                     └────┬────┘
                          │
              ┌───────────┼───────────┐
              │           │           │
             UDP         TCP         ICMP
```

- `generator` creates request IDs at a fixed interval or adaptively.
- `transmitter` sends packets and records send timestamps.
- `receiver` receives responses and records receive timestamps.
- `statista` matches send/receive entries, tracks timeouts, and computes RTTs.
- The CLI uses a presenter for live output. Library calls return `PingReport`
  without printing.
- `echo_codec` owns the shared UDP/TCP echo framing. ICMP adds its own ICMP
  header around the same echo payload.

## Requirements

- Rust edition 2024.
- ICMP support currently targets Linux ping sockets
  (`SOCK_DGRAM | IPPROTO_ICMP`), so root is not required when
  `net.ipv4.ping_group_range` allows the current user.

If ICMP fails with `EACCES`, configure ping sockets:

```sh
sudo sysctl -w net.ipv4.ping_group_range='0 2147483647'
```

## Build And Test

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
./target/release/rup --help
```

## License

Licensed under either of [Apache 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT) at your option.
