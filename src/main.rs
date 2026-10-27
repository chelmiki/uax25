use std::fs::OpenOptions;
use std::time::Duration;

use eyre::Result;
use uax25::{
    Address, Ax25Port, Frame, PortConfig, PortId, SessionEvent, SocketType,
    dispatcher::{Ax25Dispatcher, DispatcherCommand, LocalAddress},
    model::{CommandResponse, FrameType, UnnumberedFrame, UnnumberedType},
};

fn print_frame(frame: Frame) {
    print!("{} --> ", frame.source,);
    for repeater in frame.repeaters {
        print!("{}:{} --> ", repeater.address, repeater.has_repeated);
    }
    println!("{}: ", frame.destination,);
    match frame.frame {
        FrameType::Unnumbered(uframe) => {
            if let UnnumberedType::Ui { pid: _, payload } = uframe.kind {
                println!(
                    "payload: {}",
                    payload.iter().map(|&b| b as char).collect::<String>()
                )
            }
        }
        frame => println!("{:?}", frame),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open("uax25.log")?;
    env_logger::Builder::from_default_env()
        .target(env_logger::Target::Pipe(Box::new(log_file)))
        .init();

    let (frame_in_tx, mut frame_in_rx) = tokio::sync::mpsc::channel(100);
    let (to_dispatcher, dispatcher_rx) = tokio::sync::mpsc::channel(100);

    // Spawn the dispatcher
    let mut dispatcher = Ax25Dispatcher::new(dispatcher_rx);
    tokio::spawn(async move { dispatcher.run().await });

    // Create and register port
    let serial_port = Ax25Port::new(
        PortConfig {
            name: "VGC-NR76".to_string(),
            address: "EI4JKB-7".try_into()?,
            description: "APRS VHF Port 144.800 Mhz".to_string(),
            attachment: "/dev/rfcomm0".to_string(),
            speed: 9600,
            paclen: 255,
            window: 2,
        },
        to_dispatcher.clone(),
    )
    .await?;
    log::debug!("Created port: {}", serial_port);

    // Create and register port
    let tcp_port = Ax25Port::new(
        PortConfig {
            name: "Remote Direwolf".to_string(),
            address: "EI4JKB-4".try_into()?,
            description: "VHF Baofeng via Direwolf (TCP)".to_string(),
            attachment: "tcp://127.0.0.1:8001".to_string(),
            speed: 9600,
            paclen: 255,
            window: 2,
        },
        to_dispatcher.clone(),
    )
    .await?;
    log::debug!("Created port: {}", tcp_port);

    // Register a session for the serial (VGC-NR76) port
    let (serial_resp, serial_resp_rx) = tokio::sync::oneshot::channel();

    to_dispatcher
        .send(DispatcherCommand::RegisterSession {
            local: LocalAddress::Any,
            port: Some(PortId::new("VGC-NR76".to_string())),
            socket_type: SocketType::Datagram,
            session_tx: frame_in_tx.clone(),
            resp: serial_resp,
        })
        .await?;
    let serial_session_id = match serial_resp_rx.await? {
        Ok(session_id) => {
            log::info!("Registered session with session_id: {session_id}");
            session_id
        }
        Err(e) => {
            log::error!("Failed to create session: {e}");
            return Err(e.into());
        }
    };

    // Register a session for the Baofeng/Direwolf port
    let (baofeng_resp, baofeng_resp_rx) = tokio::sync::oneshot::channel();

    to_dispatcher
        .send(DispatcherCommand::RegisterSession {
            local: LocalAddress::Any,
            port: Some(PortId::new("Remote Direwolf".to_string())),
            socket_type: SocketType::Datagram,
            session_tx: frame_in_tx,
            resp: baofeng_resp,
        })
        .await?;

    let baofeng_session_id = match baofeng_resp_rx.await? {
        Ok(session_id) => {
            log::info!("Registered session with session_id: {session_id}");
            session_id
        }
        Err(e) => {
            log::error!("Failed to create session: {e}");
            return Err(e.into());
        }
    };

    let serial_address = Address::try_from("EI4JKB-7")?;
    let baofeng_address = Address::try_from("EI4JKB-4")?;

    let serial_message = "Hello from EI4JKB-7 (VGC-NR76)".to_string();
    let mut serial_message_interval = tokio::time::interval(Duration::from_secs(30));

    let baofeng_message = "Hello from EI4JKB-4 (Baofeng)".to_string();
    let mut baofeng_message_interval = tokio::time::interval(Duration::from_secs(30));

    loop {
        tokio::select! {
            frame = frame_in_rx.recv() => {
                match frame {
                    Some(SessionEvent::FrameIn(frame)) => {
                        println!("Received frame:");
                        print_frame(frame);
                    }
                    None => {
                        log::warn!("Port channel closed, exiting");
                        break;
                    }
                }
            }
            _ = serial_message_interval.tick() => {
                log::debug!("Sending message VGC-NR76 -> Baofeng");
                let msg = Frame {
                    source: serial_address.clone(),
                    destination: baofeng_address.clone(),
                    repeaters: vec![],
                    command_response: CommandResponse::Command,
                    poll_final: false,
                    frame: FrameType::Unnumbered(UnnumberedFrame {
                        kind: UnnumberedType::Ui {
                            pid: 0xF0,
                            payload: serial_message.clone().into_bytes(),
                        },
                    }),
                };
                let (resp, resp_rx) = tokio::sync::oneshot::channel();
                to_dispatcher
                    .send(DispatcherCommand::FrameOut {
                        frame: msg,
                        session_id: serial_session_id,
                        resp,
                    })
                    .await?;
                match resp_rx.await {
                    Ok(Ok(())) => log::debug!("Message sent"),
                    Ok(Err(e)) => log::warn!("Failed to send message: {e}"),
                    Err(e) => log::warn!("Dispatcher gone before responding to message: {e}"),
                }
            }
            _ = baofeng_message_interval.tick() => {
                log::debug!("Sending message Baofeng -> VGC-NR76");
                let msg = Frame {
                    source: baofeng_address.clone(),
                    destination: serial_address.clone(),
                    repeaters: vec![],
                    command_response: CommandResponse::Command,
                    poll_final: false,
                    frame: FrameType::Unnumbered(UnnumberedFrame {
                        kind: UnnumberedType::Ui {
                            pid: 0xF0,
                            payload: baofeng_message.clone().into_bytes(),
                        },
                    }),
                };
                let (resp, resp_rx) = tokio::sync::oneshot::channel();
                to_dispatcher
                    .send(DispatcherCommand::FrameOut {
                        frame: msg,
                        session_id: baofeng_session_id,
                        resp,
                    })
                    .await?;
                match resp_rx.await {
                    Ok(Ok(())) => log::debug!("Message sent"),
                    Ok(Err(e)) => log::warn!("Failed to send message: {e}"),
                    Err(e) => log::warn!("Dispatcher gone before responding to message: {e}"),
                }
            }
        }
    }

    Ok(())
}
