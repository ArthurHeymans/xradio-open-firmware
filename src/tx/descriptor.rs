use crate::global::Global;
use core::cell::Cell;

use super::*;

/// State of the backoff random generator.

pub(crate) static RETRY_RANDOM_STATE: Global<Cell<u32>> = Global::new(Cell::new(0x1234_5678));

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhyRateWords {
    pub control: u32,
    pub rate: u32,
}

/// Pure arithmetic from vendor `pas_build_phy_rate_words` (`0x834e`).
pub fn build_phy_rate_words(
    rate_index: u8,
    legacy_mode: u8,
    tx_flags: u32,
    request_flag_rate_bits: u8,
    rate_attribute: u8,
) -> PhyRateWords {
    let mut control = 2;
    let class = if rate_index < 4 {
        if legacy_mode == 0 || rate_index == 0 || (legacy_mode == 2 && rate_index == 1) {
            0x0400
        } else {
            0
        }
    } else if rate_index < 0x0e {
        0x0800
    } else {
        if tx_flags & 0x20 != 0 {
            control = 6;
        }
        if tx_flags & 0x08 != 0 { 0x1400 } else { 0x1000 }
    };
    PhyRateWords {
        control,
        rate: class
            | u32::from(rate_attribute & 0x0f)
            | (u32::from(request_flag_rate_bits & 7) << 16),
    }
}

/// Applies the HT mixed-mode duration field added by vendor
/// `txp_submit_to_pipe` (`0xadd0`) after building the base PHY words.
pub(crate) fn finalize_phy_control(phy: PhyRateWords, rate_index: u8, frame_length: u16) -> u32 {
    finalize_aggregate_phy_control(phy, rate_index, frame_length.wrapping_add(4))
}

/// Applies the HT mixed-mode L-SIG duration field for a PPDU whose length
/// already includes FCS (such as an A-MPDU's `aggregate_length`).
pub(crate) fn finalize_aggregate_phy_control(
    phy: PhyRateWords,
    rate_index: u8,
    wire_length: u16,
) -> u32 {
    if (phy.rate & 0x1fff) >> 10 == 5 {
        phy.control | (u32::from(crate::mac::ofdm_duration(rate_index, wire_length)) << 12)
    } else {
        phy.control
    }
}

