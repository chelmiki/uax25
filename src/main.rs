use eyre::Result;
use std::io::Read;
use uax25::{
    kiss::KissDecoder,
    model::{FrameType, UnnumberedType},
    parser::parse_frame,
};

fn open_stream() -> Result<Box<dyn Read>> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("serial") => {
            let path = args.get(2).map(|s| s.as_str()).unwrap_or("/dev/rfcomm0");
            let baud = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(9600);
            let port = serialport::new(path, baud)
                .timeout(std::time::Duration::from_secs(3600))
                .open()?;
            Ok(Box::new(port))
        }
        Some("tcp") => {
            let addr = args.get(2).map(|s| s.as_str()).unwrap_or("127.0.0.1:8002");
            let stream = std::net::TcpStream::connect(addr)?;
            Ok(Box::new(stream))
        }
        _ => {
            eprintln!("Usage: uax25 <serial|tcp> [address] [baud]");
            eprintln!(
                "  serial [path] [baud]  - connect to serial port (default: /dev/rfcomm0, 9600)"
            );
            eprintln!("  tcp    [addr]         - connect to TCP (default: 127.0.0.1:8002)");
            std::process::exit(1);
        }
    }
}

fn main() -> Result<()> {
    let mut stream = open_stream()?;
    let mut decoder = KissDecoder::default();

    eprintln!("Waiting for data...");
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf)?;
        // eprintln!("Read {} bytes", n);
        if n == 0 {
            break; // connection closed
        }
        // eprintln!("Raw: {:02x?}", &buf[..n]);
        let frames = decoder.feed(&buf[..n]);
        for frame in frames {
            println!(
                "Received KISS frame. Cmd: {:?}, Port {}",
                frame.command, frame.port
            );
            let parsed = parse_frame(&frame.data)?;
            print!("{} --> ", parsed.source,);
            for repeater in parsed.repeaters {
                print!("{}:{} --> ", repeater.address, repeater.has_repeated);
            }
            println!("{}: ", parsed.destination,);
            match parsed.frame {
                FrameType::Unnumbered(uframe) => match uframe.kind {
                    UnnumberedType::UI { pid: _, payload } => {
                        println!(
                            "payload: {}",
                            payload.iter().map(|&b| b as char).collect::<String>()
                        )
                    }
                    _ => {}
                },
                frame => println!("{:?}", frame),
            }
        }
    }

    Ok(())
}
