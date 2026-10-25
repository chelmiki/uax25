//! Wire format for AX.25 frames, shared by the `parse_*` and `encode_*`
//! functions below.
//!
//! ## Address field
//!
//! Each address (destination, source, or repeater) is 7 octets: the
//! first 6 are the callsign, padded with spaces if shorter than 6
//! characters, each shifted left by 1 bit (ASCII only needs 7 bits).
//! Their low bits is unused, always 0. The 7th octet holds, from low
//! to high bit:
//! - bit 0: extension bit (HLDC bit): 0 unless this is the last
//!   address in the field, in which case the control field follows.
//! - bits 1-4: SSID (0-15).
//! - bits 5-6: reserved, set to 1 when transmitting.
//! - bit 7: Command/Response bit on the source address, or the
//!   "has been repeated" bit on a repeater address.
//!
//! The address field as a whole is the destination and source
//! address (always present), followed by up to 8 repeater
//! addresses of the same shape.
//!
//! ## Command/Response bit
//!
//! | Frame Type | Dest. high bit | Source high bit |
//! |------------+-----------------+------------------|
//! | Command    |               1 |                0 |
//! | Response   |               0 |                1 |
//!
//! ## Control field
//!
//! The control field is 1 byte:
//!
//! | Control-Field Type | 7 6 5 | 4   | 3 2 1 |   0 |
//! |--------------------+-------+-----+-------+-----|
//! | I Frame            | N(R)  | P   | N(S)  |   0 |
//! | S Frame            | N(R)  | P/F | S S   | 0 1 |
//! | U Frame            | M M M | P/F | M M   | 1 1 |
//!
//! ## FRMR info field
//!
//! 3 bytes, 24 bits total:
//! - bits 7-0: the rejected frame's control field
//! - bit 8: unused, always 0
//! - bits 11-9: V(S)
//! - bit 12: C/R of the rejected frame (0 = Command, 1 = Response)
//! - bits 15-13: V(R)
//! - bit 16: W
//! - bit 17: X
//! - bit 18: Y
//! - bit 19: Z
//! - bits 23-20: unused, always 0

use crate::{
    Address, CommandResponse, Digipeater, Frame,
    model::{
        FrameRejectionInfo, FrameType, InformationFrame, SupervisoryFrame, SupervisoryType,
        UnnumberedFrame, UnnumberedType,
    },
};

/// Per AX.25 2.0/2.2, an address field carries at most 8 repeater
/// (digipeater) addresses, in addition to the mandatory destination
/// and source.
const MAX_REPEATERS: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum ParserError {
    #[error("address field truncated: expected a 7-byte address")]
    TruncatedAddress,
    #[error("too many repeater addresses (max 8)")]
    TooManyRepeaters,
    #[error("control field truncated: no bytes left for the control field")]
    TruncatedControl,
    #[error("unknown unnumbered frame control byte: {0:#04x}")]
    UnknownUnnumberedOpcode(u8),
    #[error("frame truncated: missing PID byte")]
    MissingPid,
    #[error("FRMR info field truncated: expected 3 bytes")]
    TruncatedFrmrInfo,
}

type Result<T> = std::result::Result<T, ParserError>;

struct AddressFieldWire {
    pub destination: Address,
    pub source: Address,
    pub repeaters: Vec<Digipeater>,
    pub cr: CommandResponse,
}

