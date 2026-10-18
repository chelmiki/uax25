use std::collections::HashMap;
use std::fmt;
use tokio::sync::{mpsc, oneshot};

use crate::{Address, Frame, SessionEvent};

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct PortId(String);

impl PortId {
    pub fn new(port_id: String) -> Self {
        Self(port_id)
    }
}

impl fmt::Display for PortId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct SessionId(u32);
impl SessionId {
    pub fn new(session_id: u32) -> Self {
        Self(session_id)
    }
}
impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum DispatcherError {
    #[error("no port bound to {0}")]
    NoPortForAddress(Address),
    #[error("multiple ports bound to {0}, specify one explicitly")]
    AmbiguousPort(Address),
    #[error("session {0} not found")]
    SessionNotFound(SessionId),
    #[error("port {0} is down")]
    PortDown(PortId),
    #[error("port {0} is busy")]
    PortBusy(PortId),
    #[error("port {0} not found")]
    PortNotFound(PortId),
    #[error("port required")]
    PortRequired,
    #[error("not connected")]
    NotConnected,
    #[error("address already in use on port {0}")]
    AddressInUse(PortId),
}

type Error = DispatcherError;

/// Local address a session is bound to.
#[derive(Hash, Eq, PartialEq, Clone)]
pub enum LocalAddress {
    /// Bound to a specific callsign/SSID
    Some(Address),
    /// Promiscuous: accepts traffic regardless of the frame's destination
    Any,
}

impl fmt::Display for LocalAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LocalAddress::Some(address) => write!(f, "{}", address),
            LocalAddress::Any => write!(f, "any"),
        }
    }
}

/// Remote address a session is filtered/connected to.
#[derive(Hash, Eq, PartialEq, Clone)]
pub enum RemoteAddress {
    /// Accepts/sends traffic to this specific peer only: a connected
    /// SeqPacket session, or a Datagram session that called `.connect()`.
    Some(Address),
    /// Accepts traffic from any peer - the default for a freshly
    /// registered Datagram session (UDP-like, unconnected).
    Any,
    /// Bound but not yet connected (SeqPacket only, right after
    /// `RegisterSession`). Never matched by an inbound frame.
    None,
}

