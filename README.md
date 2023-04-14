# rup
rup universal pinger is a client-server application that allows to measure round trip time (RTT) between 2 endpoints. 
Currently 2 protocols are available: TCP and UDP. Both of them require running server side to echo receiving packets from clients.

Run server (UDP is used by default):
```sh
rup server 0.0.0.0:12345
```

Run client (UDP is used by default):
```sh
rup client 127.0.0.1:12345
```
