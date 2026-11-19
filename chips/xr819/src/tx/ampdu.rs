//! A-MPDU command streams and compressed BlockAck, the hardware-free half.
//!
//! An aggregate is one pipe slot of kind 1. Its slot command carries the
//! three PHY words shared by all members and jumps to a software record that
//! transfers each member (vendor `txp_desc_emit` mode 1): every member but the
//! last is followed by a delimiter word and, when the peer asks for a minimum
//! MPDU start spacing, a jump into the spacing padding at the end of the
//! automatic response list. The recipient answers with one compressed
//! BlockAck whose bitmap says which members arrived.

/// Members per aggregate: the BlockAck window.
pub const MAX_MEMBERS: usize = super::ba::WINDOW as usize;
/// A transfer per member, a delimiter and a spacing jump between members, and
/// the terminator.
const MAX_WORDS: usize = MAX_MEMBERS * 3 - 1;

/// Padding words per MPDU for the peer's minimum start spacing (HT
/// capabilities, 0..=7) at MCS 0..=7 (rate indices 14..=21); vendor table.
const SPACING_SELECTORS: [[u8; 8]; 8] = [
    [0, 0, 0, 0, 0, 0, 0, 0],
    [1, 1, 1, 1, 1, 1, 1, 1],
    [1, 1, 1, 1, 1, 1, 1, 2],
    [1, 1, 1, 1, 2, 2, 2, 3],
    [1, 1, 2, 2, 3, 4, 4, 5],
    [1, 2, 3, 4, 5, 7, 8, 9],
    [2, 4, 5, 7, 10, 13, 15, 17],
    [4, 7, 10, 13, 20, 26, 30, 33],
];

pub const fn spacing_selector(density: u8, rate: u8) -> u8 {
    if density < 8 && rate >= 14 && rate < 22 {
        SPACING_SELECTORS[density as usize][(rate - 14) as usize]
    } else {
        0
    }
}

