use crate::{
    model::{
        FrameRejectionInfo, FrameType, InformationFrame, SupervisoryFrame, SupervisoryType,
        UnnumberedFrame, UnnumberedType,
    },
    Address, CommandResponse, Digipeater, Frame,
};
use eyre::{bail, Result, WrapErr};

pub struct AddressFieldWire {
    pub destination: Address,
    pub source: Address,
    pub repeaters: Vec<Digipeater>,
    pub cr: CommandResponse,
}

pub enum ControlField {
    Information {
        ns: u8,
        nr: u8,
        poll_final: bool,
    },
    Supervisory {
        nr: u8,
        poll_final: bool,
        kind: SupervisoryType,
    },
    Unnumbered {
        poll_final: bool,
        kind: UnnumberedOpcode,
    },
}

pub enum UnnumberedOpcode {
    SABM,
    DISC,
    UA,
    DM,
    UI,
    FRMR,
}

struct ParsedAddress {
    address: Address,
    high_bit: bool,
    hldc: bool,
}

pub fn parse_frame(bytes: &[u8]) -> Result<Frame> {
    let (address_field, mut next_bytes) =
        parse_address_field(bytes).wrap_err("Failed to parse AX.25 address field")?;

    let (ctrl_field, rest) =
        parse_control_field(next_bytes).wrap_err("Failed to parse AX.25 control field")?;
    next_bytes = rest;

    let (frame, poll_final) = match ctrl_field {
        ControlField::Information { ns, nr, poll_final } => {
            let pid = next_bytes[0]; //ensure it exists
            next_bytes = &next_bytes[1..];

            let payload = next_bytes.to_vec();

            (
                FrameType::Information(InformationFrame {
                    ns,
                    nr,
                    pid,
                    payload,
                }),
                poll_final,
            )
        }
        ControlField::Supervisory {
            nr,
            poll_final,
            kind,
        } => (
            FrameType::Supervisory(SupervisoryFrame { kind, nr }),
            poll_final,
        ),
        ControlField::Unnumbered { poll_final, kind } => {
            let kind = match kind {
                UnnumberedOpcode::SABM => UnnumberedType::SABM,
                UnnumberedOpcode::DISC => UnnumberedType::DISC,
                UnnumberedOpcode::UA => UnnumberedType::UA,
                UnnumberedOpcode::UI => {
                    let pid = next_bytes[0];
                    next_bytes = &next_bytes[1..];
                    let payload = next_bytes.to_vec();

                    UnnumberedType::UI { pid, payload }
                }
                UnnumberedOpcode::FRMR => {
                    let frmr_info = parse_frmr_info(next_bytes)?;
                    UnnumberedType::FRMR(frmr_info)
                }
                UnnumberedOpcode::DM => UnnumberedType::DM,
            };
            (FrameType::Unnumbered(UnnumberedFrame { kind }), poll_final)
        }
    };

    Ok(Frame {
        source: address_field.source,
        destination: address_field.destination,
        repeaters: address_field.repeaters,
        command_response: address_field.cr,
        poll_final,
        frame,
    })
}

pub fn parse_frmr_info(bytes: &[u8]) -> Result<FrameRejectionInfo> {
    let rejected_control = bytes[0];
    let vs = (bytes[1] >> 1) & 0x07;
    let vr = (bytes[1] >> 5) & 0x07;
    let cr_bit = (bytes[1] & 0x10) != 0;
    let rejected_cr = match cr_bit {
        true => CommandResponse::Response,
        false => CommandResponse::Command,
    };
    let w = (bytes[2] & 0x01) != 0;
    let x = (bytes[2] & 0x02) != 0;
    let y = (bytes[2] & 0x04) != 0;
    let z = (bytes[2] & 0x08) != 0;
    Ok(FrameRejectionInfo {
        rejected_control,
        vs,
        vr,
        rejected_cr,
        w,
        x,
        y,
        z,
    })
}

pub fn parse_control_field(bytes: &[u8]) -> Result<(ControlField, &[u8])> {
    let ctrl = bytes[0];
    let poll_final = ((ctrl >> 4) & 0x01) != 0;
    let bit0 = (ctrl & 0x01) != 0;
    let bit1 = (ctrl & 0x02) != 0;
    let ctrl_field = match (bit0, bit1) {
        (false, _) => {
            log::debug!("It is an IFrame");
            let ns: u8 = (ctrl >> 1) & 0x07;
            let nr: u8 = (ctrl >> 5) & 0x07;
            ControlField::Information { ns, nr, poll_final }
        }
        (true, false) => {
            log::debug!("It is a SFrame");
            let nr: u8 = (ctrl >> 5) & 0x07;
            let bit2 = (ctrl & 0x04) != 0;
            let bit3 = (ctrl & 0x08) != 0;
            let kind = match (bit2, bit3) {
                (false, false) => SupervisoryType::RR,
                (false, true) => SupervisoryType::REJ,
                (true, false) => SupervisoryType::RNR,
                (true, true) => SupervisoryType::SREJ,
            };
            ControlField::Supervisory {
                nr,
                poll_final,
                kind,
            }
        }
        (true, true) => {
            log::debug!("It is an UFrame");
            let opcode = ctrl & 0b1110_1100;
            let kind = match opcode {
                0b0010_1100 => UnnumberedOpcode::SABM,
                0b0100_0000 => UnnumberedOpcode::DISC,
                0b0110_0000 => UnnumberedOpcode::UA,
                0b0000_1100 => UnnumberedOpcode::DM,
                0b0000_0000 => UnnumberedOpcode::UI,
                0b1000_0100 => UnnumberedOpcode::FRMR,
                _ => bail!("unknown unnumbered frame"),
            };
            ControlField::Unnumbered { poll_final, kind }
        }
    };
    Ok((ctrl_field, &bytes[1..]))
}

pub fn parse_address_field(bytes: &[u8]) -> Result<(AddressFieldWire, &[u8])> {
    let dst = parse_single_address(&bytes[..7])?;
    let src = parse_single_address(&bytes[7..14])?;
    let cr = if src.high_bit {
        CommandResponse::Response
    } else {
        CommandResponse::Command
    };
    // let cr = match (dst.high_bit, src.high_bit) {
    //     (true, false) => CommandResponse::Command,
    //     (false, true) => CommandResponse::Response,
    //     _ => bail!("invalid command/response bits"),
    //     // Other combination could be valid if we implement backwards compatibility
    // };
    let mut hldc = src.hldc;
    let mut next_byte = 14;
    let mut repeaters = vec![];
    while !hldc {
        let addr = parse_single_address(&bytes[next_byte..next_byte + 7])?;
        repeaters.push(Digipeater {
            address: addr.address,
            has_repeated: addr.high_bit,
        });
        // TODO: We should check we don't keep parsing repeaters forever
        hldc = addr.hldc;
        next_byte += 7;
    }
    Ok((
        AddressFieldWire {
            destination: dst.address,
            source: src.address,
            repeaters,
            cr,
        },
        &bytes[next_byte..],
    ))
}

fn parse_single_address(bytes: &[u8]) -> Result<ParsedAddress> {
    if bytes.len() < 7 {
        bail!("Address too short");
    }

    let mut call = [b' '; 6];

    for i in 0..6 {
        call[i] = bytes[i] >> 1;
    }

    let ssid = (bytes[6] >> 1) & 0x0F;
    let hldc = (bytes[6] & 0x01) != 0;
    // Extract the higest bit of the 7th byte
    let high_bit = (bytes[6] & 0x80) != 0;
    let address = Address { call, ssid };

    Ok(ParsedAddress {
        address,
        hldc,
        high_bit,
    })
}
