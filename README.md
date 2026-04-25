# rup — universal pinger

Measure round-trip time (RTT) between two endpoints over **UDP**, **TCP**, or **ICMP**.

## Quick start

```sh
# Start a UDP echo server
rup server 0.0.0.0:5000

# Ping it from another terminal
rup client 127.0.0.1:5000

# Same over TCP
rup server -p tcp 0.0.0.0:5000
rup client -p tcp 127.0.0.1:5000

# ICMP ping (no server needed, no root required)
rup -p icmp client 8.8.8.8
```

## Client options

| Flag | Description |
|------|-------------|
| `-p` `--protocol` | Protocol: `udp` (default), `tcp`, `icmp` |
| `-i` `--interval` | Interval in ms between pings (default 1000) |
| `-A` `--adaptive-interval` | Send next ping immediately on response |
| `-n` `--ping-number` | Number of pings to send (default: infinite) |
| `-t` `--run-time` | Run duration limit in seconds |
| `-W` `--wait-time` | Response timeout in ms (default 1000) |
| `--request-size` | Request payload size in bytes (min 12) |
| `--response-size` | Response payload size in bytes (min 12) |
| `--local-address` | Bind address (default `0.0.0.0:0`) |

### Examples

```sh
# Adaptive mode, 10 pings over UDP
rup client -A -n 10 127.0.0.1:5000

# DNS name with custom request/response size
rup client -p tcp example.com:5000 --request-size 64

# ICMP via hostname (port not needed for ICMP)
rup -p icmp client google.com

# 5-second burst with 50ms interval
rup client -i 50 -t 5 127.0.0.1:5000
```

## Server options

| Flag | Description |
|------|-------------|
| `-p` `--protocol` | Protocol: `udp` (default), `tcp` |

ICMP has no server — the remote kernel handles echo replies directly.

## Address format

- **TCP / UDP**: `host:port` (e.g. `127.0.0.1:5000`, `example.com:5000`)
- **ICMP**: hostname or IP only, port is ignored (`8.8.8.8`, `google.com`)

## Output

```
seq=0 time=7.80 ms
seq=1 time=7.65 ms

--- statistics ---
2 requests sent, 2 received, 0% loss
min/med/avg/max = 7.650 ms / 7.800 ms / 7.725 ms / 7.800 ms
std_dev = 75.000 µs
```

## Architecture

```
generator ──Request──▶ transmitter ──StatEntry::Open──▶ statista ──PingRTT──▶ presenter
                          │                                    ▲
                     send()│                              recv()
                          │                                    │
                     ┌────┴────┐   StatEntry::Close ───────────┘
                     │ Transport │
                     │  trait    │
                     └────┬────┘
                    ┌─────┼─────┐
                    │     │     │
               UdpClient TcpClient IcmpClient
               Transport Transport Transport
```

- **generator** — produces `Request` messages at a fixed interval or adaptively
- **transmitter** — calls `Transport::send()`, sends `StatEntry::Open` to statista
- **receiver** — calls `Transport::recv()`, sends `StatEntry::Close` to statista
- **statista** — matches Open/Close pairs by ID, measures RTT, tracks timeouts
- **presenter** — prints live RTT and final statistics

## Requirements

- **Rust edition 2024** (stable toolchain)
- **ICMP**: Linux kernel with `net.ipv4.ping_group_range` configured (default on most distros).
  If ICMP fails with EACCES: `sudo sysctl -w net.ipv4.ping_group_range='0 2147483647'`

## Build & test

```sh
cargo build
cargo test
cargo build --release
./target/release/rup --help
```

## License

Licensed under either of [Apache 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT) at your option.
