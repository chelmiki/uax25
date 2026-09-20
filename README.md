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
./target/debug/uax25 serial /dev/rfcomm0 9600
Waiting for data...
Received KISS frame. Cmd: Data, Port 0
N0CALL-3 --> N0DIGI1-3:true --> N0DIGI2-3:true --> WIDE2-0:true --> APMI06-0: 
payload: T#133,140,132,000,051,255,00001111
Received KISS frame. Cmd: Data, Port 0
N0CALL2-0 --> N0DIGI2-3:true --> WIDE1-0:true --> N0CALL3-0: 
payload: '~I[l -/>
Received KISS frame. Cmd: Data, Port 0
N0CALL4-1 --> N0DIGI3-3:true --> WIDE2-0:true --> N0DIGI1-3:true --> N0DIGI2-3:true --> N0DIGI4-0:true --> APRS-0:false --> APU25N-0: 
payload: @201842zXXXX.XXN/XXXXX.XXW_068/000g000t081r000p000P000h40b09961WX <redacted location>
Received KISS frame. Cmd: Data, Port 0
N0CALL5-7 --> N0DIGI5-0:true --> N0DIGI1-3:true --> N0DIGI2-3:true --> WIDE2-0:true --> APALHA-0: 
payload: !XXXX.XXN/XXXXX.XXW!000/000/A=000674sotadata.org.uk
```

## What I would like this package to be

I would like next to create an AX.25 daemon `uax25d` that implements the AX.25 state machine and supports connection-oriented AX.25 sessions.

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
