use std::fmt;

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
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

impl std::convert::TryFrom<&str> for Address {
    type Error = eyre::Report;
    fn try_from(call: &str) -> eyre::Result<Address> {
        let parts: Vec<_> = call.split('-').collect();
        if parts.len() != 2 {
            eyre::bail!("Invalid Address: {:?}", call);
        }
        let callsign = parts[0].as_bytes();
        if callsign.is_empty() || callsign.len() > 6 {
            eyre::bail!("Invalid callsign length: {:?}", parts[0]);
        }
        let mut call_bytes = [b' '; 6];
        call_bytes[..callsign.len()].copy_from_slice(callsign);

        let ssid: u8 = parts[1]
            .parse()
            .map_err(|_| eyre::eyre!("Invalid SSID: {:?}", parts[1]))?;
        if ssid > 15 {
            eyre::bail!("SSID out of range (0-15): {}", ssid);
        }

        Ok(Address {
            call: call_bytes,
            ssid,
        })
    }
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct Digipeater {
    pub address: Address,
    pub has_repeated: bool,
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum CommandResponse {
    Command,
    Response,
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct InformationFrame {
    pub ns: u8,
    pub nr: u8,
    pub pid: u8,
    pub payload: Vec<u8>,
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct SupervisoryFrame {
    pub kind: SupervisoryType,
    pub nr: u8,
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub struct UnnumberedFrame {
    pub kind: UnnumberedType,
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum FrameType {
    Information(InformationFrame),
    Supervisory(SupervisoryFrame),
    Unnumbered(UnnumberedFrame),
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum SupervisoryType {
    RR,
    RNR,
    REJ,
    SREJ,
}

#[derive(Debug, Eq, PartialEq, Clone)]
pub enum UnnumberedType {
    SABM,
    DISC,
    UA,
    DM,
    UI { pid: u8, payload: Vec<u8> },
    FRMR(FrameRejectionInfo),
}

#[derive(Debug, Eq, PartialEq, Clone)]
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
#[derive(Debug, Eq, PartialEq, Clone)]
pub struct Frame {
    pub source: Address,
    pub destination: Address,
    pub repeaters: Vec<Digipeater>, // Up to 8
    pub command_response: CommandResponse,
    pub poll_final: bool,
    pub frame: FrameType,
}
