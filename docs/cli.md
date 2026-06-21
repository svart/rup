# CLI Usage

`rup` has two subcommands:

```text
rup [OPTIONS] <COMMAND>

Commands:
  client  Send requests to the remote side and measure RTT
  server  Receive requests and send them back immediately

Options:
  -p, --protocol <protocol>  udp, tcp, or icmp
```

`-p` / `--protocol` is a root option, so it must appear before `client` or
`server`. When omitted, `client` defaults to ICMP and `server` defaults to UDP.

## Client

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
| `--tos <0-255>` | Outgoing IP TOS / IPv6 traffic class byte |
| `--local-address <addr>` | Local bind address [default: `0.0.0.0:0`] |

Examples:

```sh
# ICMP is the default client protocol. Port is not required.
rup client -n 10 google.com

# Adaptive mode, 10 pings over UDP.
rup -p udp client -A -n 10 127.0.0.1:5000

# TCP with a custom request size.
rup -p tcp client example.com:5000 --request-size 64

# UDP with DSCP EF (`184`, `0xb8`) set on outgoing packets.
rup -p udp client --tos 184 127.0.0.1:5000

# Five-second ICMP burst with a 50 ms interval.
rup client -i 50 -t 5 8.8.8.8
```

## Server

```sh
rup [OPTIONS] server <local-address>
```

UDP and TCP use a `rup` echo server. ICMP does not: the remote kernel responds
to echo requests directly.

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

```text
seq=0 time=7.800 ms
seq=1 time=7.650 ms

--- statistics ---
2 requests sent, 2 received, 0% loss
min/med/avg/max = 7.650 ms / 7.800 ms / 7.725 ms / 7.800 ms
std_dev = 75.000 us
```
