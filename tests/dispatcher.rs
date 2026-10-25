use tokio::sync::{mpsc, oneshot};
use uax25::SessionEvent;
use uax25::dispatcher::{Ax25Dispatcher, DispatcherCommand, SessionId};
use uax25::dispatcher::{DispatcherError, LocalAddress, PortId, PortStatus, SocketType};
use uax25::model::{FrameType, UnnumberedFrame, UnnumberedType};
use uax25::{Address, CommandResponse, Frame};

struct TestDispatcher {
    to_dispatcher: mpsc::Sender<DispatcherCommand>,
}

impl TestDispatcher {
    fn spawn() -> Self {
        let (to_dispatcher, rx) = mpsc::channel::<DispatcherCommand>(10);
        let mut dp = Ax25Dispatcher::new(rx);
        tokio::spawn(async move { dp.run().await });
        Self { to_dispatcher }
    }

    async fn register_port(&self, port_id: PortId, address: Address, port_tx: mpsc::Sender<Frame>) {
        self.to_dispatcher
            .send(DispatcherCommand::RegisterPort {
                port_id,
                address,
                paclen: 256,
                window: 2,
                port_tx,
            })
            .await
            .unwrap();
    }

    async fn register_session(
        &self,
        local: LocalAddress,
        port: Option<PortId>,
        socket_type: SocketType,
        session_tx: mpsc::Sender<SessionEvent>,
    ) -> Result<SessionId, DispatcherError> {
        let (resp, resp_rx) = oneshot::channel();
        self.to_dispatcher
            .send(DispatcherCommand::RegisterSession {
                local,
                port,
                socket_type,
                session_tx,
                resp,
            })
            .await
            .unwrap();
        resp_rx.await.unwrap()
    }

    async fn set_remote(
        &self,
        session_id: SessionId,
        remote: Address,
    ) -> Result<(), DispatcherError> {
        let (resp, resp_rx) = oneshot::channel();
        self.to_dispatcher
            .send(DispatcherCommand::SetRemote {
                session_id,
                remote,
                resp,
            })
            .await
            .unwrap();
        resp_rx.await.unwrap()
    }

    async fn frame_out(&self, frame: Frame, session_id: SessionId) -> Result<(), DispatcherError> {
        let (resp, resp_rx) = oneshot::channel();
        self.to_dispatcher
            .send(DispatcherCommand::FrameOut {
                frame,
                session_id,
                resp,
            })
            .await
            .unwrap();
        resp_rx.await.unwrap()
    }

    async fn frame_in(&self, frame: Frame, port_id: PortId) {
        self.to_dispatcher
            .send(DispatcherCommand::FrameIn { frame, port_id })
            .await
            .unwrap();
    }

    async fn set_port_status(&self, port_id: PortId, status: PortStatus) {
        self.to_dispatcher
            .send(DispatcherCommand::PortStatus { port_id, status })
            .await
            .unwrap();
    }

    async fn deregister_port(&self, port_id: PortId) {
        self.to_dispatcher
            .send(DispatcherCommand::DeregisterPort { port_id })
            .await
            .unwrap();
    }

    async fn deregister_session(&self, session_id: SessionId) {
        self.to_dispatcher
            .send(DispatcherCommand::DeregisterSession { session_id })
            .await
            .unwrap();
    }
}

fn ui_frame(source: Address, destination: Address) -> Frame {
    Frame {
        source,
        destination,
        repeaters: vec![],
        command_response: CommandResponse::Command,
        poll_final: true,
        frame: FrameType::Unnumbered(UnnumberedFrame {
            kind: UnnumberedType::Ui {
                pid: 1,
                payload: "test".into(),
            },
        }),
    }
}