pub(crate) fn single_frame_secondary_command(
    tx_flags: u32,
    header_duration: u16,
    duration_slot: u8,
) -> u32 {
    if tx_flags & 1 == 0 {
        0x2100_0000
            | packet_ram::encode_mac_packet_offset_u32(packet_ram::duration_word(usize::from(
                duration_slot,
            )) as u32)
    } else {
        0x3200_0000 | header_duration as u32
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SingleFramePasTiming {
    pub payload_extended: u16,
    pub payload_base: u16,
    pub ack: u16,
    pub total_airtime: u32,
    pub frame_kind: u8,
}

/// Payload/response part of vendor `pas_compute_tx_timing` (`0x7fa6`).
/// A protection prefix has separate airtime and command-slot bookkeeping.
pub fn compute_single_frame_pas_timing(
    phy_config: u16,
    rate: u8,
    frame_length: u16,
    flags: u32,
    ack_duration: u16,
    special_peer: bool,
) -> Option<SingleFramePasTiming> {
    let stream = flags & 8 != 0;
    let payload_length = frame_length.wrapping_add(4);
    let payload_base = crate::mac::base_airtime(phy_config, rate, payload_length, stream);
    let payload_extended = crate::mac::extended_airtime(phy_config, rate, payload_length, stream);
    let (ack, frame_kind) = if flags & 0x0200 != 0 {
        (0, 0xff)
    } else if flags & 0x4000 != 0 {
        (ack_duration, 0x0c)
    } else {
        (ack_duration, if special_peer { 0x0e } else { 0x11 })
    };
    Some(SingleFramePasTiming {
        payload_extended,
        payload_base,
        ack,
        total_airtime: u32::from(payload_base) + u32::from(ack),
        frame_kind,
    })
}

pub const fn single_frame_slot_duration(timing: SingleFramePasTiming) -> u32 {
    (timing.payload_base as u32).wrapping_mul(0x8000)
        + if timing.frame_kind == 0xff {
            0
        } else {
            0x2000 + timing.ack as u32
        }
}

pub(crate) const DOT11_FIXED_HEADER_LENGTH: u16 = 24;
/// Where a beacon's or probe response's body starts: after the header and
/// the timestamp the MAC fills in.
pub(crate) const PROBE_RESPONSE_BODY: u16 = 32;

#[repr(C, packed)]
pub(crate) struct Dot11FixedHeaderLayout {
    pub(crate) frame_control: u16,
    pub(crate) duration: u16,
    pub(crate) address_1: [u8; 6],
    pub(crate) address_2: [u8; 6],
    pub(crate) address_3: [u8; 6],
    pub(crate) sequence_control: u16,
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TxFrameAddress(pub(crate) u32);

impl TxFrameAddress {
    pub(crate) const fn new(address: u32) -> Self {
        Self(address)
    }
    pub(crate) const fn raw(self) -> u32 {
        self.0
    }
    pub(crate) const fn field(self, offset: usize) -> u32 {
        self.0.wrapping_add(offset as u32)
    }
    pub(crate) const fn frame_control(self) -> u32 {
        self.field(core::mem::offset_of!(Dot11FixedHeaderLayout, frame_control))
    }
    pub(crate) const fn address_1(self) -> u32 {
        self.field(core::mem::offset_of!(Dot11FixedHeaderLayout, address_1))
    }
    pub(crate) const fn address_2_byte_unchecked(self, byte: u32) -> u32 {
        self.field(core::mem::offset_of!(Dot11FixedHeaderLayout, address_2))
            .wrapping_add(byte)
    }
    pub(crate) const fn address_2_halfword_unchecked(self, word: u32) -> u32 {
        self.address_2_byte_unchecked(word * 2)
    }
    pub(crate) const fn sequence_control(self) -> u32 {
        self.field(core::mem::offset_of!(
            Dot11FixedHeaderLayout,
            sequence_control
        ))
    }
    pub(crate) const fn descriptor_tail(self) -> u32 {
        self.address_1()
    }
}

/// A compressed BlockAckReq is a 20-octet control frame, so the 24-octet data
/// header is not a valid floor for every frame that reaches the descriptor path.
pub(crate) const CONTROL_FRAME_MIN_LENGTH: u16 = 20;

pub(crate) fn validated_tx_frame(address: u32, length: u16) -> Option<TxFrameAddress> {
    (length >= CONTROL_FRAME_MIN_LENGTH)
        .then(|| packet_ram::RuntimePacketAddress::new(address, usize::from(length)))
        .flatten()
        .map(|address| TxFrameAddress::new(address.raw()))
}

pub(crate) fn validated_context_tx_frame(context: &TxContext) -> Option<TxFrameAddress> {
    validated_tx_frame(context.frame.get(), context.frame_length.get())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Dot11HeaderShape {
    pub(crate) length: u32,
    pub(crate) qos_data: bool,
}

pub(crate) fn classify_dot11_header(frame_control: u16, legacy_eapol: bool) -> Dot11HeaderShape {
    let four_address = frame_control & 0x0300 == 0x0300;
    let qos_data = !legacy_eapol && frame_control & 0x008f == 0x0088;
    let base = if four_address {
        30
    } else {
        u32::from(DOT11_FIXED_HEADER_LENGTH)
    };
    let qos_extension = if qos_data {
        if frame_control & 0x8000 != 0 { 6 } else { 2 }
    } else {
        0
    };
    Dot11HeaderShape {
        length: base + qos_extension,
        qos_data,
    }
}

#[repr(C)]
pub(crate) struct TxDescriptorLayout {
    pub(crate) command: u32,
    pub(crate) flags: u32,
    pub(crate) duration: u32,
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TxDescriptorAddress(pub(crate) u32);

impl TxDescriptorAddress {
    pub(crate) const fn new(address: u32) -> Self {
        Self(address)
    }
    pub(crate) const fn raw(self) -> u32 {
        self.0
    }
    pub(crate) const fn command(self) -> u32 {
        self.0
            .wrapping_add(core::mem::offset_of!(TxDescriptorLayout, command) as u32)
    }
    pub(crate) const fn flags(self) -> u32 {
        self.0
            .wrapping_add(core::mem::offset_of!(TxDescriptorLayout, flags) as u32)
    }
    pub(crate) const fn duration(self) -> u32 {
        self.0
            .wrapping_add(core::mem::offset_of!(TxDescriptorLayout, duration) as u32)
    }
    pub(crate) const fn word_unchecked(self, index: u32) -> u32 {
        self.0.wrapping_add(index * 4)
    }
}

pub(crate) fn next_retry_random24() -> u32 {
    let state = RETRY_RANDOM_STATE.get();
    let mixed = (state >> 4) ^ state;
    let next = (state << 27) | (mixed & 0x07ff_ffff);
    RETRY_RANDOM_STATE.set(next);
    next & 0x00ff_ffff
}

/// The current contention window of a frame's interface and access
/// category, as a mask for the random backoff.
pub(crate) fn contention_window(context: &TxContext) -> u16 {
    let pas = crate::mac::low_mac().pas(context.interface.get());
    pas.contention_window
        .get(usize::from(context.access_category.get()))
        .map_or(0, |window| window.get() as u16)
}

pub(crate) fn build_single_frame_duration<M: Hw>(
    mmio: &mut M,
    descriptor: u32,
    context: &TxContext,
    expects_ack: bool,
) {
    build_frame_duration(
        mmio,
        descriptor,
        context,
        expects_ack,
        context.tx_rate.get(),
    );
}

pub(crate) fn build_frame_duration<M: Hw>(
    mmio: &mut M,
    descriptor: u32,
    context: &TxContext,
    expects_ack: bool,
    rate: u8,
) {
    let descriptor = TxDescriptorAddress::new(descriptor);
    mmio.write(descriptor.command(), 0);

    let random = (next_retry_random24() as u16) & contention_window(context);
    mmio.write(descriptor.flags(), (u32::from(random) & 0x0fff) << 10);

    let duration_word = if expects_ack {
        let low_mac = crate::mac::low_mac();
        let timing_index = low_mac
            .retry_rate_map
            .get(usize::from(rate))
            .map_or(0, Cell::get);
        let timing = u32::from(
            crate::mac::TX_DURATION_TIMING
                .get(usize::from(timing_index))
                .copied()
                .unwrap_or(0),
        );
        let duration = low_mac
            .slot_time_initial
            .get()
            .wrapping_add(low_mac.slot_time_base.get().wrapping_mul(2))
            .wrapping_add(timing)
            & 0xffff;
        0xd800_0000 | (((duration & 0x03ff) * 8).wrapping_add(0x2000))
    } else {
        0xdc00_0000
    };
    mmio.write(descriptor.duration(), duration_word);
}

/// Compute a frame's airtime and response class (`pas_compute_tx_timing`).
///
/// # Safety
/// The frame must be in packet RAM and the PAS/low-MAC tables initialized.
pub(crate) fn prepare_single_frame_pas_timing(context: &TxContext) -> Result<(), ProbeBuildError> {
    let interface = usize::from(context.interface.get());
    if interface > 2 {
        return Err(ProbeBuildError::InvalidInterface);
    }
    let flags = context.flags.get();
    let rate = context.tx_rate.get();
    let low_mac = crate::mac::low_mac();
    let pas = low_mac.pas(interface);
    let timing_index = pas.rate_map.get(usize::from(rate)).map_or(0, Cell::get);
    // The rate map index selects ACK timing only; the PHY rate words take the
    // WSM rate attribute (`request_flag_rate_bits`) unchanged. Writing the
    // index there changed healthy `0x5104....` words into `0x5107....`.
    let airtimes = if flags & 0x4000 != 0 {
        &low_mac.long_airtimes
    } else {
        &low_mac.short_airtimes
    };
    let ack_duration = airtimes.get(usize::from(timing_index)).map_or(0, Cell::get);
    validated_context_tx_frame(context).ok_or(ProbeBuildError::PacketRamMismatch)?;
    // The vendor's special-peer response class needs PAS mode 5 or 6, which
    // this firmware never sets.
    let timing = compute_single_frame_pas_timing(
        low_mac.rate_config.get(),
        rate,
        context.frame_length.get(),
        flags,
        ack_duration,
        false,
    )
    .ok_or(ProbeBuildError::UnsupportedPublicationShape)?;
    context.ack_duration.set(timing.ack);
    context.payload_extended.set(timing.payload_extended);
    context.payload_base.set(timing.payload_base);
    context.total_airtime.set(timing.total_airtime);
    context.frame_kind.set(timing.frame_kind);
    prepare_protection_timing(context, timing);
    Ok(())
}

/// Write a frame's TX descriptor (`txp_submit_to_pipe`) at `destination`.
///
/// # Safety
/// `destination` must be descriptor storage in packet RAM that the MAC is not
/// reading.
pub(crate) fn emit_descriptor(
    context: &TxContext,
    destination: u32,
) -> Result<(), ProbeBuildError> {
    let length = context.frame_length.get();
    let frame = validated_tx_frame(context.frame.get(), length)
        .ok_or(ProbeBuildError::PacketRamMismatch)?;
    let descriptor_address =
        packet_ram::RuntimePacketAddress::new(destination, core::mem::size_of::<[u32; 13]>())
            .ok_or(ProbeBuildError::PacketRamMismatch)?;
    let descriptor = TxDescriptorAddress::new(descriptor_address.raw());
    let rate = context.tx_rate.get();
    let tx_flags = context.flags.get();
    let if_id = context.interface.get();
    if usize::from(if_id) >= packet_ram::INTERFACE_METADATA_SIZE {
        return Err(ProbeBuildError::PacketRamMismatch);
    }
    let duration_slot = context.duration_slot.get();
    if usize::from(duration_slot) >= packet_ram::DURATION_WORD_COUNT {
        return Err(ProbeBuildError::PacketRamMismatch);
    }
    // The legacy-mode byte is only ever written with zero.
    let legacy_mode = 0;
    let rate_attribute = crate::mac::RATE_ENCODING
        .get(usize::from(rate))
        .copied()
        .unwrap_or(0);
    let hardware_rate = crate::mac::RATE_ATTRIBUTE
        .get(usize::from(rate))
        .copied()
        .unwrap_or(0);
    let phy = build_phy_rate_words(
        rate,
        legacy_mode,
        tx_flags,
        context.request_flag_rate_bits.get(),
        rate_attribute,
    );
    let metadata_address = packet_ram::RuntimePacketAddress::new(
        packet_ram::interface_metadata_byte(usize::from(if_id)) as u32,
        1,
    )
    .ok_or(ProbeBuildError::PacketRamMismatch)?;
    let frame_control =
        u32::from(context.frame_control.get()) | if tx_flags & 0x10 != 0 { 0x0800 } else { 0 };
    let mut word_index = 0_u32;
    let mut add = |word: u32| {
        write_u32(descriptor.word_unchecked(word_index) as usize, word);
        word_index += 1;
    };
    add(0x5100_0000 | (phy.rate & 0x00ff_ffff));
    add(0x5000_0000 | (finalize_phy_control(phy, rate, length) & 0x00ff_ffff));
    add(0x5200_0000 | (u32::from(hardware_rate) << 16) | u32::from(length + 4));
    add(0x3100_0000 + frame_control);
    add(0x4700_0000 + (frame_control >> 8));
    add(0x2080_0000 | metadata_address.mac_offset().raw());
    if context.frame_control.get() & 0x000f == 0x0004 {
        // A control frame (vendor `txp_submit_to_pipe`, control branch):
        // everything after the frame control as it is, with none of the
        // data header's duration, address and sequence commands.
        let body = packet_ram::RuntimePacketAddress::new(frame.field(2), usize::from(length - 2))
            .ok_or(ProbeBuildError::PacketRamMismatch)?;
        add(0x4000_0000 | body.tx_payload_bus_address(0x007f_fffc).raw());
        add((u32::from(length - 2) & 0x0fff) << 12 | (body.raw() & 3));
        add(0x0700_4600);
        add(0xf000_0000);
        return Ok(());
    }
    add(0x3200_0000 | u32::from(context.ack_duration.get()));
    add(0x2900_0000 | packet_ram::encode_mac_packet_offset_u32(frame.descriptor_tail()));
    add(single_frame_secondary_command(
        tx_flags,
        read_u16(frame.sequence_control() as usize),
        duration_slot,
    ));
    // The hardware descriptor always splits after the fixed three-address
    // 24-byte prefix. Optional address-4, QoS, and HT-control bytes remain
    // in the payload segment, whatever the parsed header length. A probe
    // response carries the TSF like a beacon (vendor `txp_submit_to_pipe`):
    // `0x0700_8400` has the MAC fill in its 8-octet timestamp.
    let header = if context.frame_control.get() & 0x00fc == 0x0050 && length >= PROBE_RESPONSE_BODY
    {
        add(0x0700_8400);
        PROBE_RESPONSE_BODY
    } else {
        DOT11_FIXED_HEADER_LENGTH
    };
    if length > header {
        let payload = packet_ram::RuntimePacketAddress::new(
            frame.field(usize::from(header)),
            usize::from(length - header),
        )
        .ok_or(ProbeBuildError::PacketRamMismatch)?;
        add(0x4000_0000 | payload.tx_payload_bus_address(0x007f_fffc).raw());
        add((u32::from(length - header) & 0x0fff) << 12 | (payload.raw() & 3));
    }
    add(0x0700_4600);
    add(0xf000_0000);
    Ok(())
}

/// Stage a host frame's descriptor in its context's packet-RAM area.
///
/// # Safety
/// As [`emit_descriptor`].
pub fn build_host_frame_descriptor(id: ContextId) -> Result<(), ProbeBuildError> {
    let destination = host_descriptor_address(id).ok_or(ProbeBuildError::InvalidContextPointer)?;
    emit_descriptor(id.get(), destination)
}
