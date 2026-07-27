# CLI Usage

`rup` runs as a client by default. The only subcommand is `server`:

```text
rup [OPTIONS] [CLIENT_OPTIONS] <remote-address>
rup [OPTIONS] server <local-address>

Commands:
  server  Receive requests and send them back immediately

Options:
  -p, --protocol <protocol>  udp, tcp, or icmp
```

`-p` / `--protocol` is a root option. When used with `server`, it must appear
before `server`. When omitted, client mode defaults to ICMP and server mode
defaults to UDP.

## Client

```sh
rup [OPTIONS] [CLIENT_OPTIONS] <remote-address>
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
| `--tos <0-255>` | Outgoing IP TOS / IPv6 traffic class byte |
| `--local-address <addr>` | Local bind address [default: `0.0.0.0:0`] |
| `--output <human|jsonl>` | Output format [default: `human`] |
| `--format <template>` | Customize each human-readable reply line |

Examples:

```sh
# ICMP is the default client protocol. Port is not required.
rup -n 10 google.com

# Adaptive mode, 10 pings over UDP. A Linux ICMP error also advances the mode.
rup -p udp -A -n 10 127.0.0.1:5000

# TCP with a custom request size.
rup -p tcp example.com:5000 --request-size 64

# UDP with DSCP EF (`184`, `0xb8`) set on outgoing packets.
rup -p udp --tos 184 127.0.0.1:5000

# Five-second ICMP burst with a 50 ms interval.
rup -i 50 -t 5 8.8.8.8
```

Ctrl+C stops generation of new requests, allows already-sent requests to reply
or time out, and then emits the normal human or JSONL summary.

## Server

```sh
rup [OPTIONS] server <local-address>
```

UDP and TCP use a `rup` echo server when one is available. On Linux, UDP also
measures matching remote ICMP errors such as port-unreachable responses. TCP
uses application echo if its first connection succeeds; if that connection
returns an error, it measures a fresh TCP connect outcome for every request.
This includes SYN-ACK, RST, and ICMP-derived connection errors. A connection or
datagram with no terminal response still times out normally.

An open TCP service that does not speak the rup echo protocol is treated as an
echo server and can therefore time out after accepting the connection. ICMP
mode does not use a server: the remote kernel responds to echo requests.

For UDP, the server reflects the received TOS / traffic class byte on echo
responses when the operating system supplies that packet metadata. TCP and ICMP
do not have a `rup` server-side reflection path; `--tos` still sets outgoing
client packets for those protocols.

```sh
rup server 0.0.0.0:5000
rup -p tcp server 0.0.0.0:5000
```

## Address Format

- UDP/TCP require `host:port`, for example `127.0.0.1:5000` or
  `example.com:5000`.
- ICMP accepts a hostname or IP address without a port, for example `8.8.8.8`
  or `google.com`.
- IPv6 socket addresses should use brackets when a port is present, for example
  `[::1]:5000`.

## Output

Human-readable output is the default:

```text
PING 127.0.0.1 (127.0.0.1) 12(40) bytes of data.
12 bytes from 127.0.0.1: seq=0 ttl=64 time=0.053 ms
12 bytes from 127.0.0.1: seq=1 ttl=64 time=0.052 ms

--- 127.0.0.1 ping statistics ---
2 packets transmitted, 2 received, 0% packet loss, time 1001ms
rtt min/avg/max/mdev = 0.052/0.052/0.053/0.001 ms
```

Use `--format` to customize each reply line while retaining the normal header
and final statistics:

```sh
rup 127.0.0.1 --format '{ip}: {seq} => {rtt}'
```

```text
PING 127.0.0.1 (127.0.0.1) 12(40) bytes of data.
127.0.0.1: 0 => 0.053 ms
127.0.0.1: 1 => 0.052 ms

--- 127.0.0.1 ping statistics ---
2 packets transmitted, 2 received, 0% packet loss, time 1001ms
rtt min/avg/max/mdev = 0.052/0.052/0.053/0.001 ms
```

The supported fields are:

| Field | Value |
|-------|-------|
| `{target}` | Original hostname or address supplied on the command line |
| `{ip}` | Resolved destination IP address |
| `{seq}` | Sequence number |
| `{rtt}` | RTT in milliseconds with three decimal places and the `ms` unit |
| `{rtt_ms}` | RTT in milliseconds with three decimal places and no unit |
| `{size}` | Response size in bytes |
| `{ttl}` | TTL, or `-` when unavailable |
| `{status}` | `reply` or `terminal_reply` |
| `{protocol}` | `icmp`, `udp`, or `tcp` |

Use `{{` and `}}` for literal braces. Invalid fields, unmatched braces, and
embedded newlines are rejected before the ping session starts. `--format`
cannot be used with `--output jsonl`. Placeholder precision modifiers are not
supported.

Use `--output jsonl` for machine-readable output. The first line is a
`rup.ping` metadata record with schema version 3. It is followed by `reply`,
`terminal_reply`, `timeout`, or `reorder_or_loss` event records and one final
`summary` record. Every record occupies exactly one line; errors remain on
stderr so successful stdout can be parsed as a JSON Lines stream.

Terminal UDP/TCP responses use `terminal_reply` with `size_bytes: 0` and
`ttl: null`; they count toward `received` and RTT summary statistics. Event and
summary records contain absolute `timestamp_ms` values and do not include
relative elapsed-time fields.

```json
{"adaptive":false,"address":"127.0.0.1","interval_ms":1000,"packet_size_bytes":40,"protocol":"icmp","record":"metadata","request_size_bytes":12,"schema":"rup.ping","started_at_ms":1700000000000,"target":"127.0.0.1","version":3}
{"record":"reply","rtt_ms":0.053,"seq":0,"size_bytes":12,"timestamp_ms":1700000000001,"ttl":64}
{"loss_percent":0.0,"received":1,"record":"summary","rtt_max_ms":0.053,"rtt_mean_ms":0.053,"rtt_median_ms":0.053,"rtt_min_ms":0.053,"rtt_std_dev_ms":0.0,"sent":1,"timestamp_ms":1700000000002}
```

`--output` is a client option and is rejected with the `server` subcommand.
