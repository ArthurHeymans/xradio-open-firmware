//! 802.11 frame headers as zerocopy views over received frames.

use zerocopy::byteorder::little_endian::U16;
use zerocopy::{FromBytes, Immutable, KnownLayout};

/// Frame-control type and subtype bits (protocol version zero).
pub const TYPE_SUBTYPE: u16 = 0x00fc;
pub const BLOCK_ACK_REQUEST: u16 = 0x0084;
pub const QOS_DATA: u16 = 0x0088;

const TYPE: u16 = 0x000c;
const DATA: u16 = 0x0008;
const DS: u16 = 0x0300;
const FROM_DS: u16 = 0x0200;
const TO_DS: u16 = 0x0100;
const MORE_FRAGMENTS: u16 = 0x0400;

/// A data or management frame's header up to the sequence control.
#[derive(FromBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct Header {
    pub frame_control: U16,
    pub duration: U16,
    pub addr1: [u8; 6],
    pub addr2: [u8; 6],
    pub addr3: [u8; 6],
    pub sequence_control: U16,
}

impl Header {
    pub fn parse(frame: &[u8]) -> Option<&Self> {
        Self::ref_from_prefix(frame).ok().map(|(header, _)| header)
    }

    pub fn type_subtype(&self) -> u16 {
        self.frame_control.get() & TYPE_SUBTYPE
    }

    pub fn is_data(&self) -> bool {
        self.frame_control.get() & TYPE == DATA
    }

    /// Sent by the AP to its stations.
    pub fn from_ds(&self) -> bool {
        self.frame_control.get() & DS == FROM_DS
    }

    /// Sent by a station to its AP.
    pub fn to_ds(&self) -> bool {
        self.frame_control.get() & DS == TO_DS
    }

    /// The sender holds more frames for the receiver.
    pub fn more_data(&self) -> bool {
        self.frame_control.get() & 0x2000 != 0
    }

    pub fn group_addressed(&self) -> bool {
        self.addr1[0] & 1 != 0
    }
}

/// A QoS data frame's header (four-address frames are not used here).
#[derive(FromBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct QosData {
    pub header: Header,
    pub qos_control: U16,
}

impl QosData {
    /// A QoS data frame with payload, not fragmented.
    pub fn parse(frame: &[u8]) -> Option<&Self> {
        let (data, _) = Self::ref_from_prefix(frame).ok()?;
        let control = data.header.frame_control.get();
        (control & (TYPE_SUBTYPE | MORE_FRAGMENTS) == QOS_DATA
            && data.header.sequence_control.get() & 0x000f == 0)
            .then_some(data)
    }

    pub fn tid(&self) -> u8 {
        (self.qos_control.get() & 0x000f) as u8
    }

    pub fn sequence(&self) -> u16 {
        self.header.sequence_control.get() >> 4
    }
}

/// A control frame's receiver and transmitter.
#[derive(FromBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct ControlHeader {
    pub frame_control: U16,
    pub duration: U16,
    pub receiver: [u8; 6],
    pub transmitter: [u8; 6],
}

/// A compressed BlockAckReq.
#[derive(FromBytes, Immutable, KnownLayout)]
#[repr(C)]
pub struct BlockAckRequest {
    pub header: ControlHeader,
    pub control: U16,
    pub starting_sequence: U16,
}

impl BlockAckRequest {
    pub fn parse(frame: &[u8]) -> Option<&Self> {
        let (request, _) = Self::ref_from_prefix(frame).ok()?;
        (request.header.frame_control.get() & TYPE_SUBTYPE == BLOCK_ACK_REQUEST
            && request.control.get() & 0x0006 == 0x0004)
            .then_some(request)
    }

    pub fn tid(&self) -> u8 {
        (self.control.get() >> 12) as u8
    }

    pub fn ssn(&self) -> u16 {
        self.starting_sequence.get() >> 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qos_data_yields_tid_and_sequence_and_skips_fragments() {
        let mut frame = [0_u8; 26];
        frame[0] = 0x88;
        frame[22..24].copy_from_slice(&(0x123_u16 << 4).to_le_bytes());
        frame[24] = 5;
        let data = QosData::parse(&frame).unwrap();
        assert_eq!((data.tid(), data.sequence()), (5, 0x123));
        frame[1] = 0x04; // more fragments
        assert!(QosData::parse(&frame).is_none());
        assert!(QosData::parse(&frame[..25]).is_none());
    }

    #[test]
    fn block_ack_request_fields() {
        let mut frame = [0_u8; 20];
        frame[0] = 0x84;
        frame[16..18].copy_from_slice(&((3_u16 << 12) | 4).to_le_bytes());
        frame[18..20].copy_from_slice(&(0x456_u16 << 4).to_le_bytes());
        let request = BlockAckRequest::parse(&frame).unwrap();
        assert_eq!((request.tid(), request.ssn()), (3, 0x456));
        // Basic BAR (bit 2 clear) and Multi-TID BAR (bit 1 set) must be rejected.
        frame[16..18].copy_from_slice(&(3_u16 << 12).to_le_bytes());
        assert!(BlockAckRequest::parse(&frame).is_none());
        frame[16..18].copy_from_slice(&((3_u16 << 12) | 6).to_le_bytes());
        assert!(BlockAckRequest::parse(&frame).is_none());
        frame[16..18].copy_from_slice(&((3_u16 << 12) | 4).to_le_bytes());
        frame[0] = 0x94;
        assert!(BlockAckRequest::parse(&frame).is_none());
    }
}
