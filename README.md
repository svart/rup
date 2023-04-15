# rup
`rup` universal pinger is a client-server application that allows to measure round trip time (RTT) between 2 endpoints. 
Currently 2 protocols are available: TCP and UDP. Both of them require running server side to echo receiving packets from clients.

Run server (UDP is used by default):
```sh
rup server 0.0.0.0:12345
```

Run client (UDP is used by default):
```sh
rup client 127.0.0.1:12345
```

## License

Licensed under either of

 * Apache License, Version 2.0
   ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license
   ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
