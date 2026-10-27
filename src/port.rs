//! Models a single AX.25 port: owns a Tokio task that handles the
//! KISS link over a serial device to a TNC, decoding AX.25 frames
//! and forwarding them to the dispatcher.
//!
//! `Ax25Port`'s fields are modeled after `/etc/ax25/axports` (name,
//! callsign, speed, paclen, window, description); however axports has
//! no notion of a serial device path (that's part of a KISS
//! "attachment", configured separetely using the `kissattach` command).
//! `uax25` introduces a new /etc/uax25/axports that includes the TNC
//! link details.
//!
//! ```text
//! # /etc/uax25/axports
//! #
//! # Extends the real linux-ax25 axports format with two columns before the
//! # free-text description: the KISS attachment's tty device or TCP socket, and
//! # the KISS port nibble (always present; use 0 unless this is a multi-port TNC
//! # sharing one serial line across several logical radios).
//! #
//! # name callsign speed paclen window attachment   kiss_port description
//! vhf    EI4JKB-7 9600  255    2      /dev/rfcomm0 0         144.800 MHz FM packet
//! mulit0 EI4JKB-9 1200  255    2      /dev/ttyUSB1 0         Multiport TNC - radio A
//! multi1 EI4JKB-1 1200  255    2      /dev/ttyUSB1 1         Multiport TNC - radio B
//! direwolf EI4JKB-2 9600 255   2      tcp://127.0.0.1:8001 0 Direwolf Software TNC
//! ```
use crate::{
    Address, Frame, KissDecoder, KissEncoder, KissTncCmd, PortId,
    dispatcher::{DispatcherCommand, PortStatus},
    parser::{encode_frame, parse_frame},
};
use std::{fmt, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_serial::SerialPortBuilderExt;

#[derive(Debug, thiserror::Error)]
#[error("dispatcher is no longer running")]
pub struct DispatcherGone;

/// A bidirectional KISS transport: either a serial TNC attachment or a
/// TCP KISS TNC (e.g. direwolf). Can be mocked with an in-memory
/// `tokio::io::duplex()` stream instead of a real device.
trait AsyncLink: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncLink for T {}

const READ_BUF_SIZE: usize = 8192;

pub struct PortConfig {
    pub name: String,
    pub address: Address,
    pub description: String,
    pub attachment: String,
    pub speed: u32,
    pub paclen: u16,
    pub window: u8,
}

pub struct Ax25Port {
    port_id: PortId,
    address: Address,
    description: String,
    join: tokio::task::JoinHandle<()>,
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
}

impl fmt::Display for Ax25Port {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({}): {}",
            self.port_id, self.address, self.description
        )
    }
}

impl Ax25Port {
    pub async fn new(
        config: PortConfig,
        tx: tokio::sync::mpsc::Sender<DispatcherCommand>,
    ) -> Result<Self, DispatcherGone> {
        log::debug!(
            "Starting TNC connection {} at {} bauds",
            config.attachment,
            config.speed
        );
        let port_id = PortId::new(config.name.clone());
        let link = KissLink::new(tx, port_id.clone(), &config);
        let from_dispatcher = link.register().await?;
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let join = tokio::spawn(link.run(from_dispatcher, shutdown_rx));

        Ok(Self {
            port_id,
            address: config.address,
            description: config.description,
            join,
            shutdown_tx,
        })
    }

    pub fn abort(&self) {
        self.join.abort();
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
        let _ = self.join.await;
    }
}

struct KissLink {
    speed: u32,
    attachment: String,
    port_id: PortId,
    address: Address,
    paclen: u16,
    window: u8,
    to_dispatcher: tokio::sync::mpsc::Sender<DispatcherCommand>,
    kiss_encoder: KissEncoder,
}

#[derive(Debug, PartialEq, Eq)]
enum LinkEvent {
    Continue,
    Reconnect,
    Shutdown,
}

impl KissLink {
    fn new(
        to_dispatcher: tokio::sync::mpsc::Sender<DispatcherCommand>,
        port_id: PortId,
        config: &PortConfig,
    ) -> Self {
        Self {
            to_dispatcher,
            port_id,
            address: config.address.clone(),
            paclen: config.paclen,
            window: config.window,
            attachment: config.attachment.clone(),
            speed: config.speed,
            // Multi-port TNC currently not supported, defaults to channel 0
            kiss_encoder: KissEncoder::new(0),
        }
    }