impl fmt::Display for RemoteAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RemoteAddress::Some(address) => write!(f, "{}", address),
            RemoteAddress::Any => write!(f, "any"),
            RemoteAddress::None => write!(f, "none"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum SocketType {
    SeqPacket,
    Datagram,
}

struct PortEntry {
    address: Address,
    port_tx: mpsc::Sender<Frame>,
}
type PortTable = HashMap<PortId, PortEntry>;

struct Session {
    port_id: PortId,
    session_tx: mpsc::Sender<SessionEvent>,
    local: LocalAddress,
    remote: RemoteAddress,
}

#[derive(Default)]
struct SessionTable(HashMap<SessionId, Session>);

impl SessionTable {
    fn insert(&mut self, session_id: SessionId, session: Session) {
        self.0.insert(session_id, session);
    }

    fn remove(&mut self, session_id: &SessionId) -> Option<Session> {
        self.0.remove(session_id)
    }

    fn get(&self, session_id: &SessionId) -> Option<&Session> {
        self.0.get(session_id)
    }

    fn get_mut(&mut self, session_id: &SessionId) -> Option<&mut Session> {
        self.0.get_mut(session_id)
    }

    /// Is `(port_id, local, remote)` already claimed by a session other than `excluding`?
    /// `RemoteAddress::None` is never matched by an inbound frame (see `find_for_frame`),
    /// so it can never conflict with anything.
    fn conflicts(
        &self,
        port_id: &PortId,
        local: &LocalAddress,
        remote: &RemoteAddress,
        excluding: Option<&SessionId>,
    ) -> bool {
        if *remote == RemoteAddress::None {
            return false;
        }

        self.0.iter().any(|(session_id, session)| {
            Some(session_id) != excluding
                && &session.port_id == port_id
                && &session.local == local
                && &session.remote == remote
        })
    }

    /// Find the session that should receive an inbound frame from `port_id`, preferring
    /// an exact (local, remote) match, then a local-only match (remote = Any), then a
    /// fully promiscuous session (local = Any, remote = Any).
    fn find_for_frame(&self, port_id: &PortId, dst: &Address, src: &Address) -> Option<&SessionId> {
        let on_port = |session: &&Session| &session.port_id == port_id;

        self.0
            .iter()
            .find(|(_, session)| {
                on_port(session)
                    && session.local == LocalAddress::Some(dst.clone())
                    && session.remote == RemoteAddress::Some(src.clone())
            })
            .or_else(|| {
                self.0.iter().find(|(_, session)| {
                    on_port(session)
                        && session.local == LocalAddress::Some(dst.clone())
                        && session.remote == RemoteAddress::Any
                })
            })
            .or_else(|| {
                self.0.iter().find(|(_, session)| {
                    on_port(session)
                        && session.local == LocalAddress::Any
                        && session.remote == RemoteAddress::Any
                })
            })
            .map(|(session_id, _)| session_id)
    }
}

pub enum DispatcherCommand {
    // Port lifecycle
    RegisterPort {
        port_id: PortId,
        address: Address,
        port_tx: mpsc::Sender<Frame>,
    },
    DeregisterPort {
        port_id: PortId,
    },
    // Session lifecycle
    RegisterSession {
        local: LocalAddress,
        port: Option<PortId>,
        socket_type: SocketType,
        session_tx: mpsc::Sender<SessionEvent>,
        resp: oneshot::Sender<Result<SessionId, Error>>,
    },
    DeregisterSession {
        session_id: SessionId,
    },
    SetRemote {
        session_id: SessionId,
        remote: Address,
        resp: oneshot::Sender<Result<(), Error>>,
    },

    // Frame I/O
    FrameIn {
        frame: Frame,
        port_id: PortId,
    },
    FrameOut {
        frame: Frame,
        session_id: SessionId,
        resp: oneshot::Sender<Result<(), Error>>,
    },
}

pub struct Ax25Dispatcher {
    port_table: PortTable,
    session_table: SessionTable,
    next_session_id: u32,
    rx: mpsc::Receiver<DispatcherCommand>,
}

impl Ax25Dispatcher {
    pub fn new(rx: mpsc::Receiver<DispatcherCommand>) -> Self {
        Self {
            rx,
            port_table: Default::default(),
            session_table: Default::default(),
            next_session_id: 0,
        }
    }

    pub async fn run(&mut self) {
        log::debug!("Dispatcher ready and listening for events...");
        while let Some(command) = self.rx.recv().await {
            match command {
                DispatcherCommand::RegisterSession {
                    local,
                    port,
                    socket_type,
                    session_tx,
                    resp,
                } => self.register_session(local, port, socket_type, session_tx, resp),

                DispatcherCommand::DeregisterSession { session_id } => {
                    self.deregister_session(session_id)
                }

                DispatcherCommand::SetRemote {
                    session_id,
                    remote,
                    resp,
                } => self.set_remote(session_id, remote, resp),

                DispatcherCommand::FrameOut {
                    frame,
                    session_id,
                    resp,
                } => self.send_frame_to_port(frame, session_id, resp),

                DispatcherCommand::FrameIn { frame, port_id } => {
                    self.send_frame_to_socket(frame, port_id)
                }

                DispatcherCommand::RegisterPort {
                    port_id,
                    port_tx,
                    address,
                } => self.register_port(port_id, port_tx, address),
                DispatcherCommand::DeregisterPort { port_id } => self.deregister_port(port_id),
            }
        }
        log::info!("Dispatcher shutting down: no senders remain");
    }

    fn get_session_id(&mut self) -> SessionId {
        // Deliberately never reused: overflow is unlikely to happen
        // at AX.25's scale.
        self.next_session_id += 1;
        SessionId(self.next_session_id)
    }

    fn find_port(
        &self,
        local: &LocalAddress,
        port: Option<PortId>,
    ) -> Result<PortId, DispatcherError> {
        match (local, port) {
            // Explicit port given
            (LocalAddress::Any, Some(port_id)) => {
                // Promiscuous: no address to verify against
                self.port_table
                    .contains_key(&port_id)
                    .then(|| port_id.clone())
                    .ok_or(DispatcherError::PortNotFound(port_id))
            }
            (LocalAddress::Some(address), Some(port_id)) => match self.port_table.get(&port_id) {
                Some(entry) if &entry.address == address => Ok(port_id),
                Some(entry) => {
                    log::warn!(
                        "Port {} is bound to {}, not {}",
                        port_id,
                        entry.address,
                        address
                    );
                    Err(DispatcherError::PortNotFound(port_id))
                }
                None => Err(DispatcherError::PortNotFound(port_id)),
            },

            // No port given.
            (LocalAddress::Any, None) => Err(DispatcherError::PortRequired),
            (LocalAddress::Some(address), None) => {
                let matches: Vec<PortId> = self
                    .port_table
                    .iter()
                    .filter(|(_, entry)| &entry.address == address)
                    .map(|(port_id, _)| port_id.clone())
                    .collect();

                match matches.as_slice() {
                    [] => Err(DispatcherError::NoPortForAddress(address.clone())),
                    [port_id] => Ok(port_id.clone()),
                    _ => Err(DispatcherError::AmbiguousPort(address.clone())),
                }
            }
        }
    }

    fn register_session(
        &mut self,
        local: LocalAddress,
        port: Option<PortId>,
        socket_type: SocketType,
        session_tx: mpsc::Sender<SessionEvent>,
        resp: oneshot::Sender<Result<SessionId, Error>>,
    ) {
        let msg = match self.find_port(&local, port) {
            Ok(port_id) => {
                let remote = match socket_type {
                    SocketType::Datagram => RemoteAddress::Any,
                    SocketType::SeqPacket => RemoteAddress::None,
                };

                if self
                    .session_table
                    .conflicts(&port_id, &local, &remote, None)
                {
                    let err = DispatcherError::AddressInUse(port_id);
                    log::warn!(
                        "Session registration rejected: local={} remote={} type={:?}: {}",
                        local,
                        remote,
                        socket_type,
                        err,
                    );
                    Err(err)
                } else {
                    let session_id = self.get_session_id();

                    log::debug!(
                        "Session {} registered: local={} remote={} port={} type={:?}",
                        session_id,
                        local,
                        remote,
                        port_id,
                        socket_type,
                    );

                    self.session_table.insert(
                        session_id,
                        Session {
                            port_id,
                            session_tx,
                            local,
                            remote,
                        },
                    );

                    Ok(session_id)
                }
            }
            Err(err) => {
                log::warn!("Session registration rejected: local={}: {}", local, err);
                Err(err)
            }
        };

        if let Err(returned_msg) = resp.send(msg) {
            self.drop_unanswered(returned_msg.ok());
        }
    }

    fn deregister_session(&mut self, session_id: SessionId) {
        if self.session_table.remove(&session_id).is_some() {
            log::debug!("Session {} deregistered", session_id);
        }
    }

    /// The caller dropped its response channel before we could reply (e.g. the socket was
    /// torn down concurrently). If a session was left behind as a result, clean it up.
    fn drop_unanswered(&mut self, orphaned_session: Option<SessionId>) {
        match orphaned_session {
            Some(session_id) => {
                log::warn!(
                    "Caller gone before response could be sent; cleaning up session {}",
                    session_id
                );
                self.deregister_session(session_id);
            }
            None => log::warn!("Caller gone before response could be sent"),
        }
    }

    fn set_remote(
        &mut self,
        session_id: SessionId,
        remote: Address,
        resp: oneshot::Sender<Result<(), Error>>,
    ) {
        let msg = match self.session_table.get(&session_id) {
            Some(session) => {
                let port_id = session.port_id.clone();
                let new_remote = RemoteAddress::Some(remote);

                if self.session_table.conflicts(
                    &port_id,
                    &session.local,
                    &new_remote,
                    Some(&session_id),
                ) {
                    let err = DispatcherError::AddressInUse(port_id);
                    log::warn!(
                        "SetRemote rejected for session {}: local={} remote={}: {}",
                        session_id,
                        session.local,
                        new_remote,
                        err,
                    );
                    Err(err)
                } else {
                    let session = self.session_table.get_mut(&session_id).unwrap();
                    session.remote = new_remote;
                    Ok(())
                }
            }
            None => {
                let err = DispatcherError::SessionNotFound(session_id);
                log::warn!("SetRemote rejected: {}", err);
                Err(err)
            }
        };

        if resp.send(msg).is_err() {
            self.drop_unanswered(Some(session_id));
        }
    }

    fn send_frame_to_port(
        &mut self,
        frame: Frame,
        session_id: SessionId,
        resp: oneshot::Sender<Result<(), Error>>,
    ) {
        let msg = if let Some(session) = self.session_table.get(&session_id) {
            if session.remote == RemoteAddress::None {
                log::warn!(
                    "FrameOut rejected for session {}: not connected",
                    session_id
                );
                Err(DispatcherError::NotConnected)
            } else if let Some(entry) = self.port_table.get(&session.port_id) {
                log::trace!(
                    "Sending frame for session {} with src: {} dst: {} via {} port",
                    session_id,
                    frame.source,
                    frame.destination,
                    session.port_id,
                );
                match entry.port_tx.try_send(frame) {
                    Ok(()) => Ok(()),
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        let err = DispatcherError::PortBusy(session.port_id.clone());
                        log::warn!("FrameOut failed for session {}: {}", session_id, err);
                        Err(err)
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        let err = DispatcherError::PortDown(session.port_id.clone());
                        log::warn!("FrameOut failed for session {}: {}", session_id, err);
                        Err(err)
                    }
                }
            } else {
                let err = DispatcherError::PortNotFound(session.port_id.clone());
                log::warn!("FrameOut rejected for session {}: {}", session_id, err);
                Err(err)
            }
        } else {
            let err = DispatcherError::SessionNotFound(session_id);
            log::warn!("FrameOut rejected: {}", err);
            Err(err)
        };

        if resp.send(msg).is_err() {
            self.drop_unanswered(Some(session_id));
        }
    }

    fn send_frame_to_socket(&mut self, frame: Frame, port_id: PortId) {
        let session_id = self
            .session_table
            .find_for_frame(&port_id, &frame.destination, &frame.source)
            .copied();

        if let Some(session_id) = session_id {
            let session = self
                .session_table
                .get(&session_id)
                .expect("find_for_frame returned a session id that isn't in the table");

            log::trace!(
                "Received frame for session {} with src: {} dst: {} via {} port",
                session_id,
                frame.source,
                frame.destination,
                port_id,
            );

            match session.session_tx.try_send(SessionEvent::FrameIn(frame)) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    log::warn!(
                        "Session {} busy, dropping inbound frame via {} port",
                        session_id,
                        port_id
                    );
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    log::error!("Socket is gone, cleaning up session {}", session_id);
                    self.deregister_session(session_id);
                }
            }
        } else {
            log::warn!(
                "Session not found for inbound frame with src: {} dst: {} via {} port",
                frame.source,
                frame.destination,
                port_id,
            );
        }
    }

    fn register_port(&mut self, port_id: PortId, port_tx: mpsc::Sender<Frame>, address: Address) {
        log::info!("Port {} registered with address {}", port_id, address);
        self.port_table
            .insert(port_id, PortEntry { address, port_tx });
    }

    fn deregister_port(&mut self, port_id: PortId) {
        if self.port_table.remove(&port_id).is_some() {
            log::info!("Port {} deregistered", port_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_session_table() -> SessionTable {
        let (session_tx, _) = tokio::sync::mpsc::channel(10);
        let port_id = PortId::new("vhf".to_string());
        let local = LocalAddress::Some(Address::try_from("EI4JKB-6").unwrap());
        let local2 = LocalAddress::Some(Address::try_from("EI4JKB-9").unwrap());
        let remote = RemoteAddress::Some(Address::try_from("N0CALL-1").unwrap());

        let datagram_session = Session {
            port_id: port_id.clone(),
            session_tx: session_tx.clone(),
            local: local.clone(),
            remote: RemoteAddress::Any,
        };

        let seqpacket_session = Session {
            port_id: port_id.clone(),
            session_tx: session_tx.clone(),
            local: local2.clone(),
            remote: RemoteAddress::None,
        };

        let seqpacket_session2 = Session {
            port_id: port_id.clone(),
            session_tx,
            local: local2.clone(),
            remote: remote.clone(),
        };

        let mut table = SessionTable::default();
        table.insert(SessionId::new(1), datagram_session);
        table.insert(SessionId::new(2), seqpacket_session);
        table.insert(SessionId::new(3), seqpacket_session2);
        table
    }

    #[test]
    fn test_session_table_conflicts() {
        let port_id = PortId::new("vhf".to_string());
        let local = LocalAddress::Some(Address::try_from("EI4JKB-6").unwrap());
        let local2 = LocalAddress::Some(Address::try_from("EI4JKB-9").unwrap());
        let remote = RemoteAddress::Some(Address::try_from("N0CALL-1").unwrap());

        let table = create_session_table();

        // Only one session can exist with the same port_id, local
        // address and remote `Any`
        assert!(table.conflicts(&port_id, &local, &RemoteAddress::Any, None,));

        // A datagram session with remote `Any` can coexist with a any
        // other session sharing the same port and local address if
        // the remote is Some. The most specific address take
        // precedence when dispatching frames.
        assert!(!table.conflicts(
            &port_id,
            &local,
            &RemoteAddress::Some(Address::try_from("N1CALL-0").unwrap()),
            None,
        ));

        // Multiple SEQPACKET sessions can coexist with remote == None,
        // the conflict only arises when the remote is set.
        assert!(!table.conflicts(&port_id, &local2, &RemoteAddress::None, None,));

        // Multiple SEQPACKET sessions can coexist if they have different remotes.
        assert!(!table.conflicts(
            &port_id,
            &local2,
            &RemoteAddress::Some(Address::try_from("N1CALL-2").unwrap()),
            None,
        ));

        // Only one SEQPACKET session can exist with the same port_id, local and remote
        assert!(table.conflicts(&port_id, &local2, &remote, None,));

        // A session checking against its own existing claim is not a conflict
        // (idempotent reconnect/no-op case)
        assert!(!table.conflicts(&port_id, &local2, &remote, Some(&SessionId::new(3))));

        // The same (local, remote) tuple on a different port does not conflict
        let other_port = PortId::new("uhf".to_string());
        assert!(!table.conflicts(&other_port, &local2, &remote, None));
    }

    #[test]
    fn test_find_for_frame() {
        let table = create_session_table();
        let port_id = PortId::new("vhf".to_string());
        let dst = Address::try_from("EI4JKB-6").unwrap();
        let dst2 = Address::try_from("EI4JKB-9").unwrap();
        let remote = Address::try_from("N0CALL-1").unwrap();

        // A frame from `vhf` with destination EI4JKB-6 matches
        // session 1, no matter the source address as session one
        // remote is Any
        assert_eq!(
            table
                .find_for_frame(&port_id, &dst, &Address::try_from("N5CALL-1").unwrap())
                .unwrap(),
            &SessionId::new(1)
        );

        // A frame from `vhf` with destination EI4JKB-9 and source
        // N5CALL-1 does not match any session. There is session with
        // local == EI4JKB-9 but with remote set to either None or an
        // address differente than the N5CALL-1
        assert!(
            table
                .find_for_frame(&port_id, &dst2, &Address::try_from("N5CALL-1").unwrap())
                .is_none(),
        );

        // A frame from `vhf` with destination EI4JKB-9 and source
        // N0CALL-1 matches session 3.
        assert_eq!(
            table.find_for_frame(&port_id, &dst2, &remote).unwrap(),
            &SessionId::new(3)
        );

        // The same destination and source, but arriving on a different
        // port, does not match session 3's entry on `vhf`.
        let other_port = PortId::new("uhf".to_string());
        assert!(table.find_for_frame(&other_port, &dst2, &remote).is_none(),);
    }

    #[test]
    fn test_find_port() {
        let (_, rx) = mpsc::channel(10);
        let mut dispatcher = Ax25Dispatcher::new(rx);

        let addr = Address::try_from("EI4JKB-6").unwrap();
        let addr2 = Address::try_from("EI4JKB-9").unwrap();
        let vhf = PortId::new("vhf".to_string());
        let uhf = PortId::new("uhf".to_string());

        // Two ports deliberately bound to the same address, to exercise
        // the AmbiguousPort case below.
        let (vhf_tx, _) = mpsc::channel(10);
        let (uhf_tx, _) = mpsc::channel(10);
        dispatcher.register_port(vhf.clone(), vhf_tx, addr.clone());
        dispatcher.register_port(uhf.clone(), uhf_tx, addr.clone());

        // Any local + explicit, existing port: no address check at all.
        assert_eq!(
            dispatcher
                .find_port(&LocalAddress::Any, Some(vhf.clone()))
                .unwrap(),
            vhf
        );

        // Any local + explicit port that doesn't exist.
        let ghost = PortId::new("ghost".to_string());
        assert_eq!(
            dispatcher
                .find_port(&LocalAddress::Any, Some(ghost.clone()))
                .unwrap_err(),
            DispatcherError::PortNotFound(ghost)
        );

        // Some(address) local + explicit port that exists but is bound to
        // a different address.
        assert_eq!(
            dispatcher
                .find_port(&LocalAddress::Some(addr2.clone()), Some(vhf.clone()))
                .unwrap_err(),
            DispatcherError::PortNotFound(vhf.clone())
        );

        // Any local + no port given: there's no address to resolve from.
        assert_eq!(
            dispatcher.find_port(&LocalAddress::Any, None).unwrap_err(),
            DispatcherError::PortRequired
        );

        // Some(address) local + no port given + zero ports bound to that
        // address.
        assert_eq!(
            dispatcher
                .find_port(&LocalAddress::Some(addr2.clone()), None)
                .unwrap_err(),
            DispatcherError::NoPortForAddress(addr2)
        );

        // Some(address) local + no port given + two ports bound to the
        // same address: can't resolve unambiguously.
        assert_eq!(
            dispatcher
                .find_port(&LocalAddress::Some(addr.clone()), None)
                .unwrap_err(),
            DispatcherError::AmbiguousPort(addr)
        );
    }
}