#[tokio::test]
async fn test_inbound_frame_routing_precedence() {
    let dispatcher = TestDispatcher::spawn();
    let (to_port, _port_rx) = mpsc::channel(10);
    let (to_seq_session, mut seq_session_rx) = mpsc::channel(10);
    let (to_dgram_session, mut dgram_session_rx) = mpsc::channel(10);

    let address: Address = "EI4JKB-4".try_into().unwrap();
    let port_id = PortId::new("test_port".to_string());

    // A session needs a registered port to bind to
    dispatcher
        .register_port(port_id.clone(), address.clone(), to_port)
        .await;

    // Register SEQ_PACKET Session with explicit port and same
    // Session: `port: test_vhf, local: remote: None`
    let seq_session = dispatcher
        .register_session(
            LocalAddress::Some(address.clone()),
            Some(port_id.clone()),
            SocketType::SeqPacket,
            to_seq_session,
        )
        .await
        .unwrap();
    assert_eq!(seq_session, SessionId::new(1));

    // Receive a frame with destination EI4JKB-4 via `test_vhf`port
    // shoud be dropped as sessions is still not `Connected`, the
    // remote is still not set.
    let remote: Address = "N0CALL-1".try_into().unwrap();
    let frame = Frame {
        source: remote.clone(),
        destination: address.clone(),
        repeaters: vec![],
        command_response: CommandResponse::Command,
        poll_final: true,
        frame: FrameType::Unnumbered(UnnumberedFrame {
            kind: UnnumberedType::Ui {
                pid: 1,
                payload: "test".into(),
            },
        }),
    };
    dispatcher.frame_in(frame.clone(), port_id.clone()).await;

    // Register a Datagram Session
    // Session: `port: test_vhf, local: EI4JKB-4 remote: Any`
    let dgram_session = dispatcher
        .register_session(
            LocalAddress::Some(address.clone()),
            None,
            SocketType::Datagram,
            to_dgram_session,
        )
        .await
        .unwrap();
    assert_eq!(dgram_session, SessionId::new(2));

    // The RegisterSession round-trip above guarantees the dispatcher already
    // drained and dropped the first frame, since it's a single sequential
    // consumer: neither session should have received anything from it.
    assert!(seq_session_rx.try_recv().is_err());
    assert!(dgram_session_rx.try_recv().is_err());

    // Receiving the same frame will now land in the Datagram session
    // which is bound to the same port, same local address but remote
    // is by default Any.
    dispatcher.frame_in(frame.clone(), port_id.clone()).await;

    // SetRemote in the SEQPACKET session
    dispatcher
        .set_remote(seq_session, remote.clone())
        .await
        .unwrap();

    // The SetRemote round-trip above guarantees the dispatcher already
    // drained and routed the second frame: it should have landed in the
    // Datagram session (seq_session was still unconnected at the time),
    // and nothing should have reached seq_session.
    match dgram_session_rx.try_recv() {
        Ok(SessionEvent::FrameIn(received_frame)) => assert_eq!(received_frame, frame),
        _ => panic!("expected SessionEvent::FrameIn"),
    }
    assert!(seq_session_rx.try_recv().is_err());

    // Receiving the same frame will now land in the SEQPACKET session
    // which offers a more specific match than the Datagram
    // session. Now the source address of the frame matches the remote
    // address of the SEQPACKET session.
    dispatcher.frame_in(frame.clone(), port_id.clone()).await;

    match seq_session_rx.recv().await {
        Some(SessionEvent::FrameIn(received_frame)) => assert_eq!(received_frame, frame),
        _ => panic!("expected SessionEvent::FrameIn"),
    }
    assert!(dgram_session_rx.try_recv().is_err());

    // A frame with the same destination but a different source doesn't
    // match the SEQPACKET session's connected remote, so it falls back to
    // the Datagram session's looser `remote: Any` match.
    let other_source: Address = "N9CALL-1".try_into().unwrap();
    let frame = Frame {
        source: other_source,
        destination: address,
        repeaters: vec![],
        command_response: CommandResponse::Command,
        poll_final: true,
        frame: FrameType::Unnumbered(UnnumberedFrame {
            kind: UnnumberedType::Ui {
                pid: 1,
                payload: "test".into(),
            },
        }),
    };
    dispatcher.frame_in(frame.clone(), port_id).await;

    match dgram_session_rx.recv().await {
        Some(SessionEvent::FrameIn(received_frame)) => assert_eq!(received_frame, frame),
        _ => panic!("expected SessionEvent::FrameIn"),
    }
    assert!(seq_session_rx.try_recv().is_err());
}