    async fn set_status(&self, status: PortStatus) -> Result<(), DispatcherGone> {
        match self
            .to_dispatcher
            .send(DispatcherCommand::PortStatus {
                port_id: self.port_id.clone(),
                status,
            })
            .await
        {
            Ok(()) => Ok(()),
            _ => Err(DispatcherGone),
        }
    }

    pub async fn open_kiss_link(&self) -> Result<Box<dyn AsyncLink>, DispatcherGone> {
        let mut delay = Duration::from_millis(500);
        loop {
            match self.connect().await {
                Ok(stream) => {
                    self.set_status(PortStatus::Up).await?;
                    return Ok(stream);
                }
                Err(e) => {
                    log::warn!(
                        "Failed to open {}: {e}, retrying in {delay:?}",
                        self.attachment
                    );
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(10));
                }
            }
        }
    }

    /// Opens the configured KISS attachment: a `tcp://host:port` KISS
    /// TNC (e.g. direwolf), or otherwise a serial device path.
    async fn connect(&self) -> std::io::Result<Box<dyn AsyncLink>> {
        if let Some(addr) = self.attachment.strip_prefix("tcp://") {
            let stream = TcpStream::connect(addr).await?;
            Ok(Box::new(stream))
        } else {
            let stream = tokio_serial::new(&self.attachment, self.speed).open_native_async()?;
            Ok(Box::new(stream))
        }
    }

    async fn register(&self) -> Result<tokio::sync::mpsc::Receiver<Frame>, DispatcherGone> {
        let (port_tx, from_dispatcher) = tokio::sync::mpsc::channel(10);
        match self
            .to_dispatcher
            .send(DispatcherCommand::RegisterPort {
                port_id: self.port_id.clone(),
                address: self.address.clone(),
                paclen: self.paclen,
                window: self.window,
                port_tx,
            })
            .await
        {
            Ok(()) => Ok(from_dispatcher),
            Err(_) => Err(DispatcherGone),
        }
    }

    pub async fn run(
        self,
        mut from_dispatcher: tokio::sync::mpsc::Receiver<Frame>,
        mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
    ) {
        loop {
            let Ok(mut stream) = self.open_kiss_link().await else {
                break;
            };
            match self
                .serve_link(&mut stream, &mut from_dispatcher, &mut shutdown_rx)
                .await
            {
                Ok(LinkEvent::Reconnect) => continue,
                Ok(LinkEvent::Shutdown) | Err(_) => break,
                Ok(LinkEvent::Continue) => unreachable!("serve_link's loop never returns Continue"),
            }
        }

        log::info!("Shutting down port {}...", self.port_id);
        if self
            .to_dispatcher
            .send(DispatcherCommand::DeregisterPort {
                port_id: self.port_id,
            })
            .await
            .is_err()
        {
            log::debug!("Dispatcher already gone, nothing to deregister");
        };
    }

    async fn serve_link(
        &self,
        stream: &mut dyn AsyncLink,
        from_dispatcher: &mut tokio::sync::mpsc::Receiver<Frame>,
        shutdown_rx: &mut tokio::sync::oneshot::Receiver<()>,
    ) -> Result<LinkEvent, DispatcherGone> {
        let mut decoder = KissDecoder::default();
        let mut buf = [0u8; READ_BUF_SIZE];
        loop {
            let event = tokio::select! {
                result = stream.read(&mut buf) => self.handle_inbound(result, &buf, &mut decoder).await?,
                msg = from_dispatcher.recv() => self.handle_outbound(msg, stream).await?,
                _ = &mut *shutdown_rx => LinkEvent::Shutdown,
            };

            // The link is no longer valid, or a shutdown was
            // requested: exit serve_link so the caller can react
            // (reconnect or stop for good).
            if !matches!(event, LinkEvent::Continue) {
                return Ok(event);
            }
        }
    }

    async fn handle_outbound(
        &self,
        msg: Option<Frame>,
        stream: &mut dyn AsyncLink,
    ) -> Result<LinkEvent, DispatcherGone> {
        let Some(frame) = msg else {
            return Err(DispatcherGone);
        };
        let bytes = match self
            .kiss_encoder
            .encode(KissTncCmd::Data, &encode_frame(&frame))
        {
            Ok(bytes) => bytes,
            Err(e) => {
                log::error!("Failed to encode outbound frame: {e}");
                return Ok(LinkEvent::Continue);
            }
        };
        if let Err(e) = stream.write_all(&bytes).await {
            log::error!("Serial write error: {e}");
            self.set_status(PortStatus::Down).await?;
            return Ok(LinkEvent::Reconnect);
        };

        Ok(LinkEvent::Continue)
    }

    async fn handle_inbound(
        &self,
        result: std::io::Result<usize>,
        buf: &[u8],
        decoder: &mut KissDecoder,
    ) -> Result<LinkEvent, DispatcherGone> {
        let n = match result {
            Ok(n) => n,
            Err(e) => {
                log::error!("Serial read error: {e}");
                self.set_status(PortStatus::Down).await?;
                return Ok(LinkEvent::Reconnect);
            }
        };
        log::debug!("Read {} bytes", n);
        if n == 0 {
            self.set_status(PortStatus::Down).await?;
            return Ok(LinkEvent::Reconnect);
        };
        log::debug!("Raw: {:02x?}", &buf[..n]);
        let frames = decoder.feed(&buf[..n]);
        for frame in frames {
            log::debug!("Detected KISS frame");
            if !matches!(frame.command, KissTncCmd::Data) {
                log::debug!("Ignoring non-Data KISS frame: {:?}", frame.command);
                continue;
            }
            if let Ok(ax25frame) = parse_frame(&frame.data) {
                log::debug!("Succeessfully parsed AX.25 frame");
                if self
                    .to_dispatcher
                    .send(DispatcherCommand::FrameIn {
                        frame: ax25frame,
                        port_id: self.port_id.clone(),
                    })
                    .await
                    .is_err()
                {
                    log::warn!("Dispatcher is gone, stopping port");
                    return Err(DispatcherGone);
                }
            } else {
                log::error!("Invalid AX.25 frame");
            }
        }
        Ok(LinkEvent::Continue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CommandResponse, FrameType, UnnumberedFrame, UnnumberedType};
    use std::io;

    fn test_config() -> PortConfig {
        PortConfig {
            name: "test".to_string(),
            address: Address::try_from("N0CALL-1").unwrap(),
            description: "test port".to_string(),
            attachment: "unused".to_string(),
            speed: 9600,
            paclen: 256,
            window: 2,
        }
    }

    fn test_link() -> (KissLink, tokio::sync::mpsc::Receiver<DispatcherCommand>) {
        let (to_dispatcher, rx) = tokio::sync::mpsc::channel(10);
        let config = test_config();
        let port_id = PortId::new(config.name.clone());
        (KissLink::new(to_dispatcher, port_id, &config), rx)
    }

    fn test_frame() -> Frame {
        Frame {
            source: Address::try_from("N0CALL-1").unwrap(),
            destination: Address::try_from("APRS-0").unwrap(),
            repeaters: vec![],
            command_response: CommandResponse::Command,
            poll_final: false,
            frame: FrameType::Unnumbered(UnnumberedFrame {
                kind: UnnumberedType::Ui {
                    pid: 0xF0,
                    payload: b"hello".to_vec(),
                },
            }),
        }
    }

    fn kiss_encode(frame: &Frame) -> Vec<u8> {
        KissEncoder::new(0)
            .encode(KissTncCmd::Data, &encode_frame(frame))
            .unwrap()
    }

    #[tokio::test]
    async fn handle_inbound_forwards_decoded_frame() {
        let (link, mut rx) = test_link();
        let frame = test_frame();
        let bytes = kiss_encode(&frame);
        let mut decoder = KissDecoder::default();

        let event = link
            .handle_inbound(Ok(bytes.len()), &bytes, &mut decoder)
            .await
            .unwrap();
        assert_eq!(event, LinkEvent::Continue);

        match rx.try_recv().unwrap() {
            DispatcherCommand::FrameIn { frame: got, .. } => assert_eq!(got, frame),
            _ => panic!("expected FrameIn"),
        }
    }

    #[tokio::test]
    async fn handle_inbound_ignores_non_data_kiss_command() {
        let (link, mut rx) = test_link();
        let bytes = KissEncoder::new(0)
            .encode(KissTncCmd::SetHardware, b"whatever")
            .unwrap();
        let mut decoder = KissDecoder::default();

        let event = link
            .handle_inbound(Ok(bytes.len()), &bytes, &mut decoder)
            .await
            .unwrap();
        assert_eq!(event, LinkEvent::Continue);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn handle_inbound_ignores_malformed_ax25_payload() {
        let (link, mut rx) = test_link();
        // Too short to be a valid AX.25 address field.
        let bytes = KissEncoder::new(0)
            .encode(KissTncCmd::Data, &[0x01, 0x02])
            .unwrap();
        let mut decoder = KissDecoder::default();

        let event = link
            .handle_inbound(Ok(bytes.len()), &bytes, &mut decoder)
            .await
            .unwrap();
        assert_eq!(event, LinkEvent::Continue);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn handle_inbound_read_error_marks_down_and_reconnects() {
        let (link, mut rx) = test_link();
        let mut decoder = KissDecoder::default();

        let event = link
            .handle_inbound(Err(io::Error::other("error")), &[], &mut decoder)
            .await
            .unwrap();
        assert_eq!(event, LinkEvent::Reconnect);

        match rx.try_recv().unwrap() {
            DispatcherCommand::PortStatus { status, .. } => assert_eq!(status, PortStatus::Down),
            _ => panic!("expected PortStatus"),
        }
    }

    #[tokio::test]
    async fn handle_inbound_eof_marks_down_and_reconnects() {
        let (link, mut rx) = test_link();
        let mut decoder = KissDecoder::default();

        let event = link.handle_inbound(Ok(0), &[], &mut decoder).await.unwrap();
        assert_eq!(event, LinkEvent::Reconnect);

        match rx.try_recv().unwrap() {
            DispatcherCommand::PortStatus { status, .. } => assert_eq!(status, PortStatus::Down),
            _ => panic!("expected PortStatus"),
        }
    }

    #[tokio::test]
    async fn handle_inbound_dispatcher_gone_when_forwarding() {
        let (link, rx) = test_link();
        drop(rx);
        let frame = test_frame();
        let bytes = kiss_encode(&frame);
        let mut decoder = KissDecoder::default();

        let result = link
            .handle_inbound(Ok(bytes.len()), &bytes, &mut decoder)
            .await;
        assert!(matches!(result, Err(DispatcherGone)));
    }

    #[tokio::test]
    async fn handle_outbound_encodes_and_writes_frame() {
        let (link, _rx) = test_link();
        let frame = test_frame();
        let (mut test_side, mut mock_link) = tokio::io::duplex(4096);

        let event = link
            .handle_outbound(Some(frame.clone()), &mut mock_link)
            .await
            .unwrap();
        assert_eq!(event, LinkEvent::Continue);

        let mut buf = [0u8; 4096];
        let n = test_side.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], kiss_encode(&frame).as_slice());
    }

    #[tokio::test]
    async fn handle_outbound_dispatcher_gone_when_channel_closed() {
        let (link, _rx) = test_link();
        let (_test_side, mut mock_link) = tokio::io::duplex(1024);

        let result = link.handle_outbound(None, &mut mock_link).await;
        assert!(matches!(result, Err(DispatcherGone)));
    }

    #[tokio::test]
    async fn handle_outbound_write_error_marks_down_and_reconnects() {
        let (link, mut rx) = test_link();
        let frame = test_frame();
        let (test_side, mut mock_link) = tokio::io::duplex(1024);
        drop(test_side); // peer gone: writes to mock_link should now fail

        let event = link
            .handle_outbound(Some(frame), &mut mock_link)
            .await
            .unwrap();
        assert_eq!(event, LinkEvent::Reconnect);

        match rx.try_recv().unwrap() {
            DispatcherCommand::PortStatus { status, .. } => assert_eq!(status, PortStatus::Down),
            _ => panic!("expected PortStatus"),
        }
    }
}
