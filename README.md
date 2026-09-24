# uax25 - A userspace AX.25 Rust implementation

This is an early stage project where I have been playing around with the idea of doing a userspace implementation of AX.25, motivated by an interest in packet radio and following the recent removal of AX.25 from the main Linux kernel tree in 7.1.

So far, I have implemented an AX.25 packet parser and model, based on the AX.25 v2.0 specification, and also a KISS framer/decoder. I was able to successfully decode APRS packets received from the Vero VGC NR76 via its KISS TNC.

Build the project:
```
cargo build
```

Bind the VGC NR76 Bluetooth TNC to rfcomm0:
```
sudo rfcomm bind 0 38:D2:00:XX:XX:XX 1
```

Listen for AX.25 packets and print the source, repeaters and destination addresses:
```
❯ RUST_LOG=debug ./target/debug/uax25 
[2026-09-24T16:45:09Z DEBUG uax25::port] Starting TNC connection /dev/rfcomm0 at 9600 bauds
[2026-09-24T16:45:09Z DEBUG uax25] Created port: vhf (EI4JKB-5): APRS VHF Port 144.800 Mhz
[2026-09-24T16:45:09Z DEBUG mio_serial] opening serial port in synchronous blocking mode
[2026-09-24T16:45:09Z DEBUG mio_serial] setting O_NONBLOCK for /dev/rfcomm0
Waiting for data...
[2026-09-24T16:46:03Z DEBUG uax25::port] Read 76 bytes
[2026-09-24T16:46:03Z DEBUG uax25::port] Detected KISS frame
[2026-09-24T16:46:03Z DEBUG uax25::parser] It is an UFrame
[2026-09-24T16:46:03Z DEBUG uax25::port] Succeessfully parsed AX.25 frame
EI4JKB-7 --> EDXXXX-3:true --> WIDE1-0:true --> WIDE2-1:false --> APN000-0: 
payload: =XXXX.XXN/XXXXX.XXW(360/000/A=000249
```

## What I would like this package to be

I would like next to create an AX.25 daemon `uax25d` that implements the AX.25 state machine and supports connection-oriented AX.25 sessions.
### Architecture
```
        Radio A                Radio B
       ┌───────┐              ┌───────┐
       │  TNC  │              │  TNC  │
       └───┬───┘              └───┬───┘
           │ KISS/serial          │ KISS/serial
           ▼                      ▼
     ┌───────────┐          ┌───────────┐
     │  Ax25Port │          │  Ax25Port │
     └─────┬─────┘          └─────┬─────┘
           │                      │
           └──────────┬───────────┘
                      ▼
              ┌───────────────────┐
              │    Dispatcher     │
              │     (router)      │
              └─────────┬─────────┘
                        │
        ┌───────────────┼───────────────┐
        ▼               ▼               ▼
  ┌───────────┐   ┌───────────┐    ┌───────────┐
  │  Ax25Conn │   │  Ax25Conn │    │  Ax25Conn │ 
  └─────┬─────┘   └─────┬─────┘    └─────┬─────┘ 
        │               │                │
        ▼               ▼                ▼
  ┌───────────┐   ┌───────────┐    ┌───────────┐
  │    BBS    │   │ uax25call │    │   APRS    │
  └───────────┘   └───────────┘    └───────────┘
```

### Radio communication

The idea is to support KISS, extending the current framer/decoder to also be capable of sending AX.25 packets within KISS frames to a radio using a KISS TNC. This would allow the use of any hardware or software (Direwolf) TNC. Communication with the TNC can be done via a serial interface or through a TCP socket.

Since `uax25d` is a daemon rather than a one-shot CLI tool, it will read a ports configuration file at startup (ideally the existing `/etc/ax25/axports`) instead of taking a single TNC's connection details as command-line flags, so it can manage several radios/TNCs concurrently, each under its own callsign + SSID.

### State machine

`uax25d` needs to support AX.25 connected-mode sessions: establishing and tearing down connections, tracking sequence numbers, retries, timers and flow control.

### Clients

`uax25d` should interoperate with clients, applications wanting to send AX.25 packets but also applications that want an AX.25 connection to a BBS or a mail provider. Communication between clients and `uax25d` will be done via IPC (unix sockets, for example). For this we will create uax25call, uax25listen, and basically an uax25 version of each of the tools in [linuxax25](https://linux-ax25.in-berlin.de/wiki/Main_Page). These tools will use IPC to communicate with `uax25d`, rather than AF_AX25 sockets with the Linux Kernel. For the client side, we must also provide a thin client library that facilitates the creation of new AX.25 applications.

### Kernel networking interoperation

One of the advantages of the AX.25 kernel implementation is the support of AF_AX25 sockets. These sockets not only give applications a way into the kernel's AX.25 implementation (connection handling, timers, AX.25 encapsulation, etc), but also expose `ax` network interfaces that can be bound to an IP address, making AX.25 just another layer-2 protocol on the networking stack.

This could be achieved by having `uax25d` set up `tun` interfaces, encapsulating IP packets into AX.25 frames and maintaining a mapping of IP addresses to callsigns.

## License

LGPL-3.0-or-later. See `COPYING.LESSER`.
