# Agent guide for rup

## Project overview

`rup` is a universal pinger — a client-server tool that measures RTT over UDP,
TCP, and ICMP. Written in Rust (edition 2024) with tokio async runtime.

## Source map

| File | Purpose |
|------|---------|
| `src/main.rs` | Entry point, wires tasks, DNS resolution, protocol dispatch |
| `src/cli.rs` | CLAP argument definitions and parsing |
| `src/pinger.rs` | Domain types (`Request`, `Response`, `Entry`, `StatEntry`, `Echo`, `SendMode`) + generator task |
| `src/statistics.rs` | RTT matching, timeout watchers, presenter, RttSequence stats |
| `src/transport/mod.rs` | `Transport` trait + `transmitter()`/`receiver()` adapter functions |
| `src/transport/async_udp.rs` | `UdpClientTransport` + UDP echo server |
| `src/transport/async_tcp.rs` | `TcpClientTransport` + TCP echo server |
| `src/transport/async_icmp.rs` | `IcmpClientTransport` (Linux ping socket, no root needed) |

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
               UdpClient TcpClient IcmpClient
```

### Data flow

1. **Generator** produces `Request` messages (with id, sizes) — either at a fixed
   interval or adaptively (immediately on receiving a response signal from statista).
2. **Transmitter** takes each `Request`, calls `Transport::send()`, records the
   send timestamp, and sends `StatEntry::Open` to statista.
3. **Receiver** calls `Transport::recv()`, and sends `StatEntry::Close` to statista.
4. **Statista** matches Open/Close entries by ID, computes RTT, forwards `PingRTT`
   to presenter. Spawns timeout watchers for each Open entry.
5. **Presenter** prints live RTT lines and final min/med/avg/std_dev/max statistics
   with packet loss percentage.

### Transport trait

```rust
pub(crate) trait Transport {
    async fn send(self: &Self, req: &Request) -> io::Result<Instant>;
    async fn recv(self: &Self) -> io::Result<Response>;
}
```

Each protocol implements this trait. The `transmitter()` and `receiver()` adapter
functions in `transport/mod.rs` handle channel wiring — new protocols only need
to implement this trait.

Transport types must be `Clone` (they use `Arc` internally) since `transmitter`
and `receiver` run as separate tasks sharing the same transport.

## Key patterns

### Error handling
All errors return `io::Result`. Transport implementations never `panic!` or
`unwrap()` on network errors. The `transmitter`/`receiver` adapters log errors
to stderr and exit gracefully.

### Actor model
Each component runs as a `tokio::spawn`ed task. Communication is via
`tokio::sync::mpsc` channels. The `tokio::select!` macro is used throughout
for multiplexing channels, timers, and signals.

### ICMP (no root)
Uses `SOCK_DGRAM | IPPROTO_ICMP` (Linux ping socket) instead of `SOCK_RAW`.
The kernel handles ICMP identifier assignment and matching. User constructs the
ICMP header (type=8, checksum, identifier placeholder, sequence) + Echo payload.
On receive, the kernel strips the IP header; we parse the ICMP header + Echo
payload from the datagram buffer.

### Packet format (wire)
```
[ICMP header (8)] [Echo struct (12+)]
```
Where `Echo` is bincode-serialized: `{id: u64, len: u16, resp_size: u16}`.
`PING_HDR_LEN = 12` (sum of field sizes, not `size_of::<Echo>()` which is 16
due to alignment padding).

### CLI (`-p` flag is global)
```
rup [global opts] <subcommand> [subcommand opts]
```
`-p`/`--protocol` is defined at the root level (not inside subcommands), so it
must come before the subcommand: `rup -p icmp client 8.8.8.8`.

Protocol string is stored as `PingerParams.protocol` (owned `String`).

### Port handling
- TCP/UDP require port in address (`host:port`)
- ICMP ignores port; `ensure_port()` appends `:0` if missing before DNS resolution
- DNS resolution via `tokio::net::lookup_host()`

### Wire protocol (echo server)
On UDP/TCP, the server reads the `Echo` header, swaps `resp_size` into `len`,
zeroes `resp_size`, and sends back `len` bytes padded with zeros. This allows
variable-length responses.

## Build & test

```sh
cargo build                # debug build
cargo build --release      # release build (test with this)
cargo test                 # 14 unit tests
cargo clippy               # must pass before committing (zero warnings)
```

## Testing manually

```sh
# Terminal 1: start UDP echo server
cargo run --release -- server 127.0.0.1:5000

# Terminal 2: run client
cargo run --release -- client -A -n 5 127.0.0.1:5000

# ICMP (no server needed)
cargo run --release -- -p icmp client 8.8.8.8

# TCP
cargo run --release -- -p tcp client 127.0.0.1:5000
```

## Known issues / TODOs

- `statista` exits immediately when receiver is aborted — timeout watchers may
  fire after presenter has already printed final stats (cosmetic, unused entries
  are silently dropped)
- No jitter/mean deviation in statistics (only std_dev)
- `Response.size` field was removed; packet size statistics not tracked
