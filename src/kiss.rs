use eyre::{Result, bail};

const FEND: u8 = 0xC0; // Frame End
const FESC: u8 = 0xDB; // Frame Escape
const TFEND: u8 = 0xDC; // Transposed Frame End
const TFESC: u8 = 0xDD; // Transposed Frame Escape

pub struct KissFrame {
    pub port: u8,
    pub command: KissTncCmd,
    pub data: Vec<u8>,
}

#[derive(Debug)]
pub enum KissTncCmd {
    Data,
    TxDelay,
    P,
    SlotTime,
    TxTail,
    FullDuplex,
    SetHardware,
    Return,
}

impl TryFrom<u8> for KissTncCmd {
    type Error = eyre::Report;

    fn try_from(value: u8) -> Result<Self> {
        match value & 0x0F {
            0 => Ok(KissTncCmd::Data),
            1 => Ok(KissTncCmd::TxDelay),
            2 => Ok(KissTncCmd::P),
            3 => Ok(KissTncCmd::SlotTime),
            4 => Ok(KissTncCmd::TxTail),
            5 => Ok(KissTncCmd::FullDuplex),
            6 => Ok(KissTncCmd::SetHardware),
            7 => Ok(KissTncCmd::Return),
            _ => {
                bail!("Invalid KISS TNC Command");
            }
        }
    }
}

#[derive(Default)]
pub struct KissDecoder {
    buffer: Vec<u8>,
}

impl KissDecoder {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<KissFrame> {
        self.buffer.extend(bytes);

        let mut frames = vec![];

        let mut frame_start: Option<usize> = None;
        let mut last_consumed = 0;

        for (pos, byte) in self.buffer.iter().enumerate() {
            if *byte != FEND {
                continue;
            }

            last_consumed = pos;

            match frame_start {
                None => frame_start = Some(pos + 1),
                Some(start) if start == pos => frame_start = Some(pos + 1),
                Some(start) => {
                    let frame_bytes = &self.buffer[start..pos];
                    match Self::decode(frame_bytes) {
                        Ok(frame) => frames.push(frame),
                        Err(e) => log::error!("Invalid frame: {:?}", e),
                    };
                    frame_start = Some(pos + 1);
                }
            }
        }
        self.buffer.drain(..last_consumed);
        frames
    }

    fn unescape(input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len());
        let mut i = 0;

        while i < input.len() {
            match input[i] {
                FESC => {
                    i += 1;
                    if i >= input.len() {
                        break; // or error
                    }

                    match input[i] {
                        TFEND => out.push(FEND),
                        TFESC => out.push(FESC),
                        _ => {
                            // invalid escape sequence
                        }
                    }
                }
                b => out.push(b),
            }
            i += 1;
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<KissFrame> {
        let type_byte = bytes[0];
        let cmd_byte = type_byte & 0x0F;
        let command = cmd_byte.try_into()?;
        let port = type_byte >> 4;
        let data = Self::unescape(&bytes[1..]);
        Ok(KissFrame {
            port,
            command,
            data,
        })
    }
}
