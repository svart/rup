# rup

`rup` universal pinger measures round trip time (RTT) between 2 endpoints over
UDP, TCP, or ICMP. UDP and TCP require a server to echo received packets. ICMP
uses raw sockets (requires root or `CAP_NET_RAW`).

## Usage

Run server (UDP default):
```sh
rup server 0.0.0.0:12345
rup server -p tcp 0.0.0.0:12345
```

Run client:
```sh
rup client 127.0.0.1:12345           # 1s interval, infinite
rup client -A -n 10 127.0.0.1:12345  # adaptive, 10 pings
rup client -p icmp -n 5 example.com  # ICMP via DNS name
```

### Options

| Flag | Description |
|------|-------------|
| `-p` | Protocol: `udp` (default), `tcp`, `icmp` |
| `-i` | Interval in ms (default 1000) |
| `-A` | Adaptive mode (send next on response) |
| `-n` | Number of pings to send |
| `-t` | Run time limit in seconds |
| `-W` | Response wait timeout in ms (default 1000) |
| `--request-size` | Request payload size (min 12) |
| `--response-size` | Response payload size (min 12) |
| `--local-address` | Bind address (default `0.0.0.0:0`) |

## License

Licensed under either of

 * Apache License, Version 2.0
   ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license
   ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.
