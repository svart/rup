# Architecture

The client path is an actor-style Tokio pipeline:

```text
generator --Request--> transmitter --StatEntry::Open--> statista --PingEvent--> caller
                         |                                  ^
                   send()|                            recv()|
                         v                                  |
                    Transport ----------- StatEntry::Close --
```

## Pipeline

1. `generator` creates `Request` values with sequence IDs and packet sizes.
2. `transmitter` calls `Transport::send()`, records the send timestamp, and
   forwards `StatEntry::Open`.
3. `receiver` calls `Transport::recv()` and forwards `StatEntry::Close`.
4. `statista` matches open and close entries by ID, computes RTTs, tracks
   timeouts, and emits structured `PingEvent` values for live sessions.
5. Callers consume `PingSession::next()` and decide how to present or process
   events. The CLI can render the same event stream as human-readable text or
   versioned JSON Lines. Completed sessions return a `PingReport`.

Fixed interval mode sleeps between generated requests. Adaptive mode waits for
`statista` to signal after either a response or timeout.

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

ICMP uses Linux ping sockets (`SOCK_DGRAM | IPPROTO_ICMP`) rather than raw
sockets. The kernel handles identifier assignment; `rup` builds the ICMP header
and embeds the same echo payload after it.

## Packet Format

UDP/TCP packets contain the bincode-serialized `Echo` payload plus optional zero
padding:

```text
Echo { id: u64, len: u16, resp_size: u16 } [padding...]
```

ICMP packets wrap that payload:

```text
[ICMP header: 8 bytes] [Echo payload: 12 bytes minimum] [padding...]
```

`PING_HDR_LEN` is 12 because it is the wire size of `Echo`, not
`size_of::<Echo>()`.

## Adding A Protocol

1. Add `src/transport/async_<proto>.rs`.
2. Implement `Transport` for the client transport.
3. Register the module in `src/transport/mod.rs`.
4. Add a `Protocol` variant and parser/display support in `src/protocol.rs`.
5. Register client construction in `src/lib.rs`.
6. Register server support if the protocol needs a `rup` echo server.
7. Add parser, packet, transport, and high-level session tests.