#[tokio::test]
async fn test_frame_out_delivers_to_port() {
    let dispatcher = TestDispatcher::spawn();
    let (to_port, mut port_rx) = mpsc::channel(10);
    let (to_session, _session_rx) = mpsc::channel(10);

    let address: Address = "EI4JKB-4".try_into().unwrap();
    let remote: Address = "N0CALL-1".try_into().unwrap();
    let port_id = PortId::new("test_port".to_string());

    dispatcher
        .register_port(port_id.clone(), address.clone(), to_port)
        .await;
    dispatcher
        .set_port_status(port_id.clone(), PortStatus::Up)
        .await;

    let session_id = dispatcher
        .register_session(
            LocalAddress::Some(address.clone()),
            Some(port_id),
            SocketType::SeqPacket,
            to_session,
        )
        .await
        .unwrap();

    dispatcher
        .set_remote(session_id, remote.clone())
        .await
        .unwrap();

    let frame = ui_frame(address, remote);
    dispatcher
        .frame_out(frame.clone(), session_id)
        .await
        .unwrap();

    let received = port_rx
        .recv()
        .await
        .expect("Expected frame from Dispatcher");
    assert_eq!(received, frame);
}

#[tokio::test]
async fn test_frame_out_not_connected() {
    let dispatcher = TestDispatcher::spawn();
    let (to_port, _port_rx) = mpsc::channel(10);
    let (to_session, _session_rx) = mpsc::channel(10);

    let address: Address = "EI4JKB-4".try_into().unwrap();
    let port_id = PortId::new("test_port".to_string());

    dispatcher
        .register_port(port_id.clone(), address.clone(), to_port)
        .await;

    // A SEQPACKET session that has only been bound, never connected
    let session_id = dispatcher
        .register_session(
            LocalAddress::Some(address.clone()),
            Some(port_id),
            SocketType::SeqPacket,
            to_session,
        )
        .await
        .unwrap();

    let frame = ui_frame(address, "N0CALL-1".try_into().unwrap());
    let err = dispatcher.frame_out(frame, session_id).await.unwrap_err();
    assert_eq!(err, DispatcherError::NotConnected);
}

#[tokio::test]
async fn test_frame_out_session_not_found() {
    let dispatcher = TestDispatcher::spawn();

    let address: Address = "EI4JKB-4".try_into().unwrap();
    let remote: Address = "N0CALL-1".try_into().unwrap();
    let bogus_session = SessionId::new(999);

    let frame = ui_frame(address, remote);
    let err = dispatcher
        .frame_out(frame, bogus_session)
        .await
        .unwrap_err();
    assert_eq!(err, DispatcherError::SessionNotFound(bogus_session));
}

#[tokio::test]
async fn test_frame_out_port_channel_closed() {
    let dispatcher = TestDispatcher::spawn();
    let (to_port, port_rx) = mpsc::channel(10);
    let (to_session, _session_rx) = mpsc::channel(10);

    let address: Address = "EI4JKB-4".try_into().unwrap();
    let remote: Address = "N0CALL-1".try_into().unwrap();
    let port_id = PortId::new("test_port".to_string());

    dispatcher
        .register_port(port_id.clone(), address.clone(), to_port)
        .await;
    dispatcher
        .set_port_status(port_id.clone(), PortStatus::Up)
        .await;

    let session_id = dispatcher
        .register_session(
            LocalAddress::Some(address.clone()),
            Some(port_id.clone()),
            SocketType::SeqPacket,
            to_session,
        )
        .await
        .unwrap();

    dispatcher
        .set_remote(session_id, remote.clone())
        .await
        .unwrap();

    // Simulate the port task crashing without deregistering or reporting
    // its own status: the port entry is still in the table and still
    // marked Up, but its channel is now closed.
    drop(port_rx);

    let frame = ui_frame(address, remote);
    let err = dispatcher.frame_out(frame, session_id).await.unwrap_err();
    assert_eq!(err, DispatcherError::PortDown(port_id));
}