enum ControlField {
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

enum UnnumberedOpcode {
    Sabm,
    Disc,
    Ua,
    Dm,
    Ui,
    Frmr,
}

struct ParsedAddress {
    address: Address,
    high_bit: bool,
    hldc: bool,
}

pub fn parse_frame(bytes: &[u8]) -> Result<Frame> {
    let (address_field, mut next_bytes) = parse_address_field(bytes)?;

    let (ctrl_field, rest) = parse_control_field(next_bytes)?;
    next_bytes = rest;

    let (frame, poll_final) = match ctrl_field {
        ControlField::Information { ns, nr, poll_final } => {
            let (&pid, payload) = next_bytes.split_first().ok_or(ParserError::MissingPid)?;
            next_bytes = payload;

            (
                FrameType::Information(InformationFrame {
                    ns,
                    nr,
                    pid,
                    payload: next_bytes.to_vec(),
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
                UnnumberedOpcode::Sabm => UnnumberedType::Sabm,
                UnnumberedOpcode::Disc => UnnumberedType::Disc,
                UnnumberedOpcode::Ua => UnnumberedType::Ua,
                UnnumberedOpcode::Ui => {
                    let (&pid, payload) =
                        next_bytes.split_first().ok_or(ParserError::MissingPid)?;
                    next_bytes = payload;

                    UnnumberedType::Ui {
                        pid,
                        payload: next_bytes.to_vec(),
                    }
                }
                UnnumberedOpcode::Frmr => {
                    let frmr_info = parse_frmr_info(next_bytes)?;
                    UnnumberedType::Frmr(frmr_info)
                }
                UnnumberedOpcode::Dm => UnnumberedType::Dm,
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

/// See module docs for the FRMR info field layout.
fn parse_frmr_info(bytes: &[u8]) -> Result<FrameRejectionInfo> {
    let (chunk, _) = bytes
        .split_first_chunk::<3>()
        .ok_or(ParserError::TruncatedFrmrInfo)?;
    let rejected_control = chunk[0];
    let vs = (chunk[1] >> 1) & 0x07;
    let vr = (chunk[1] >> 5) & 0x07;
    let cr_bit = (chunk[1] & 0x10) != 0;
    let rejected_cr = match cr_bit {
        true => CommandResponse::Response,
        false => CommandResponse::Command,
    };
    let w = (chunk[2] & 0x01) != 0;
    let x = (chunk[2] & 0x02) != 0;
    let y = (chunk[2] & 0x04) != 0;
    let z = (chunk[2] & 0x08) != 0;
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

/// See module docs for the control field layout.
fn parse_control_field(bytes: &[u8]) -> Result<(ControlField, &[u8])> {
    let (&ctrl, rest) = bytes.split_first().ok_or(ParserError::TruncatedControl)?;
    let poll_final = ((ctrl >> 4) & 0x01) != 0;
    let bit0 = (ctrl & 0x01) != 0;
    let bit1 = (ctrl & 0x02) != 0;
    let ctrl_field = match (bit0, bit1) {
        (false, _) => {
            let ns: u8 = (ctrl >> 1) & 0x07;
            let nr: u8 = (ctrl >> 5) & 0x07;
            ControlField::Information { ns, nr, poll_final }
        }
        (true, false) => {
            let nr: u8 = (ctrl >> 5) & 0x07;
            let bit2 = (ctrl & 0x04) != 0;
            let bit3 = (ctrl & 0x08) != 0;
            let kind = match (bit2, bit3) {
                (false, false) => SupervisoryType::Rr,
                (false, true) => SupervisoryType::Rej,
                (true, false) => SupervisoryType::Rnr,
                (true, true) => SupervisoryType::Srej,
            };
            ControlField::Supervisory {
                nr,
                poll_final,
                kind,
            }
        }
        (true, true) => {
            let opcode = ctrl & 0b1110_1100;
            let kind = match opcode {
                0b0010_1100 => UnnumberedOpcode::Sabm,
                0b0100_0000 => UnnumberedOpcode::Disc,
                0b0110_0000 => UnnumberedOpcode::Ua,
                0b0000_1100 => UnnumberedOpcode::Dm,
                0b0000_0000 => UnnumberedOpcode::Ui,
                0b1000_0100 => UnnumberedOpcode::Frmr,
                other => return Err(ParserError::UnknownUnnumberedOpcode(other)),
            };
            ControlField::Unnumbered { poll_final, kind }
        }
    };
    Ok((ctrl_field, rest))
}

/// See module docs for the address field layout.
fn parse_address_field(bytes: &[u8]) -> Result<(AddressFieldWire, &[u8])> {
    let (dst_chunk, rest) = bytes
        .split_first_chunk::<7>()
        .ok_or(ParserError::TruncatedAddress)?;
    let dst = parse_address(dst_chunk);

    let (src_chunk, mut rest) = rest
        .split_first_chunk::<7>()
        .ok_or(ParserError::TruncatedAddress)?;
    let src = parse_address(src_chunk);

    let cr = if src.high_bit {
        CommandResponse::Response
    } else {
        CommandResponse::Command
    };

    let mut hldc = src.hldc;
    let mut repeaters = vec![];
    while !hldc {
        if repeaters.len() >= MAX_REPEATERS {
            return Err(ParserError::TooManyRepeaters);
        }
        let (chunk, remaining) = rest
            .split_first_chunk::<7>()
            .ok_or(ParserError::TruncatedAddress)?;
        let addr = parse_address(chunk);
        repeaters.push(Digipeater {
            address: addr.address,
            has_repeated: addr.high_bit,
        });
        hldc = addr.hldc;
        rest = remaining;
    }
    Ok((
        AddressFieldWire {
            destination: dst.address,
            source: src.address,
            repeaters,
            cr,
        },
        rest,
    ))
}

fn parse_address(bytes: &[u8; 7]) -> ParsedAddress {
    let mut call = [b' '; 6];

    for i in 0..6 {
        call[i] = bytes[i] >> 1;
    }

    let ssid = (bytes[6] >> 1) & 0x0F;
    let hldc = (bytes[6] & 0x01) != 0;

    let high_bit = (bytes[6] & 0x80) != 0;
    let address = Address { call, ssid };

    ParsedAddress {
        address,
        hldc,
        high_bit,
    }
}

pub fn encode_frame(frame: &Frame) -> Vec<u8> {
    let mut bytes = Vec::new();

    let (dst_high_bit, src_high_bit) = match frame.command_response {
        CommandResponse::Command => (true, false),
        CommandResponse::Response => (false, true),
    };

    bytes.extend_from_slice(&encode_address(&frame.destination, dst_high_bit, false));
    let mut hldc = frame.repeaters.is_empty();
    bytes.extend_from_slice(&encode_address(&frame.source, src_high_bit, hldc));

    for i in 0..frame.repeaters.len() {
        if i == frame.repeaters.len() - 1 {
            hldc = true
        }
        bytes.extend_from_slice(&encode_address(
            &frame.repeaters[i].address,
            frame.repeaters[i].has_repeated,
            hldc,
        ));
    }
    bytes.push(encode_control(frame.poll_final, &frame.frame));

    bytes.extend(&encode_info_field(&frame.frame));
    bytes
}

/// See module docs for the address field layout.
fn encode_address(address: &Address, high_bit: bool, hldc: bool) -> [u8; 7] {
    let mut address_bytes = [0u8; 7];

    for (i, &b) in address.call.iter().enumerate() {
        address_bytes[i] = b << 1;
    }
    address_bytes[6] = (address.ssid << 1)
        | if hldc { 0x01 } else { 0 }
        | if high_bit { 0x80 } else { 0 }
        | 0b0110_0000;

    address_bytes
}

fn encode_info_field(frame_type: &FrameType) -> Vec<u8> {
    match frame_type {
        FrameType::Information(InformationFrame { pid, payload, .. }) => {
            let mut bytes = Vec::new();
            bytes.push(*pid);
            bytes.extend_from_slice(payload);
            bytes
        }
        FrameType::Unnumbered(UnnumberedFrame {
            kind: UnnumberedType::Ui { pid, payload },
        }) => {
            let mut bytes = Vec::new();
            bytes.push(*pid);
            bytes.extend_from_slice(payload);
            bytes
        }
        FrameType::Unnumbered(UnnumberedFrame {
            kind:
                UnnumberedType::Frmr(FrameRejectionInfo {
                    vs,
                    vr,
                    rejected_cr,
                    rejected_control,
                    w,
                    x,
                    y,
                    z,
                }),
        }) => {
            // See module docs for the FRMR info field layout.
            let mut bytes = Vec::new();
            bytes.push(*rejected_control);
            let rejected_cr_bit = match rejected_cr {
                CommandResponse::Command => 0,
                CommandResponse::Response => 0x10,
            };
            bytes.push((vs << 1) | rejected_cr_bit | (vr << 5));
            bytes.push(
                if *z { 0x08 } else { 0 }
                    | if *y { 0x04 } else { 0 }
                    | if *x { 0x02 } else { 0 }
                    | if *w { 0x01 } else { 0 },
            );
            bytes
        }
        _ => vec![],
    }
}

/// See module docs for the control field layout.
fn encode_control(poll_final: bool, frame_type: &FrameType) -> u8 {
    let mut control = if poll_final { 0x10 } else { 0 };
    control |= match frame_type {
        FrameType::Information(iframe) => (iframe.ns << 1) | (iframe.nr << 5),
        FrameType::Supervisory(sframe) => {
            let kind = match sframe.kind {
                SupervisoryType::Rr => 0b0000_0000,
                SupervisoryType::Rej => 0b0000_1000,
                SupervisoryType::Rnr => 0b0000_0100,
                SupervisoryType::Srej => 0b0000_1100,
            };
            0b0000_0001 | kind | (sframe.nr << 5)
        }
        FrameType::Unnumbered(uframe) => {
            0b0000_0011
                | match uframe.kind {
                    UnnumberedType::Sabm => 0b0010_1100,
                    UnnumberedType::Disc => 0b0100_0000,
                    UnnumberedType::Ua => 0b0110_0000,
                    UnnumberedType::Dm => 0b0000_1100,
                    UnnumberedType::Ui { pid: _, payload: _ } => 0b0000_0000,
                    UnnumberedType::Frmr(_) => 0b1000_0100,
                }
        }
    };
    control
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_address_known_bytes() {
        // "APRS-5", HLDC set, high_bit set.
        let bytes = [0x82, 0xA0, 0xA4, 0xA6, 0x40, 0x40, 0xEB];
        let parsed = parse_address(&bytes);
        assert_eq!(
            parsed.address,
            Address {
                call: *b"APRS  ",
                ssid: 5
            }
        );
        assert!(parsed.hldc);
        assert!(parsed.high_bit);
    }

    #[test]
    fn test_encode_address_known_bytes() {
        let address = Address {
            call: *b"APRS  ",
            ssid: 5,
        };
        let bytes = encode_address(&address, true, true);
        assert_eq!(bytes, [0x82, 0xA0, 0xA4, 0xA6, 0x40, 0x40, 0xEB]);
    }

    #[test]
    fn test_parse_address_field_with_repeater() {
        // destination "DEST-0", source "SRC-1" (high_bit set -> Response,
        // not last), repeater "RPT1-2" (has_repeated set, last -> HLDC
        // set), followed by one trailing byte standing in for the start
        // of the control field.
        let bytes = [
            0x88, 0x8A, 0xA6, 0xA8, 0x40, 0x40, 0x60, // DEST-0
            0xA6, 0xA4, 0x86, 0x40, 0x40, 0x40, 0xE2, // SRC-1
            0xA4, 0xA0, 0xA8, 0x62, 0x40, 0x40, 0xE5, // RPT1-2
            0x7A,
        ];

        let (wire, rest) = parse_address_field(&bytes).unwrap();
        assert_eq!(
            wire.destination,
            Address {
                call: *b"DEST  ",
                ssid: 0
            }
        );
        assert_eq!(
            wire.source,
            Address {
                call: *b"SRC   ",
                ssid: 1
            }
        );
        assert_eq!(wire.cr, CommandResponse::Response);
        assert_eq!(
            wire.repeaters,
            vec![Digipeater {
                address: Address {
                    call: *b"RPT1  ",
                    ssid: 2
                },
                has_repeated: true,
            }]
        );
        assert_eq!(rest, &[0x7A]);
    }

    #[test]
    fn test_parse_address_field_truncated() {
        assert!(matches!(
            parse_address_field(&[0x88, 0x8A, 0xA6]),
            Err(ParserError::TruncatedAddress)
        ));
    }

    #[test]
    fn test_parse_address_field_too_many_repeaters() {
        let dest = encode_address(
            &Address {
                call: *b"DEST  ",
                ssid: 0,
            },
            false,
            false,
        );
        let src = encode_address(
            &Address {
                call: *b"SRC   ",
                ssid: 0,
            },
            false,
            false,
        );
        // None of these set HLDC, so the field never terminates within
        // MAX_REPEATERS.
        let repeater = encode_address(
            &Address {
                call: *b"RPT   ",
                ssid: 0,
            },
            false,
            false,
        );

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&dest);
        bytes.extend_from_slice(&src);
        for _ in 0..=MAX_REPEATERS {
            bytes.extend_from_slice(&repeater);
        }

        assert!(matches!(
            parse_address_field(&bytes),
            Err(ParserError::TooManyRepeaters)
        ));
    }

    #[test]
    fn test_parse_control_field_information() {
        // I-frame, N(R)=3, P=1, N(S)=5.
        let (field, rest) = parse_control_field(&[0x7A, 0xFF]).unwrap();
        assert!(matches!(
            field,
            ControlField::Information {
                ns: 5,
                nr: 3,
                poll_final: true
            }
        ));
        assert_eq!(rest, &[0xFF]);
    }

    #[test]
    fn test_encode_control_information() {
        let frame_type = FrameType::Information(InformationFrame {
            ns: 5,
            nr: 3,
            pid: 0,
            payload: vec![],
        });
        assert_eq!(encode_control(true, &frame_type), 0x7A);
    }

    #[test]
    fn test_parse_control_field_supervisory_rej() {
        let (field, _) = parse_control_field(&[0x49]).unwrap();
        assert!(matches!(
            field,
            ControlField::Supervisory {
                nr: 2,
                poll_final: false,
                kind: SupervisoryType::Rej
            }
        ));
    }

    #[test]
    fn test_encode_control_supervisory_rej() {
        let frame_type = FrameType::Supervisory(SupervisoryFrame {
            kind: SupervisoryType::Rej,
            nr: 2,
        });
        assert_eq!(encode_control(false, &frame_type), 0x49);
    }

    #[test]
    fn test_parse_control_field_unnumbered_sabm() {
        let (field, _) = parse_control_field(&[0x3F]).unwrap();
        assert!(matches!(
            field,
            ControlField::Unnumbered {
                poll_final: true,
                kind: UnnumberedOpcode::Sabm
            }
        ));
    }

    #[test]
    fn test_parse_control_field_truncated() {
        assert!(matches!(
            parse_control_field(&[]),
            Err(ParserError::TruncatedControl)
        ));
    }

    #[test]
    fn test_parse_control_field_unknown_opcode() {
        assert!(matches!(
            parse_control_field(&[0xFF]),
            Err(ParserError::UnknownUnnumberedOpcode(0xEC))
        ));
    }

    #[test]
    fn test_parse_frmr_info_known_bytes() {
        // rejected_control=0x7A, V(S)=3, V(R)=5, C/R=Response, W=1, X=0,
        // Y=1, Z=0.
        let info = parse_frmr_info(&[0x7A, 0xB6, 0x05]).unwrap();
        assert_eq!(
            info,
            FrameRejectionInfo {
                rejected_control: 0x7A,
                vs: 3,
                vr: 5,
                rejected_cr: CommandResponse::Response,
                w: true,
                x: false,
                y: true,
                z: false,
            }
        );
    }

    #[test]
    fn test_encode_frmr_info_known_bytes() {
        let frame_type = FrameType::Unnumbered(UnnumberedFrame {
            kind: UnnumberedType::Frmr(FrameRejectionInfo {
                rejected_control: 0x7A,
                vs: 3,
                vr: 5,
                rejected_cr: CommandResponse::Response,
                w: true,
                x: false,
                y: true,
                z: false,
            }),
        });
        assert_eq!(encode_info_field(&frame_type), vec![0x7A, 0xB6, 0x05]);
    }

    #[test]
    fn test_parse_frmr_info_truncated() {
        assert!(matches!(
            parse_frmr_info(&[0x7A, 0xB6]),
            Err(ParserError::TruncatedFrmrInfo)
        ));
    }

    // --- Full frame round trips -----------------------------------------

    #[test]
    fn test_round_trip_information_frame() {
        let frame = Frame {
            destination: Address {
                call: *b"DEST  ",
                ssid: 0,
            },
            source: Address {
                call: *b"SRC   ",
                ssid: 1,
            },
            repeaters: vec![Digipeater {
                address: Address {
                    call: *b"RPT1  ",
                    ssid: 2,
                },
                has_repeated: false,
            }],
            command_response: CommandResponse::Command,
            poll_final: true,
            frame: FrameType::Information(InformationFrame {
                ns: 3,
                nr: 5,
                pid: 0xF0,
                payload: b"hello".to_vec(),
            }),
        };

        let bytes = encode_frame(&frame);
        assert_eq!(parse_frame(&bytes).unwrap(), frame);
    }

    #[test]
    fn test_round_trip_supervisory_frame_all_kinds() {
        for kind in [
            SupervisoryType::Rr,
            SupervisoryType::Rnr,
            SupervisoryType::Rej,
            SupervisoryType::Srej,
        ] {
            let frame = Frame {
                destination: Address {
                    call: *b"DEST  ",
                    ssid: 0,
                },
                source: Address {
                    call: *b"SRC   ",
                    ssid: 1,
                },
                repeaters: vec![],
                command_response: CommandResponse::Response,
                poll_final: false,
                frame: FrameType::Supervisory(SupervisoryFrame { kind, nr: 4 }),
            };

            let bytes = encode_frame(&frame);
            assert_eq!(parse_frame(&bytes).unwrap(), frame);
        }
    }

    #[test]
    fn test_round_trip_unnumbered_frame_simple_kinds() {
        for kind in [
            UnnumberedType::Sabm,
            UnnumberedType::Disc,
            UnnumberedType::Ua,
            UnnumberedType::Dm,
        ] {
            let frame = Frame {
                destination: Address {
                    call: *b"DEST  ",
                    ssid: 0,
                },
                source: Address {
                    call: *b"SRC   ",
                    ssid: 1,
                },
                repeaters: vec![],
                command_response: CommandResponse::Command,
                poll_final: true,
                frame: FrameType::Unnumbered(UnnumberedFrame { kind }),
            };

            let bytes = encode_frame(&frame);
            assert_eq!(parse_frame(&bytes).unwrap(), frame);
        }
    }

    #[test]
    fn test_round_trip_unnumbered_ui_frame() {
        let frame = Frame {
            destination: Address {
                call: *b"APRS  ",
                ssid: 0,
            },
            source: Address {
                call: *b"SRC   ",
                ssid: 1,
            },
            repeaters: vec![],
            command_response: CommandResponse::Command,
            poll_final: false,
            frame: FrameType::Unnumbered(UnnumberedFrame {
                kind: UnnumberedType::Ui {
                    pid: 0xF0,
                    payload: b"beacon".to_vec(),
                },
            }),
        };

        let bytes = encode_frame(&frame);
        assert_eq!(parse_frame(&bytes).unwrap(), frame);
    }

    #[test]
    fn test_round_trip_unnumbered_frmr_frame() {
        let frame = Frame {
            destination: Address {
                call: *b"DEST  ",
                ssid: 0,
            },
            source: Address {
                call: *b"SRC   ",
                ssid: 1,
            },
            repeaters: vec![],
            command_response: CommandResponse::Response,
            poll_final: true,
            frame: FrameType::Unnumbered(UnnumberedFrame {
                kind: UnnumberedType::Frmr(FrameRejectionInfo {
                    rejected_control: 0x7A,
                    vs: 3,
                    vr: 5,
                    rejected_cr: CommandResponse::Response,
                    w: true,
                    x: false,
                    y: true,
                    z: false,
                }),
            }),
        };

        let bytes = encode_frame(&frame);
        assert_eq!(parse_frame(&bytes).unwrap(), frame);
    }

    #[test]
    fn test_parse_frame_missing_pid() {
        let frame = Frame {
            destination: Address {
                call: *b"DEST  ",
                ssid: 0,
            },
            source: Address {
                call: *b"SRC   ",
                ssid: 1,
            },
            repeaters: vec![],
            command_response: CommandResponse::Command,
            poll_final: false,
            frame: FrameType::Information(InformationFrame {
                ns: 0,
                nr: 0,
                pid: 0,
                payload: vec![],
            }),
        };

        // Drop the trailing PID byte, leaving nothing after the control
        // byte.
        let mut bytes = encode_frame(&frame);
        bytes.truncate(bytes.len() - 1);
        assert!(matches!(parse_frame(&bytes), Err(ParserError::MissingPid)));
    }
}
