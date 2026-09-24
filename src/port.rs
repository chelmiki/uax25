//! Models a single AX.25 port: owns a Tokio task that handles the
//! KISS link over a serial device to a TNC, decoding AX.25 frames
//! and forwarding them to whoever holds the receiving end of its
//! channel.
//!
//! `Ax25Port`'s fields are modeled after `/etc/ax25/axports` (name,
//! callsign, speed, paclen, window, description); however axports has
//! no notion of a serial device path (that's part of a KISS
//! "attachment", configured separetely using the `kissattach` command).
//!
//! ```text
//! # /etc/ax25/axports
//! #
//! # The format of this file is:
//! #
//! # name callsign speed paclen window description
//! #
//! #1	OH2BNS-1	1200	255	2	144.675 MHz (1200  bps)
//! #2	OH2BNS-9	38400	255	7	TNOS/Linux  (38400 bps)
//! ```
use crate::{Address, Frame, KissDecoder, parser::parse_frame};
use std::fmt;
use tokio::io::AsyncReadExt;
use tokio_serial::SerialPortBuilderExt;

pub struct Ax25Port {
    name: String,
    address: Address, // callsign + SSID
    _paclen: u16,
    _window: u8,
    description: String,
    join: tokio::task::JoinHandle<()>,
}

impl fmt::Display for Ax25Port {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}): {}", self.name, self.address, self.description)
    }
}

impl Ax25Port {
    pub fn new(
        name: String,
        address: Address,
        description: String,
        tty_device: String,
        speed: u32,
        tx: tokio::sync::mpsc::Sender<Frame>,
    ) -> Self {
        log::debug!("Starting TNC connection {tty_device} at {speed} bauds");
        let port = KissLink::new(tx, tty_device, speed);
        let join = tokio::spawn(port.run());

        Self {
            address,
            name,
            description,
            _paclen: 2048,
            _window: 10,
            join,
        }
    }

    pub fn abort(&self) {
        self.join.abort();
    }
}

struct KissLink {
    speed: u32,
    tty_device: String,
    frame_in_tx: tokio::sync::mpsc::Sender<Frame>,
}

impl KissLink {
    fn new(frame_in_tx: tokio::sync::mpsc::Sender<Frame>, tty_device: String, speed: u32) -> Self {
        Self {
            frame_in_tx,
            tty_device,
            speed,
        }
    }

    pub async fn run(self) {
        let mut stream = match tokio_serial::new(self.tty_device, self.speed).open_native_async() {
            Ok(stream) => stream,
            Err(e) => {
                log::error!("Failed to open serial port: {e}");
                return;
            }
        };
        let mut decoder = KissDecoder::default();

        println!("Waiting for data...");
        let mut buf = [0u8; 4096];
        'read_loop: loop {
            let n = match stream.read(&mut buf).await {
                Ok(n) => n,
                Err(e) => {
                    log::error!("Serial read error: {e}");
                    break;
                }
            };
            log::debug!("Read {} bytes", n);
            if n == 0 {
                break; // connection closed
            }
            log::debug!("Raw: {:02x?}", &buf[..n]);
            let frames = decoder.feed(&buf[..n]);
            for frame in frames {
                log::debug!("Detected KISS frame");
                if let Ok(ax25frame) = parse_frame(&frame.data) {
                    log::debug!("Succeessfully parsed AX.25 frame");
                    if self.frame_in_tx.send(ax25frame).await.is_err() {
                        log::warn!("Frame receiver dropped, stopping port");
                        break 'read_loop;
                    }
                } else {
                    log::error!("Invalid AX.25 frame");
                }
            }
        }
    }
}
