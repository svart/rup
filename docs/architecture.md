# Architecture

The client path is an actor-style Tokio pipeline:

```text
generator --Request--> transmitter --sent--> tracker --PingEvent--> caller
                         |                    ^
                   send()|              recv()|
                         v                    |
                    Transport --received------
```

## Pipeline

1. `generator` creates `Request` values with sequence IDs and packet sizes.
2. `transmitter` calls `Transport::send()`, records the send timestamp, and
   forwards `StatEntry::Open`.
3. `receiver` calls `Transport::recv()` and forwards `StatEntry::Close`.
4. The statistics actor passes entries to a synchronous measurement tracker,
   which matches IDs, computes RTTs, tracks timeouts, and returns structured
   `PingEvent` values. Tokio-specific channel and timer work stays outside the
   deterministic tracker.
5. Callers consume `PingSession::next()` and decide how to present or process
   events. The CLI can render the same event stream as human-readable text or
   versioned JSON Lines. Completed sessions return a `PingReport`.

Fixed interval mode generates requests on a fixed schedule, so send overhead and
timer rounding do not accumulate; after a stall it resumes one interval later
instead of bursting. A request due exactly at the run-time limit is not sent.
Adaptive mode waits for `statista` to signal after either a response or timeout.

Diagnostics use `tracing`; the library never writes to stderr directly. The CLI
installs a stderr subscriber for the `rup` target at `WARN`, or `TRACE` with
`-v`, and the session pipeline nests `generator`, `transmitter`, `receiver`, and
`statistics` spans under a `session` span. Library callers can install their own
subscriber instead.

Live sessions own an explicit stop channel. The CLI translates Ctrl+C into a
session stop; the generator closes, the transmitter drains requests already
queued, and statista settles outstanding replies/timeouts before returning the
summary. Signal handling therefore remains outside reusable transport and
generator building blocks.

## Transport Trait

Transport implementations provide the network-specific work:

```rust
pub trait Transport: Send + Sync {
    async fn send(&self, req: &Request) -> io::Result<Instant>;
    async fn recv(&self) -> io::Result<Response>;
}
```

Transport values are cloned into transmitter and receiver tasks. Existing
implementations use `Arc` internally where sharing is needed.

Because terminal connect results can be available before the transmitter has
forwarded its send timestamp, statista buffers early close entries until the
matching open entry arrives.

## Protocols

UDP and TCP prefer a `rup` echo server. The server reads the shared `Echo`
payload, swaps `resp_size` into `len`, clears `resp_size`, and sends a padded
response of the requested size.

On Linux, UDP also enables `IP_RECVERR` or `IPV6_RECVERR`. The receiver drains
`MSG_ERRQUEUE`, verifies that an error originated from remote ICMP, and decodes
the original UDP payload supplied by the kernel to recover the sequence ID.
This makes an ICMP error a zero-size response without requiring a raw socket.

High-level TCP sessions defer their first connection until sequence zero. A
successful connection selects the persistent application-echo path. A completed
connection error selects connect-probe mode, where every later request creates
a bounded connection attempt and treats either success or a completed error as
a zero-size response. A connect timeout produces no response.

Client sessions can set a full IP TOS / IPv6 traffic class byte. UDP servers
receive packet TOS/TCLASS metadata with `recvmsg` where the platform supports
it, then apply that byte to the echo response. TCP and ICMP support client-side
outgoing TOS; there is no TCP or ICMP server-side reflection path.

HTTP transports (`src/transport/async_http.rs`) run every request in its own
task: `send()` records the start time and spawns the exchange, and the task
queues a `Response` once the full body is read. HTTP/1.1 opens a new connection
per exchange; HTTP/2 shares one `SendRequest` handle and replaces it after
GOAWAY or a closed connection. A failed or timed-out exchange produces no
response, so statista reports it as lost. Sockets set `TCP_NODELAY`, so that
small HTTP/2 control frames do not delay requests through Nagle's algorithm.
The URL type (`src/http_target.rs`) has no dependencies and stays available
without the `http` feature.

ICMP uses Linux ping sockets (`SOCK_DGRAM | IPPROTO_ICMP`) rather than raw
sockets. The kernel handles identifier assignment; `rup` builds the ICMP header
and embeds the same echo payload after it.

## Packet Format

UDP/TCP packets contain a fixed-width little-endian header plus optional zero
padding:

```text
Echo { id: u64, len: u16, resp_size: u16 } [padding...]
```

ICMP packets wrap that payload:

```text
[ICMP header: 8 bytes] [Echo payload: 12 bytes minimum] [padding...]
```

The header is 12 bytes: eight bytes for the sequence ID and two bytes each for
request and response lengths. Encoding is manual and does not depend on Rust
struct layout.

## Optional Transports

A transport with heavy dependencies sits behind a cargo feature. The only
feature-gated `cfg` attributes are the module selection in
`src/transport/mod.rs` and the public re-export in `src/lib.rs` (platform
`cfg`s around socket extensions are separate):

```rust
#[cfg_attr(not(feature = "http"), path = "disabled/async_http.rs")]
pub mod async_http;
```

The stub in `src/transport/disabled/` declares the same signatures, with an
uninhabited transport type whose constructor returns `ErrorKind::Unsupported`.
`Protocol`, `Target`, session dispatch, the CLI, and the output code therefore compile
unchanged. CI builds and tests both feature sets, which catches signature drift
between the real module and its stub.

## Adding A Protocol

1. Add `src/transport/async_<proto>.rs`.
2. Implement `Transport` for the client transport.
3. Register the module in `src/transport/mod.rs`.
4. Add a `Protocol` variant and parser/display support in `src/protocol.rs`.
5. Add a `Target` variant that carries the protocol-specific data, and its
   client construction arm in `src/session.rs`. Transport-independent settings
   belong in `SessionConfig`.
6. Register server support if the protocol needs a `rup` echo server.
   Otherwise, add it to the "no server" arm of `run_server_until`.
7. If the protocol adds dependencies, put it behind a feature with a stub, as
   described in "Optional Transports".
8. Add parser, packet, transport, and high-level session tests.