#[tokio::test]
async fn test_frame_out_rejected_while_port_status_down() {
    let dispatcher = TestDispatcher::spawn();
    let (to_port, _port_rx) = mpsc::channel(10);
    let (to_session, _session_rx) = mpsc::channel(10);

    let address: Address = "EI4JKB-4".try_into().unwrap();
    let remote: Address = "N0CALL-1".try_into().unwrap();
    let port_id = PortId::new("test_port".to_string());

    // A freshly registered port defaults to Down, even though its
    // channel is perfectly healthy. The port will be set to UP by the
    // KISS Link actor once the KISS link becomes healthy.
    dispatcher
        .register_port(port_id.clone(), address.clone(), to_port)
        .await;

    let session_id = dispatcher
        .register_session(
            LocalAddress::Some(address.clone()),
            Some(port_id.clone()),
            SocketType::SeqPacket,
            to_session,
        )
        .await
        .unwrap();

    dispatcher
        .set_remote(session_id, remote.clone())
        .await
        .unwrap();

    let frame = ui_frame(address.clone(), remote.clone());
    let err = dispatcher.frame_out(frame, session_id).await.unwrap_err();
    assert_eq!(err, DispatcherError::PortDown(port_id.clone()));

    // Once the port reports itself Up, the same session can send.
    dispatcher.set_port_status(port_id, PortStatus::Up).await;

    let frame = ui_frame(address, remote);
    dispatcher.frame_out(frame, session_id).await.unwrap();
}