/// MAC transfer opcode for an aligned packet-RAM address.
pub const fn transfer_word(address: u32) -> u32 {
    0x6500_0000 | (address & 0x001f_fffc)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Member {
    /// Packet-RAM address of the member's frame state; the MPDU descriptor
    /// starts 8 bytes in.
    pub frame_state: u32,
    pub frame_length: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandStream {
    words: [u32; MAX_WORDS],
    length: usize,
    pub aggregate_length: u16,
}

impl CommandStream {
    pub fn words(&self) -> &[u32] {
        &self.words[..self.length]
    }
}

/// Byte count used both for admission and the actual MAC descriptor. Only
/// nonfinal MPDUs carry the alignment padding and inter-MPDU spacing.
#[derive(Default)]
pub struct WireLength { nonfinal: u32 }

impl WireLength {
    pub fn append(&mut self, length: u16, selector: u8) -> u32 {
        let mpdu = u32::from(length) + 8; // delimiter and FCS
        let total = self.nonfinal + mpdu;
        self.nonfinal += ((mpdu + 3) & !3) + u32::from(selector) * 4;
        total
    }
}

/// The software-record command stream for 2..=MAX_MEMBERS members.
/// `spacing` is the transfer into the spacing padding for `selector` words.
pub fn command_stream(members: &[Member], selector: u8, spacing: Option<u32>) -> Option<CommandStream> {
    if !(2..=MAX_MEMBERS).contains(&members.len()) || (selector != 0) != spacing.is_some() {
        return None;
    }
    let (last, rest) = members.split_last()?;
    let between = [Some(0x6600_0000), spacing];
    let words = rest
        .iter()
        .flat_map(|member| core::iter::once(Some(transfer_word(member.frame_state + 8))).chain(between))
        .chain([Some(transfer_word(last.frame_state + 8)), Some(0xe400_0000)])
        .flatten();
    let mut stream = CommandStream { words: [0; MAX_WORDS], length: 0, aggregate_length: 0 };
    for (slot, word) in stream.words.iter_mut().zip(words) {
        *slot = word;
        stream.length += 1;
    }
    // Non-final members: MPDU, 4-byte delimiter and FCS, padded to a word,
    // plus the spacing; the last one without padding.
    let mut wire = WireLength::default();
    let mut length = 0;
    for member in members { length = wire.append(member.frame_length, selector); }
    stream.aggregate_length = u16::try_from(length).ok()?;
    Some(stream)
}

/// The PHY words of the slot command, shared by all members.
pub const fn phy_words(rate_word: u32, control_word: u32, hardware_rate: u8, aggregate_length: u16) -> [u32; 3] {
    [
        0x5100_0000 | (rate_word & 0x00ff_ffff),
        0x5000_0000 | (control_word & 0x00ff_ffff),
        0x5200_0000 | ((hardware_rate as u32) << 16) | aggregate_length as u32,
    ]
}

/// A compressed BlockAck.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockAck {
    pub receiver: [u8; 6],
    pub transmitter: [u8; 6],
    pub tid: u8,
    pub ssn: u16,
    pub bitmap: u64,
}

impl BlockAck {
    /// A BlockAck belongs to one peer/TID on one local interface.
    pub fn matches(&self, own: [u8; 6], peer: [u8; 6], tid: u8) -> bool {
        self.receiver == own && self.transmitter == peer && self.tid == tid
    }

    /// Whether the bitmap acknowledges `sequence`. A sequence before the
    /// start sequence is not: an Intel recipient starts at its window start,
    /// but others (the HLH router) start at the first MPDU of the PPDU they
    /// received, so a lost first member would look delivered.
    pub fn acknowledges(&self, sequence: u16) -> bool {
        let offset = sequence.wrapping_sub(self.ssn) & 0x0fff;
        offset < 64 && self.bitmap >> offset & 1 != 0
    }
}

/// Decode a compressed BlockAck without filtering its addresses. Shared
/// FIFO lookahead must route every peer/interface before advancing its cursor.
pub fn parse_block_ack(frame: &[u8]) -> Option<BlockAck> {
    let u16_at = |offset: usize| Some(u16::from_le_bytes([*frame.get(offset)?, *frame.get(offset + 1)?]));
    let control = u16_at(16)?;
    // Control frame, BlockAck subtype; compressed bitmap, single TID.
    if u16_at(0)? & 0x00fc != 0x0094
        || control & 0x0006 != 0x0004
    {
        return None;
    }
    Some(BlockAck {
        receiver: frame.get(4..10)?.try_into().ok()?,
        transmitter: frame.get(10..16)?.try_into().ok()?,
        tid: (control >> 12) as u8,
        ssn: u16_at(18)? >> 4,
        bitmap: u64::from_le_bytes(frame.get(20..28)?.try_into().ok()?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members(count: usize) -> std::vec::Vec<Member> {
        (0..count)
            .map(|index| Member { frame_state: 0x0901_0000 + index as u32 * 0x400, frame_length: 1500 })
            .collect()
    }

    #[test]
    fn command_streams_match_the_vendor_member_loop() {
        assert_eq!(
            [spacing_selector(0, 19), spacing_selector(4, 19), spacing_selector(7, 21)],
            [0, 4, 33]
        );
        assert_eq!([spacing_selector(8, 19), spacing_selector(4, 13)], [0, 0]);

        // Vectors from the qualified depth-two and depth-four builders.
        let two = command_stream(&members(2), 0, None).unwrap();
        assert_eq!(two.words(), &[0x6501_0008, 0x6600_0000, 0x6501_0408, 0xe400_0000]);
        assert_eq!(two.aggregate_length, 0x0bc8);
        assert_eq!(phy_words(0x031407, 0x06c006, 0x0c, 0x0bc8), [0x5103_1407, 0x5006_c006, 0x520c_0bc8]);
        let four = command_stream(&members(4), 0, None).unwrap();
        assert_eq!(four.words().len(), 8);
        assert_eq!(four.aggregate_length, 0x1790);
        let spacing = transfer_word(0x0901_2000);
        let spaced = command_stream(&members(4), 2, Some(spacing)).unwrap();
        assert_eq!(spaced.aggregate_length, 0x17a8);
        assert_eq!([spaced.words()[2], spaced.words()[5], spaced.words()[8]], [spacing; 3]);
        assert_eq!(spaced.words().len(), 11);

        let full = command_stream(&members(MAX_MEMBERS), 33, Some(spacing)).unwrap();
        assert_eq!(full.words().len(), MAX_WORDS);
        assert_eq!(command_stream(&members(1), 0, None), None);
        assert_eq!(command_stream(&members(MAX_MEMBERS + 1), 0, None), None);
        assert_eq!(command_stream(&members(2), 2, None), None);
    }

    #[test]
    fn block_acks_with_the_same_tid_and_bitmap_stay_with_their_peer() {
        let own = [0x02; 6];
        let peers = [[0x04; 6], [0x06; 6]];
        let expected = peers.map(|peer| BlockAck { receiver: own, transmitter: peer, tid: 0, ssn: 10, bitmap: 3 });
        let received = peers.map(|peer| {
            let mut frame = [0_u8; 28];
            frame[0] = 0x94;
            frame[4..10].copy_from_slice(&own);
            frame[10..16].copy_from_slice(&peer);
            frame[16] = 4;
            frame[18..20].copy_from_slice(&(10_u16 << 4).to_le_bytes());
            frame[20] = 3;
            parse_block_ack(&frame).unwrap()
        });
        assert_eq!(received, expected);
        // Reverse arrival order must not turn either bitmap into the other's ACK.
        let matches = received.into_iter().rev().map(|ack| peers.iter().position(|&peer| ack.matches(own, peer, 0)));
        assert_eq!(matches.collect::<std::vec::Vec<_>>(), [Some(1), Some(0)]);
    }

    #[test]
    fn compressed_block_ack() {
        const OWN: [u8; 6] = [0x12, 0x42, 0x2a, 0x37, 0x70, 0x07];
        const PEER: [u8; 6] = [0x98, 0x5f, 0x41, 0x18, 0x76, 0x17];
        let mut frame = std::vec![0x94, 0, 0, 0];
        frame.extend_from_slice(&OWN);
        frame.extend_from_slice(&PEER);
        // TID 1, compressed; SSN 0xffe; members 0xffe, 0x000 and 0x002 arrived.
        frame.extend_from_slice(&[0x04, 0x10, 0xe0, 0xff]);
        frame.extend_from_slice(&0b10101_u64.to_le_bytes());
        let ba = parse_block_ack(&frame).unwrap();
        assert_eq!(ba, BlockAck { receiver: OWN, transmitter: PEER, tid: 1, ssn: 0xffe, bitmap: 0b10101 });
        assert!(ba.matches(OWN, PEER, 1));
        assert!(!ba.matches(OWN, [0x02; 6], 1));
        assert!(!ba.matches([0x02; 6], PEER, 1));
        assert!(!ba.matches(OWN, PEER, 2));
        assert_eq!([0xffe, 0xfff, 0x000, 0x001, 0x002].map(|sequence| ba.acknowledges(sequence)), [true, false, true, false, true]);
        assert!(!ba.acknowledges(0xff0));
        assert_eq!(parse_block_ack(&frame[..27]), None);
        frame[16] = 0x00; // basic (uncompressed) BlockAck
        assert_eq!(parse_block_ack(&frame), None);
    }
}
