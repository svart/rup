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

Examples:

```sh
# ICMP is the default client protocol. Port is not required.
rup -n 10 google.com

# Adaptive mode, 10 pings over UDP.
rup -p udp -A -n 10 127.0.0.1:5000

# TCP with a custom request size.
rup -p tcp example.com:5000 --request-size 64

# UDP with DSCP EF (`184`, `0xb8`) set on outgoing packets.
rup -p udp --tos 184 127.0.0.1:5000

# Five-second ICMP burst with a 50 ms interval.
rup -i 50 -t 5 8.8.8.8
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
PING 127.0.0.1 (127.0.0.1) 12(40) bytes of data.
20 bytes from 127.0.0.1: seq=0 ttl=64 time=0.053 ms
20 bytes from 127.0.0.1: seq=1 ttl=64 time=0.052 ms

--- 127.0.0.1 ping statistics ---
2 packets transmitted, 2 received, 0% packet loss, time 1001ms
rtt min/avg/max/mdev = 0.052/0.052/0.053/0.001 ms
```