#[tokio::test]
async fn test_frame_out_port_status_recovers() {
    let dispatcher = TestDispatcher::spawn();
    let (to_port, _port_rx) = mpsc::channel(10);
    let (to_session, _session_rx) = mpsc::channel(10);

    let address: Address = "EI4JKB-4".try_into().unwrap();
    let remote: Address = "N0CALL-1".try_into().unwrap();
    let port_id = PortId::new("test_port".to_string());

    dispatcher
        .register_port(port_id.clone(), address.clone(), to_port)
        .await;
    dispatcher
        .set_port_status(port_id.clone(), PortStatus::Up)
        .await;

    let session_id = dispatcher
        .register_session(
            LocalAddress::Some(address.clone()),
            Some(port_id.clone()),
            SocketType::SeqPacket,
            to_session,
        )
        .await
        .unwrap();

    dispatcher
        .set_remote(session_id, remote.clone())
        .await
        .unwrap();

    // Healthy to start with.
    dispatcher
        .frame_out(ui_frame(address.clone(), remote.clone()), session_id)
        .await
        .unwrap();

    // The link goes down (e.g. the TNC stops responding)
    dispatcher
        .set_port_status(port_id.clone(), PortStatus::Down)
        .await;
    let err = dispatcher
        .frame_out(ui_frame(address.clone(), remote.clone()), session_id)
        .await
        .unwrap_err();
    assert_eq!(err, DispatcherError::PortDown(port_id.clone()));

    // ...and recovers, without the session ever being torn down.
    dispatcher.set_port_status(port_id, PortStatus::Up).await;
    dispatcher
        .frame_out(ui_frame(address, remote), session_id)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_port_status_for_unknown_port_is_noop() {
    let dispatcher = TestDispatcher::spawn();

    // Should not panic even though this port was never registered.
    dispatcher
        .set_port_status(PortId::new("ghost_port".to_string()), PortStatus::Up)
        .await;

    // A round-trip afterward proves the dispatcher is still alive and
    // responsive.
    let (to_port, _port_rx) = mpsc::channel(10);
    let (to_session, _session_rx) = mpsc::channel(10);
    let address: Address = "EI4JKB-4".try_into().unwrap();
    let port_id = PortId::new("test_port".to_string());

    dispatcher
        .register_port(port_id.clone(), address.clone(), to_port)
        .await;
    dispatcher
        .register_session(
            LocalAddress::Some(address),
            Some(port_id),
            SocketType::Datagram,
            to_session,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn test_frame_out_port_not_found() {
    let dispatcher = TestDispatcher::spawn();
    let (to_port, _port_rx) = mpsc::channel(10);
    let (to_session, _session_rx) = mpsc::channel(10);

    let address: Address = "EI4JKB-4".try_into().unwrap();
    let remote: Address = "N0CALL-1".try_into().unwrap();
    let port_id = PortId::new("test_port".to_string());

    dispatcher
        .register_port(port_id.clone(), address.clone(), to_port)
        .await;

    let session_id = dispatcher
        .register_session(
            LocalAddress::Some(address.clone()),
            Some(port_id.clone()),
            SocketType::SeqPacket,
            to_session,
        )
        .await
        .unwrap();

    dispatcher
        .set_remote(session_id, remote.clone())
        .await
        .unwrap();

    // The port itself is torn down and deregistered, leaving the session
    // still pointing at a port that no longer exists in the table.
    dispatcher.deregister_port(port_id.clone()).await;

    let frame = ui_frame(address, remote);
    let err = dispatcher.frame_out(frame, session_id).await.unwrap_err();
    assert_eq!(err, DispatcherError::PortNotFound(port_id));
}

#[tokio::test]
async fn test_frame_out_port_busy() {
    let dispatcher = TestDispatcher::spawn();
    // Capacity 1, and nothing ever drains it: the second FrameOut below
    // must find it full.
    let (to_port, _port_rx) = mpsc::channel(1);
    let (to_session, _session_rx) = mpsc::channel(10);

    let address: Address = "EI4JKB-4".try_into().unwrap();
    let remote: Address = "N0CALL-1".try_into().unwrap();
    let port_id = PortId::new("test_port".to_string());

    dispatcher
        .register_port(port_id.clone(), address.clone(), to_port)
        .await;
    dispatcher
        .set_port_status(port_id.clone(), PortStatus::Up)
        .await;

    let session_id = dispatcher
        .register_session(
            LocalAddress::Some(address.clone()),
            Some(port_id.clone()),
            SocketType::SeqPacket,
            to_session,
        )
        .await
        .unwrap();

    dispatcher
        .set_remote(session_id, remote.clone())
        .await
        .unwrap();

    // Fills the port's single buffer slot.
    dispatcher
        .frame_out(ui_frame(address.clone(), remote.clone()), session_id)
        .await
        .unwrap();

    // The channel is now full, so this must fail immediately with
    // `PortBusy`.
    let err = dispatcher
        .frame_out(ui_frame(address, remote), session_id)
        .await
        .unwrap_err();
    assert_eq!(err, DispatcherError::PortBusy(port_id));
}

#[tokio::test]
async fn test_deregister_session_removes_session() {
    let dispatcher = TestDispatcher::spawn();
    let (to_port, _port_rx) = mpsc::channel(10);
    let (to_session, _session_rx) = mpsc::channel(10);

    let address: Address = "EI4JKB-4".try_into().unwrap();
    let remote: Address = "N0CALL-1".try_into().unwrap();
    let port_id = PortId::new("test_port".to_string());

    dispatcher
        .register_port(port_id.clone(), address.clone(), to_port)
        .await;

    let session_id = dispatcher
        .register_session(
            LocalAddress::Some(address.clone()),
            Some(port_id),
            SocketType::SeqPacket,
            to_session,
        )
        .await
        .unwrap();

    dispatcher.deregister_session(session_id).await;

    // SetRemote on a deregistered session should fail
    let err = dispatcher
        .set_remote(session_id, remote.clone())
        .await
        .unwrap_err();
    assert_eq!(err, DispatcherError::SessionNotFound(session_id));

    // FrameOut on a deregistered session should fail too
    let frame = ui_frame(address, remote);
    let err = dispatcher.frame_out(frame, session_id).await.unwrap_err();
    assert_eq!(err, DispatcherError::SessionNotFound(session_id));
}

#[tokio::test]
async fn test_deregister_session_frees_address() {
    let dispatcher = TestDispatcher::spawn();
    let (to_port, _port_rx) = mpsc::channel(10);
    let (to_session1, _session_rx1) = mpsc::channel(10);
    let (to_session2, _session_rx2) = mpsc::channel(10);

    let address: Address = "EI4JKB-4".try_into().unwrap();
    let port_id = PortId::new("test_port".to_string());

    dispatcher
        .register_port(port_id.clone(), address.clone(), to_port)
        .await;

    let session_id = dispatcher
        .register_session(
            LocalAddress::Some(address.clone()),
            Some(port_id.clone()),
            SocketType::Datagram,
            to_session1,
        )
        .await
        .unwrap();

    // A second Datagram bind to the same tuple is rejected while the
    // first session is still alive.
    let err = dispatcher
        .register_session(
            LocalAddress::Some(address.clone()),
            Some(port_id.clone()),
            SocketType::Datagram,
            to_session2.clone(),
        )
        .await
        .unwrap_err();
    assert_eq!(err, DispatcherError::AddressInUse(port_id.clone()));

    dispatcher.deregister_session(session_id).await;

    // Now that the first session is gone, the same bind succeeds.
    dispatcher
        .register_session(
            LocalAddress::Some(address),
            Some(port_id),
            SocketType::Datagram,
            to_session2,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn test_deregister_unknown_session_is_noop() {
    let dispatcher = TestDispatcher::spawn();

    // Should not panic even though this session was never registered.
    dispatcher.deregister_session(SessionId::new(999)).await;

    // A round-trip afterward proves the dispatcher is still alive and
    // responsive.
    let (to_port, _port_rx) = mpsc::channel(10);
    let (to_session, _session_rx) = mpsc::channel(10);
    let address: Address = "EI4JKB-4".try_into().unwrap();
    let port_id = PortId::new("test_port".to_string());

    dispatcher
        .register_port(port_id.clone(), address.clone(), to_port)
        .await;
    dispatcher
        .register_session(
            LocalAddress::Some(address),
            Some(port_id),
            SocketType::Datagram,
            to_session,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn test_deregister_port_removes_port() {
    let dispatcher = TestDispatcher::spawn();
    let (to_port, _port_rx) = mpsc::channel(10);
    let (to_session, _session_rx) = mpsc::channel(10);

    let address: Address = "EI4JKB-4".try_into().unwrap();
    let port_id = PortId::new("test_port".to_string());

    dispatcher
        .register_port(port_id.clone(), address.clone(), to_port)
        .await;
    dispatcher.deregister_port(port_id.clone()).await;

    // RegisterSession referencing the now-gone port explicitly should fail.
    let err = dispatcher
        .register_session(
            LocalAddress::Some(address),
            Some(port_id.clone()),
            SocketType::Datagram,
            to_session,
        )
        .await
        .unwrap_err();
    assert_eq!(err, DispatcherError::PortNotFound(port_id));
}

#[tokio::test]
async fn test_deregister_unknown_port_is_noop() {
    let dispatcher = TestDispatcher::spawn();

    // Should not panic even though this port was never registered.
    dispatcher
        .deregister_port(PortId::new("ghost_port".to_string()))
        .await;

    // A round-trip afterward proves the dispatcher is still alive and
    // responsive.
    let (to_port, _port_rx) = mpsc::channel(10);
    let (to_session, _session_rx) = mpsc::channel(10);
    let address: Address = "EI4JKB-4".try_into().unwrap();
    let port_id = PortId::new("test_port".to_string());

    dispatcher
        .register_port(port_id.clone(), address.clone(), to_port)
        .await;
    dispatcher
        .register_session(
            LocalAddress::Some(address),
            Some(port_id),
            SocketType::Datagram,
            to_session,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn test_set_remote_address_in_use() {
    let dispatcher = TestDispatcher::spawn();
    let (to_port, _port_rx) = mpsc::channel(10);
    let (to_session1, _session_rx1) = mpsc::channel(10);
    let (to_session2, _session_rx2) = mpsc::channel(10);

    let address: Address = "EI4JKB-4".try_into().unwrap();
    let remote: Address = "N0CALL-1".try_into().unwrap();
    let port_id = PortId::new("test_port".to_string());

    dispatcher
        .register_port(port_id.clone(), address.clone(), to_port)
        .await;

    let session1 = dispatcher
        .register_session(
            LocalAddress::Some(address.clone()),
            Some(port_id.clone()),
            SocketType::SeqPacket,
            to_session1,
        )
        .await
        .unwrap();

    let session2 = dispatcher
        .register_session(
            LocalAddress::Some(address),
            Some(port_id.clone()),
            SocketType::SeqPacket,
            to_session2,
        )
        .await
        .unwrap();

    dispatcher
        .set_remote(session1, remote.clone())
        .await
        .unwrap();

    // session2 connecting to the same remote that session1 already
    // claimed on the same local/port should be rejected.
    let err = dispatcher.set_remote(session2, remote).await.unwrap_err();
    assert_eq!(err, DispatcherError::AddressInUse(port_id));
}
