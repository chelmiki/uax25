use eyre::Result;
use uax25::{
    Ax25Port, Frame,
    model::{FrameType, UnnumberedType},
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
    env_logger::init();

    let (frame_in_tx, mut frame_in_rx) = tokio::sync::mpsc::channel(100);
    let port = Ax25Port::new(
        "vhf".to_string(),
        "EI4JKB-7".try_into()?,
        "APRS VHF Port 144.800 Mhz".to_string(),
        "/dev/rfcomm0".to_string(),
        9600,
        frame_in_tx,
    );
    log::debug!("Created port: {}", port);

    while let Some(frame) = frame_in_rx.recv().await {
        print_frame(frame);
    }
    log::warn!("Port channel closed, exiting");

    Ok(())
}
