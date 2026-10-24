//! Encodes/decodes the KISS framing protocol between a host and a TNC:
//! `FEND` delimited frames, escaping of `FEND`/`FESC` within a frame's
//! body, and the leading type byte (channel + command).
//!
const FEND: u8 = 0xC0; // Frame End
const FESC: u8 = 0xDB; // Frame Escape
const TFEND: u8 = 0xDC; // Transposed Frame End
const TFESC: u8 = 0xDD; // Transposed Frame Escape

#[derive(Debug, thiserror::Error)]
pub enum KissError {
    #[error("invalid KISS TNC command nibble: {0}")]
    InvalidCommand(u8),
    #[error("truncated escape sequence: FESC at end of frame")]
    TruncatedEscape,
    #[error("invalid escape sequence: FESC followed by {0:#04x}")]
    InvalidEscape(u8),
    #[error("{0:?} requires exactly 1 payload byte, got {1}")]
    WrongPayloadLength(KissTncCmd, usize),
    #[error("Return carries no payload, got {0} bytes")]
    ReturnHasPayload(usize),
}

type Result<T> = std::result::Result<T, KissError>;

pub struct KissFrame {
    pub channel: u8,
    pub command: KissTncCmd,
    pub data: Vec<u8>,
}

/// Per the KISS spec, `Data` is the only command a TNC ever sends to the
/// host. `TxDelay`/`P`/`SlotTime`/`TxTail`/`FullDuplex`/`SetHardware`/
/// `Return` only flow host -> TNC. So in practice `KissDecoder` should only
/// ever produce `Data`.
#[derive(Debug, Clone, Copy)]
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
    type Error = KissError;

    fn try_from(value: u8) -> Result<Self> {
        // `Return` is deliberately not handled here: it's the standalone
        // byte 0xFF, not a command nibble combined with a channel
        // number like the other commands.
        match value & 0x0F {
            0 => Ok(KissTncCmd::Data),
            1 => Ok(KissTncCmd::TxDelay),
            2 => Ok(KissTncCmd::P),
            3 => Ok(KissTncCmd::SlotTime),
            4 => Ok(KissTncCmd::TxTail),
            5 => Ok(KissTncCmd::FullDuplex),
            6 => Ok(KissTncCmd::SetHardware),
            other => Err(KissError::InvalidCommand(other)),
        }
    }
}

impl From<KissTncCmd> for u8 {
    fn from(cmd: KissTncCmd) -> u8 {
        match cmd {
            KissTncCmd::Data => 0,
            KissTncCmd::TxDelay => 1,
            KissTncCmd::P => 2,
            KissTncCmd::SlotTime => 3,
            KissTncCmd::TxTail => 4,
            KissTncCmd::FullDuplex => 5,
            KissTncCmd::SetHardware => 6,
            // Return is encoded using the whole byte.
            KissTncCmd::Return => 0xFF,
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

    fn unescape(input: &[u8]) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(input.len());
        let mut i = 0;

        while i < input.len() {
            match input[i] {
                FESC => {
                    i += 1;
                    if i >= input.len() {
                        return Err(KissError::TruncatedEscape);
                    }

                    match input[i] {
                        TFEND => out.push(FEND),
                        TFESC => out.push(FESC),
                        other => {
                            return Err(KissError::InvalidEscape(other));
                        }
                    }
                }
                b => out.push(b),
            }
            i += 1;
        }
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<KissFrame> {
        let type_byte = bytes[0];

        // Return is a standalone sentinel byte (no channel, or
        // trailing data), so it has to be checked before the generic
        // channel/command split below.
        if type_byte == 0xFF {
            return Ok(KissFrame {
                channel: 0,
                command: KissTncCmd::Return,
                data: vec![],
            });
        }

        let cmd_byte = type_byte & 0x0F;
        let command = cmd_byte.try_into()?;
        let channel = type_byte >> 4;
        let data = Self::unescape(&bytes[1..])?;
        Ok(KissFrame {
            channel,
            command,
            data,
        })
    }
}

pub struct KissEncoder {
    channel: u8,
}

impl KissEncoder {
    pub fn new(channel: u8) -> Self {
        Self { channel }
    }

    fn escape(input: &[u8]) -> Vec<u8> {
        let mut out: Vec<u8> = vec![];

        for &b in input {
            match b {
                FESC => {
                    out.push(FESC);
                    out.push(TFESC);
                }
                FEND => {
                    out.push(FESC);
                    out.push(TFEND);
                }
                b => out.push(b),
            }
        }
        out
    }

    /// `TxDelay`/`P`/`SlotTime`/`TxTail`/`FullDuplex` are each specified as
    /// exactly one payload byte; `Data`/`SetHardware` are variable-length
    /// by spec, so nothing fixed to check. `Return` carries no payload at all.
    pub fn encode(&self, command: KissTncCmd, input: &[u8]) -> Result<Vec<u8>> {
        match command {
            KissTncCmd::TxDelay
            | KissTncCmd::P
            | KissTncCmd::SlotTime
            | KissTncCmd::TxTail
            | KissTncCmd::FullDuplex => {
                if input.len() != 1 {
                    return Err(KissError::WrongPayloadLength(command, input.len()));
                }
            }
            KissTncCmd::Data | KissTncCmd::SetHardware => {}
            KissTncCmd::Return => {
                if !input.is_empty() {
                    return Err(KissError::ReturnHasPayload(input.len()));
                }
            }
        }

        let cmd_byte: u8 = command.into();
        let channel_byte = (self.channel & 0x0F) << 4;

        let type_byte = cmd_byte | channel_byte;
        let data = Self::escape(input);

        let mut result = vec![FEND, type_byte];
        result.extend(data);
        result.push(FEND);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_unescape_round_trip() {
        let input = vec![0x01, FEND, 0x02, FESC, 0x03, FEND, FESC];
        let escaped = KissEncoder::escape(&input);

        // Escaping must remove every literal FEND from the body.
        assert!(!escaped.contains(&FEND));

        let unescaped = KissDecoder::unescape(&escaped).unwrap();
        assert_eq!(unescaped, input);
    }

    #[test]
    fn test_unescape_rejects_truncated_escape_sequence() {
        // A dangling FESC with nothing after is corrupted input, since
        // by the time unescape runs the frame is already bounded by two
        // real FEND delimiters.
        assert!(KissDecoder::unescape(&[0x01, FESC]).is_err());
    }

    #[test]
    fn test_unescape_rejects_invalid_escape_sequence() {
        // FESC followed by anything other than TFEND/TFESC.
        assert!(KissDecoder::unescape(&[0x01, FESC, 0x02]).is_err());
    }

    #[test]
    fn test_encode_decode_round_trip_plain() {
        let payload = b"Test AX.25".to_vec();
        let encoder = KissEncoder::new(0);
        let encoded = encoder.encode(KissTncCmd::Data, &payload).unwrap();

        let mut decoder = KissDecoder::default();
        let frames = decoder.feed(&encoded);

        assert_eq!(frames.len(), 1);
        assert!(matches!(frames[0].command, KissTncCmd::Data));
        assert_eq!(frames[0].channel, 0);
        assert_eq!(frames[0].data, payload);
    }

    #[test]
    fn test_encode_decode_round_trip_needs_escaping() {
        // Payloadvcontains bytes that need escaping.
        let payload = vec![0xAA, FEND, 0xBB, FESC, 0xCC];
        let encoder = KissEncoder::new(0);
        let encoded = encoder.encode(KissTncCmd::Data, &payload).unwrap();

        let mut decoder = KissDecoder::default();
        let frames = decoder.feed(&encoded);

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, payload);
    }

    #[test]
    fn test_encode_uses_given_channel() {
        let payload = b"on channel 3".to_vec();
        let encoder = KissEncoder::new(3);
        let encoded = encoder.encode(KissTncCmd::Data, &payload).unwrap();

        let mut decoder = KissDecoder::default();
        let frames = decoder.feed(&encoded);

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].channel, 3);
    }

