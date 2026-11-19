//! XR819 RTS/CTS prefixes (vendor 0x7fa6, 0x1132, 0x11c4 and 0xa712).
//! A protected publication has two command slots but exactly one host owner.

use super::*;

pub const MIB_RTS_THRESHOLD: u16 = 0x0007;
pub const MIB_NON_ERP_PROTECTION: u16 = 0x1000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtectionPolicy {
    pub rts_threshold: u32,
    pub non_erp: bool,
}

impl ProtectionPolicy {
    pub const DISABLED: Self = Self {
        rts_threshold: u32::MAX,
        non_erp: false,
    };

    pub fn select(
        self,
        frame_control: u16,
        length: u16,
        rate: u8,
        flags: u32,
    ) -> Option<Protection> {
        if frame_control & 0x0c == 4 {
            return None;
        }
        if frame_control & 0x0c == 8
            && flags & 0x300 == 0
            && u32::from(length) + 4 > self.rts_threshold
        {
            Some(Protection::Rts)
        } else if self.non_erp && rate > 3 {
            Some(Protection::CtsToSelf)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Protection {
    Rts,
    CtsToSelf,
}

impl Protection {
    pub(crate) const fn flag(self) -> u32 {
        match self {
            Self::Rts => 0x400,
            Self::CtsToSelf => 0x800,
        }
    }

    pub(crate) const fn from_flags(flags: u32) -> Option<Self> {
        if flags & 0x400 != 0 {
            Some(Self::Rts)
        } else if flags & 0x800 != 0 {
            Some(Self::CtsToSelf)
        } else {
            None
        }
    }

    pub(crate) const fn expected_status(self) -> u8 {
        match self {
            Self::Rts => 6,
            Self::CtsToSelf => 0xff,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProtectionTiming {
    pub kind: Protection,
    pub rate: u8,
    pub base: u16,
    pub extended: u16,
    pub cts: u16,
    pub nav: u16,
}

impl ProtectionTiming {
    pub(crate) fn compute(
        kind: Protection,
        rate: u8,
        config: u16,
        data: SingleFramePasTiming,
    ) -> Self {
        let length = if kind == Protection::Rts { 20 } else { 14 };
        // The prefix PHY builder uses legacy_mode=0 (long CCK preamble).
        let config = if rate < 4 { config | 0x100 } else { config };
        let cts = if kind == Protection::Rts {
            crate::mac::extended_airtime(config, rate, 14, true)
        } else {
            0
        };
        Self {
            kind,
            rate,
            base: crate::mac::base_airtime(config, rate, length, true),
            extended: crate::mac::extended_airtime(config, rate, length, true),
            cts,
            nav: data
                .payload_extended
                .wrapping_add(data.ack)
                .wrapping_add(cts),
        }
    }

    pub(crate) const fn slot_duration(self) -> u32 {
        (self.extended as u32) * 0x8000
            + if matches!(self.kind, Protection::Rts) {
                0xa000 + self.cts as u32
            } else {
                0
            }
    }
}

/// RTS uses rate_map[data]; non-ERP protection uses the vendor's 1 Mbps.
fn protection_rate(fallback: u8, non_erp: bool) -> u8 {
    if non_erp { 0 } else { fallback }
}

#[cfg(target_arch = "arm")]
pub(crate) fn apply_protection_policy(
    id: ContextId,
    policy: ProtectionPolicy,
) -> Result<(), ProbeBuildError> {
    let context = id.get();
    let protection = policy.select(
        context.frame_control.get(),
        context.frame_length.get(),
        context.tx_rate.get(),
        context.flags.get(),
    );
    let flags = (context.flags.get() & !0x0c00) | protection.map_or(0, Protection::flag);
    // PAS-queued frames already have timing. Avoid repeating software airtime
    // divisions in the default unprotected scheduler path.
    if flags == context.flags.get() && context.erp_protection.get() == policy.non_erp {
        return Ok(());
    }
    context.flags.set(flags);
    // Retain the ERP constraint for retries whose data rate changes.
    context.erp_protection.set(policy.non_erp);
    prepare_single_frame_pas_timing(context)
}

pub(crate) fn prepare_protection_timing(context: &TxContext, data: SingleFramePasTiming) {
    let low_mac = crate::mac::low_mac();
    let pas = low_mac.pas(context.interface.get());
    let timing = Protection::from_flags(context.flags.get()).map(|kind| {
        let fallback = pas
            .rate_map
            .get(usize::from(context.tx_rate.get()))
            .map_or(0, core::cell::Cell::get);
        let erp_rate = context.erp_protection.get()
            && (kind == Protection::CtsToSelf || context.tx_rate.get() > 3);
        let rate = protection_rate(fallback, erp_rate);
        ProtectionTiming::compute(kind, rate, low_mac.rate_config.get(), data)
    });
    context.protection.set(timing);
    if let Some(prefix) = timing {
        context.total_airtime.set(
            u32::from(prefix.base)
                + u32::from(prefix.cts)
                + u32::from(data.payload_extended)
                + u32::from(data.ack),
        );
    }
}

/// Vendor's kind-1 aggregate branch omits CTS-to-self (but not RTS).
/// Queued/retried singles recover their policy at the reservation boundary.
pub(crate) fn omit_aggregate_cts(context: &TxContext) -> bool {
    if context.flags.get() & 0x400 != 0 {
        return false;
    }
    context
        .flags
        .set(context.flags.get() & !(0x800 | (1 << 15)));
    context.protection.set(None);
    context
        .total_airtime
        .set(u32::from(context.payload_base.get()) + u32::from(context.ack_duration.get()));
    true
}

/// Exact inline command shapes of the two vendor prefix builders. Lengths
/// include the generated FCS; neither control frame has a sequence field.
fn commands(
    timing: ProtectionTiming,
    frame: u32,
    interface_metadata: u32,
    own_mac: [u8; 6],
    request_bits: u8,
) -> ([u32; 11], usize) {
    let rate = usize::from(timing.rate);
    let phy = build_phy_rate_words(
        timing.rate,
        0,
        0,
        request_bits,
        crate::mac::RATE_ENCODING[rate],
    );
    let mut words = [0xf000_0000; 11];
    words[..7].copy_from_slice(&[
        0x5100_0000 | phy.rate,
        0x5000_0000 | phy.control,
        0x5200_0000
            | u32::from(crate::mac::RATE_ATTRIBUTE[rate]) << 16
            | if timing.kind == Protection::Rts {
                20
            } else {
                14
            },
        0x3100_0000
            | if timing.kind == Protection::Rts {
                0xb4
            } else {
                0xc4
            },
        0x4700_0000,
        0x2080_0000 | packet_ram::encode_mac_packet_offset_u32(interface_metadata),
        0x3200_0000 | u32::from(timing.nav),
    ]);
    let tail = if timing.kind == Protection::Rts {
        words[7] = 0x2600_0000 | packet_ram::encode_mac_packet_offset_u32(frame + 4);
        8
    } else {
        words[7] = 0x3300_0000 | u32::from_le_bytes([own_mac[0], own_mac[1], own_mac[2], 0]);
        words[8] = 0x3300_0000 | u32::from_le_bytes([own_mac[3], own_mac[4], own_mac[5], 0]);
        9
    };
    words[tail] = 0x0700_4600;
    (words, tail + 2)
}

#[cfg(target_arch = "arm")]
pub(crate) fn emit_protection_descriptor(
    context: &TxContext,
    command: u32,
) -> Result<(), ProbeBuildError> {
    let timing = context
        .protection
        .get()
        .ok_or(ProbeBuildError::UnsupportedPublicationShape)?;
    validated_context_tx_frame(context).ok_or(ProbeBuildError::PacketRamMismatch)?;
    packet_ram::RuntimePacketAddress::new(command, packet_ram::TX_COMMAND_SIZE)
        .ok_or(ProbeBuildError::PacketRamMismatch)?;
    let interface = usize::from(context.interface.get());
    let own_mac = core::array::from_fn(|i| crate::mac::low_mac().pas(interface).own_mac[i].get());
    let (words, len) = commands(
        timing,
        context.frame.get(),
        packet_ram::interface_metadata_byte(interface) as u32,
        own_mac,
        context.request_flag_rate_bits.get(),
    );
    for (index, &word) in words[..len].iter().enumerate() {
        let address = command as usize + 0x0c + index * 4;
        write_u32(address, word);
        if read_u32(address) != word {
            return Err(ProbeBuildError::DescriptorReadbackMismatch);
        }
    }
    Ok(())
}

/// Both slots are staged before one trigger/GO, including when the ring wraps.
pub(crate) fn execute_protected_publication<M: Hw>(
    mmio: &mut M,
    pipes: &Pipes,
    pipe: u8,
    data_slot: u8,
    context: ContextId,
) {
    let prefix_slot = data_slot.wrapping_sub(1) & 3;
    for (slot, batch) in [
        (prefix_slot, BatchPosition::First),
        (data_slot, BatchPosition::Last),
    ] {
        execute_single_probe_publication(
            mmio,
            pipes,
            SingleProbePublicationInput {
                pipe,
                slot,
                context,
                batch,
            },
        );
    }
    if !finalize_staged_pipe(mmio, pipes, pipe, prefix_slot, data_slot) {
        crate::halt_always!();
    }
}

/// Vendor 0x997c..0x99ae: leave the hardware consumer cursor alone, rewind
/// only bits 27..29, clear the two command bits and the middle pending byte.
pub(crate) fn protected_retry_cursor(cursor: u32, current: u8, prefix: u8) -> u32 {
    let masked = cursor & 0xc7ff_00ff & !(1_u32 << current) & !(1_u32 << prefix);
    let rewind = masked | u32::from(prefix) << 27;
    if (masked & 0x03ff_ffff) >> 24 == u32::from(prefix) {
        rewind | 0x8000_0000
    } else {
        rewind
    }
}

/// Reuse the existing ring entries (no sentinel, duration FIFO, arm or GO).
/// An RTS retry clears one pending bit; a data retry can rewind its prefix.
pub(crate) fn execute_protected_retry<M: Hw>(
    mmio: &mut M,
    pipes: &Pipes,
    pipe_index: u8,
    id: ContextId,
    pending_mask: u32,
    replay_prefix: bool,
) {
    let pipe = pipes.pipe(pipe_index);
    let Some(ring) = pipe.hardware_ring(pipe_index) else {
        crate::halt_always!()
    };
    let current = pipe.current.get();
    let data_slot = pipe.last.get();
    let prefix_slot = data_slot.wrapping_sub(1) & 3;
    let context = id.get();
    let Some(timing) = context.protection.get() else {
        crate::halt_always!()
    };
    let prefix = pipe.slot(prefix_slot);
    if pipe.producer.get() != prefix_slot
        || ![prefix_slot, data_slot].contains(&current)
        || prefix.prefix.get() != Some(timing.kind)
        || prefix.frame.get().is_some_and(|owner| owner != id)
        || pipe.slot(data_slot).frame.get() != Some(id)
    {
        crate::halt_always!();
    }
    let prefix_command =
        packet_ram::tx_command(usize::from(pipe_index), usize::from(prefix_slot)) as u32;
    build_frame_duration(
        mmio,
        prefix_command,
        context,
        timing.kind == Protection::Rts,
        timing.rate,
    );
    if timing.kind == Protection::Rts {
        let flags = mmio.read(prefix_command + 4) | 0x86;
        mmio.write(prefix_command + 4, flags);
    } else if !replay_prefix {
        // Vendor mode 2 makes the old CTS prefix a non-transmitting command.
        mmio.write(prefix_command + 8, 0xc400_0000);
    }
    if current == data_slot {
        let command =
            packet_ram::tx_command(usize::from(pipe_index), usize::from(data_slot)) as u32;
        build_single_frame_duration(mmio, command, context, context.frame_kind.get() != 0xff);
        if context.frame_kind.get() != 0xff {
            let flags = mmio.read(command + 4) | (u32::from(context.frame_kind.get()) + 0x80);
            mmio.write(command + 4, flags);
        }
    }
    mmio.write(PIPE_IRQ_TRIGGER, (1_u32 << pipe_index) << 25);
    let cursor = mmio.read(ring.cursor_and_pending_mask());
    let ack = if current == data_slot && replay_prefix {
        let prefix = pipe.slot(prefix_slot);
        if prefix.frame.get().is_none() {
            prefix.reserve(id, timing.kind.expected_status());
            prefix.prefix.set(Some(timing.kind));
            pipes.active_tx.set(pipes.active_tx.get().wrapping_add(1));
        }
        pipe.current.set(prefix_slot);
        mmio.write(
            ring.cursor_and_pending_mask(),
            protected_retry_cursor(cursor, current, prefix_slot),
        );
        0xf010
    } else {
        mmio.write(ring.cursor_and_pending_mask(), cursor & !(1_u32 << current));
        if current == pipe.producer.get() {
            1
        } else {
            0xf010
        }
    };
    mmio.write(
        PIPE_IRQ_PENDING,
        0_u32.wrapping_sub(pending_mask.wrapping_add(ack)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewind_preserves_consumer_and_sets_kick_only_at_prefix() {
        for prefix in 0..4 {
            let current = (prefix + 1) & 3;
            for consumer in 0..4 {
                let cursor = 0x4400_ff0f | consumer << 24 | 7 << 27;
                let word = protected_retry_cursor(cursor, current, prefix);
                assert_eq!(word & 0x0700_0000, cursor & 0x0700_0000);
                assert_eq!((word >> 27) & 7, u32::from(prefix));
                assert_eq!(word & 0xff00, 0);
                assert_eq!(word & 0x0f, 0x0f & !(1 << current) & !(1 << prefix));
                assert_eq!(word >> 31, u32::from(consumer == u32::from(prefix)));
            }
        }
    }

    #[test]
    fn policy_threshold_includes_fcs_and_rts_wins_over_erp() {
        let policy = ProtectionPolicy {
            rts_threshold: 300,
            non_erp: true,
        };
        assert_eq!(policy.select(8, 296, 0, 0), None);
        assert_eq!(policy.select(8, 297, 0, 0), Some(Protection::Rts));
        assert_eq!(policy.select(8, 1000, 21, 0), Some(Protection::Rts));
        assert_eq!(
            policy.select(8, 1000, 21, 0x300),
            Some(Protection::CtsToSelf)
        );
        assert_eq!(policy.select(0, 1000, 6, 0), Some(Protection::CtsToSelf));
        assert_eq!(policy.select(0x84, 20, 6, 0), None);
        assert_eq!(ProtectionPolicy::DISABLED.select(8, 1500, 21, 0), None);
    }

    #[test]
    fn prefix_nav_and_slot_words_cover_the_whole_exchange() {
        let data = SingleFramePasTiming {
            payload_extended: 110,
            payload_base: 100,
            ack: 44,
            total_airtime: 144,
            frame_kind: 0x11,
        };
        for kind in [Protection::Rts, Protection::CtsToSelf] {
            let timing = ProtectionTiming::compute(kind, 0, 0x17, data);
            assert_eq!(timing.nav, 154 + timing.cts);
            assert_eq!(
                timing.slot_duration(),
                u32::from(timing.extended) * 0x8000
                    + if kind == Protection::Rts {
                        0xa000 + u32::from(timing.cts)
                    } else {
                        0
                    }
            );
            let (words, len) = commands(
                timing,
                packet_ram::tx_command(0, 0) as u32,
                packet_ram::interface_metadata_byte(0) as u32,
                [1, 2, 3, 4, 5, 6],
                3,
            );
            assert_eq!(words[0] & 0x0007_0000, 3 << 16);
            assert_eq!(
                words[2] & 0xffff,
                if kind == Protection::Rts { 20 } else { 14 }
            );
            assert_eq!(
                words[3],
                if kind == Protection::Rts {
                    0x3100_00b4
                } else {
                    0x3100_00c4
                }
            );
            assert_eq!(words[6], 0x3200_0000 | u32::from(timing.nav));
            if kind == Protection::CtsToSelf {
                assert_eq!(&words[7..9], &[0x3303_0201, 0x3306_0504]);
            }
            assert_eq!(&words[len - 2..len], &[0x0700_4600, 0xf000_0000]);
            assert!(0x0c + len * 4 <= packet_ram::TX_COMMAND_SIZE);
        }
        assert_eq!(protection_rate(6, true), 0);
        assert_eq!(protection_rate(6, false), 6);
    }
}
