use std::fmt;

#[derive(Debug)]
pub struct Address {
    pub call: [u8; 6], // Up to 6 ascii chars (using space if less than 6)
    pub ssid: u8,
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let call = self
            .call
            .iter()
            .map(|&b| b as char)
            .take_while(|&c| c != ' ')
            .collect::<String>();

        write!(f, "{}-{}", call, self.ssid)
    }
}

#[derive(Debug)]
pub struct Digipeater {
    pub address: Address,
    pub has_repeated: bool,
}

#[derive(Debug)]
pub enum CommandResponse {
    Command,
    Response,
}

#[derive(Debug)]
pub struct InformationFrame {
    pub ns: u8,
    pub nr: u8,
    pub pid: u8,
    pub payload: Vec<u8>,
}

#[derive(Debug)]
pub struct SupervisoryFrame {
    pub kind: SupervisoryType,
    pub nr: u8,
}

#[derive(Debug)]
pub struct UnnumberedFrame {
    pub kind: UnnumberedType,
}

#[derive(Debug)]
pub enum FrameType {
    Information(InformationFrame),
    Supervisory(SupervisoryFrame),
    Unnumbered(UnnumberedFrame),
}

#[derive(Debug)]
pub enum SupervisoryType {
    RR,
    RNR,
    REJ,
    SREJ,
}

#[derive(Debug)]
pub enum UnnumberedType {
    SABM,
    DISC,
    UA,
    DM,
    UI { pid: u8, payload: Vec<u8> },
    FRMR(FrameRejectionInfo),
}

#[derive(Debug)]
pub struct FrameRejectionInfo {
    pub vs: u8,
    pub vr: u8,
    pub rejected_cr: CommandResponse,
    pub rejected_control: u8, // the original control byte
    pub w: bool,
    pub x: bool,
    pub y: bool,
    pub z: bool,
}
#[derive(Debug)]
pub struct Frame {
    pub source: Address,
    pub destination: Address,
    pub repeaters: Vec<Digipeater>, // Up to 8
    pub command_response: CommandResponse,
    pub poll_final: bool,
    pub frame: FrameType,
}
