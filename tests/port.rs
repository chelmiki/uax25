use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use uax25::dispatcher::{DispatcherCommand, PortStatus};
use uax25::model::{CommandResponse, FrameType, UnnumberedFrame, UnnumberedType};
use uax25::parser::encode_frame;
use uax25::{Address, Ax25Port, Frame, KissEncoder, KissTncCmd, PortConfig};

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

// End-to-end: spawn a real Ax25Port over a loopback TCP KISS TNC
#[tokio::test]
async fn port_lifecycle_over_tcp() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accept = tokio::spawn(async move { listener.accept().await.unwrap().0 });

    let (to_dispatcher, mut from_port) = tokio::sync::mpsc::channel(10);
    let config = PortConfig {
        name: "test".to_string(),
        address: Address::try_from("N0CALL-1").unwrap(),
        description: "test port".to_string(),
        attachment: format!("tcp://{addr}"),
        speed: 9600,
        paclen: 256,
        window: 2,
    };

    let port = Ax25Port::new(config, to_dispatcher).await.unwrap();
    let mut tnc = accept.await.unwrap();

    let port_tx = match from_port.recv().await.unwrap() {
        DispatcherCommand::RegisterPort { port_tx, .. } => port_tx,
        _ => panic!("expected RegisterPort"),
    };
    match from_port.recv().await.unwrap() {
        DispatcherCommand::PortStatus { status, .. } => assert_eq!(status, PortStatus::Up),
        _ => panic!("expected PortStatus"),
    }

    // Inbound: bytes arriving from the TNC get parsed and forwarded.
    let frame = test_frame();
    let bytes = kiss_encode(&frame);
    tnc.write_all(&bytes).await.unwrap();
    match from_port.recv().await.unwrap() {
        DispatcherCommand::FrameIn { frame: got, .. } => assert_eq!(got, frame),
        _ => panic!("expected FrameIn"),
    }

    // Outbound: a frame handed to the port gets encoded and sent to the TNC.
    port_tx.send(frame.clone()).await.unwrap();
    let mut buf = [0u8; 4096];
    let n = tnc.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], bytes.as_slice());

    // Shutdown: deregisters cleanly and the task actually completes.
    port.shutdown().await;
    match from_port.recv().await.unwrap() {
        DispatcherCommand::DeregisterPort { .. } => {}
        _ => panic!("expected DeregisterPort"),
    }
}