    #[test]
    fn test_feed_frame_split_across_two_calls() {
        let payload = b"split me".to_vec();
        let encoder = KissEncoder::new(0);
        let encoded = encoder.encode(KissTncCmd::Data, &payload).unwrap();
        let (first_half, second_half) = encoded.split_at(encoded.len() / 2);

        let mut decoder = KissDecoder::default();

        // Nothing to emit yet: the terminating FEND hasn't arrived.
        let frames = decoder.feed(first_half);
        assert!(frames.is_empty());

        let frames = decoder.feed(second_half);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, payload);
    }

    #[test]
    fn test_feed_multiple_frames_in_one_call() {
        let payload_a = b"frame a".to_vec();
        let payload_b = b"frame b".to_vec();
        let encoder = KissEncoder::new(0);

        let mut combined = encoder.encode(KissTncCmd::Data, &payload_a).unwrap();
        combined.extend(encoder.encode(KissTncCmd::Data, &payload_b).unwrap());

        let mut decoder = KissDecoder::default();
        let frames = decoder.feed(&combined);

        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].data, payload_a);
        assert_eq!(frames[1].data, payload_b);
    }

    #[test]
    fn test_feed_tolerates_back_to_back_fend_idle_fill() {
        let payload = b"after idle fill".to_vec();
        let encoder = KissEncoder::new(0);
        let encoded = encoder.encode(KissTncCmd::Data, &payload).unwrap();

        // Prepend extra idle FENDs, as a TNC commonly sends between frames.
        let mut noisy = vec![FEND, FEND, FEND];
        noisy.extend(encoded);

        let mut decoder = KissDecoder::default();
        let frames = decoder.feed(&noisy);

        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, payload);
    }

    #[test]
    fn test_encode_decode_non_data_command() {
        let encoder = KissEncoder::new(0);
        let encoded = encoder.encode(KissTncCmd::TxDelay, &[50]).unwrap();

        let mut decoder = KissDecoder::default();
        let frames = decoder.feed(&encoded);

        assert_eq!(frames.len(), 1);
        assert!(matches!(frames[0].command, KissTncCmd::TxDelay));
        assert_eq!(frames[0].data, vec![50]);
    }

    #[test]
    fn test_encode_rejects_wrong_payload_length_for_fixed_size_commands() {
        let encoder = KissEncoder::new(0);
        assert!(encoder.encode(KissTncCmd::TxDelay, &[1, 2]).is_err());
        assert!(encoder.encode(KissTncCmd::TxDelay, &[]).is_err());
        assert!(encoder.encode(KissTncCmd::TxDelay, &[50]).is_ok());
    }

    #[test]
    fn test_encode_rejects_return_with_payload() {
        let encoder = KissEncoder::new(0);
        assert!(encoder.encode(KissTncCmd::Return, &[1]).is_err());
    }

    #[test]
    fn test_decode_return_command() {
        // Return is the standalone 0xFF sentinel, not a channel+command
        // pair.
        let frame = KissDecoder::decode(&[0xFF]).unwrap();
        assert!(matches!(frame.command, KissTncCmd::Return));
        assert!(frame.data.is_empty());
    }

    #[test]
    fn test_decode_rejects_undefined_command_nibble() {
        // Nibble 7 is undefined in the KISS spec. It must not be confused
        // with Return, which only exists as the whole byte 0xFF, not as
        // channel 0 / command nibble 7.
        assert!(KissDecoder::decode(&[0x07]).is_err());
    }
}
