//! Vendor MAC wake restoration and post-channel reprogramming.

use crate::global::Global;
use core::cell::Cell;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use crate::{packet_ram, platform};

/// Per-interface low-MAC parameters (the vendor PAS record): activity, rates,
/// addresses and the EDCA/backoff bank the TX path draws from. Record 2 is the
/// synthetic record scanning uses.
pub(crate) struct Pas {
    /// 1 idle, 2 active.
    pub(crate) activity: Cell<u8>,
    pub(crate) slot_bits: Cell<u8>,
    pub(crate) mode: Cell<u8>,
    pub(crate) path_selector: Cell<u8>,
    pub(crate) basic_rates: Cell<u32>,
    pub(crate) own_mac: [Cell<u8>; 6],
    /// Rate-RAM column of this record's rate tables.
    pub(crate) rate_table_column: Cell<u8>,
    /// Fallback rate for ACK timing, per TX rate.
    pub(crate) rate_map: [Cell<u8>; 22],
    // Backoff bank, per access category (see `backoff`).
    pub(crate) retry_count: [Cell<u32>; 4],
    pub(crate) contention_window: [Cell<u32>; 4],
    pub(crate) cw_min: [Cell<u16>; 4],
    pub(crate) cw_max: [Cell<u16>; 4],
    pub(crate) txop_limit: [Cell<u16>; 4],
    pub(crate) slot_timing: Cell<u32>,
    /// EDCA AIFS values packed for MAC register 0x0e64.
    pub(crate) packed_aifs: Cell<u32>,
}

impl Pas {
    pub(crate) const fn new() -> Self {
        Self {
            activity: Cell::new(0),
            slot_bits: Cell::new(0),
            mode: Cell::new(0),
            path_selector: Cell::new(0),
            basic_rates: Cell::new(0),
            own_mac: [const { Cell::new(0) }; 6],
            rate_table_column: Cell::new(0),
            rate_map: [const { Cell::new(0) }; 22],
            retry_count: [const { Cell::new(0) }; 4],
            contention_window: [const { Cell::new(0) }; 4],
            cw_min: [const { Cell::new(0) }; 4],
            cw_max: [const { Cell::new(0) }; 4],
            txop_limit: [const { Cell::new(0) }; 4],
            slot_timing: Cell::new(0),
            packed_aifs: Cell::new(0),
        }
    }
}

pub(crate) const PAS_COUNT: usize = 3;

/// Low-MAC state shared by the channel, JOIN and TX paths.
pub(crate) struct LowMac {
    pub(crate) pas: [Pas; PAS_COUNT],
    /// PHY/rate configuration bits (`cfg` of the airtime helpers).
    pub(crate) rate_config: Cell<u16>,
    pub(crate) slot_time_base: Cell<u32>,
    pub(crate) slot_time_initial: Cell<u32>,
    pub(crate) slot_time_constant: Cell<u32>,
    /// ACK airtimes per TX rate, short and long preamble.
    pub(crate) short_airtimes: [Cell<u16>; 22],
    pub(crate) long_airtimes: [Cell<u16>; 22],
    /// PHY class of each TX rate's fallback, for retry durations.
    pub(crate) retry_rate_map: [Cell<u8>; 22],
    /// Last value written to MAC register 0x0e64.
    pub(crate) edca_slot_timing: Cell<u32>,
    /// MAC mode word (register 0x0200).
    pub(crate) mode: Cell<u32>,
    /// Response selector (register 0x0a04).
    pub(crate) beacon_selector: Cell<u32>,
    pub(crate) current_channel: Cell<u16>,
    /// Bit 0: packet receive disabled.
    pub(crate) receive_gate: Cell<u8>,
    /// 1 stopped .. 4 receiving.
    pub(crate) receive_state: Cell<u8>,
    /// Station MAC addresses of the two operating interfaces.
    pub(crate) own_mac: [[Cell<u8>; 6]; 2],
}

impl LowMac {
    pub(crate) const fn new() -> Self {
        Self {
            pas: [const { Pas::new() }; PAS_COUNT],
            rate_config: Cell::new(0),
            slot_time_base: Cell::new(0),
            slot_time_initial: Cell::new(0),
            slot_time_constant: Cell::new(0),
            short_airtimes: [const { Cell::new(0) }; 22],
            long_airtimes: [const { Cell::new(0) }; 22],
            retry_rate_map: [const { Cell::new(0) }; 22],
            edca_slot_timing: Cell::new(0),
            mode: Cell::new(0),
            beacon_selector: Cell::new(0),
            current_channel: Cell::new(0),
            receive_gate: Cell::new(0),
            receive_state: Cell::new(0),
            own_mac: [const { [const { Cell::new(0) }; 6] }; 2],
        }
    }

    /// PAS record of `interface` (0..=2).
    pub(crate) fn pas(&self, interface: impl Into<usize>) -> &Pas {
        &self.pas[interface.into().min(PAS_COUNT - 1)]
    }
}

static LOW_MAC: Global<LowMac> = Global::new(LowMac::new());

pub(crate) fn low_mac() -> &'static LowMac {
    &LOW_MAC
}

/// The channel the MAC is programmed for, 0 when stopped.
pub fn current_channel() -> u16 {
    LOW_MAC.current_channel.get()
}

/// The MAC's packet offset of `address`, which must lie in runtime packet
/// RAM.
fn packet_offset(address: usize) -> u32 {
    if !(packet_ram::RUNTIME_CPU_START as usize..packet_ram::RUNTIME_CPU_END as usize)
        .contains(&address)
    {
        crate::halt_always!();
    }
    packet_ram::encode_mac_packet_offset_u32(address as u32)
}

pub(crate) const RATE_ENCODING: [u8; 22] = [
    0, 0, 1, 1, 0, 0, 2, 3, 4, 5, 6, 7, 8, 9, 2, 4, 5, 6, 7, 8, 9, 10,
];
// Matching-container DTCM `0x04000138..0x0400014c`, consumed by vendor
// `tx_build_duration_desc()` through the rate map at `0x04001aec`.
pub(crate) const TX_DURATION_TIMING: [u16; 10] = [
    0x00c2, 0x00c2, 0x001c, 0x0000, 0x001e, 0x0021, 0x0002, 0x0004, 0x000b, 0x0016,
];

pub(crate) const RATE_ATTRIBUTE: [u8; 22] = [
    0, 1, 2, 3, 3, 3, 11, 15, 10, 14, 9, 13, 8, 12, 0, 1, 2, 3, 4, 5, 6, 7,
];
const PRIMARY_MCS_FALLBACK: [u8; 8] = [6, 8, 9, 10, 11, 12, 13, 13];
const SECONDARY_MCS_FALLBACK: [u8; 8] = [14, 15, 16, 17, 18, 19, 20, 21];
const CCK_DIVISORS: [u16; 4] = [2, 4, 11, 22];
const OFDM_DIVISORS: [u16; 16] = [
    24, 36, 48, 72, 96, 144, 192, 216, 26, 52, 78, 104, 156, 208, 234, 260,
];
const OFFSET_TABLE: [[u8; 4]; 6] = [
    [0x00, 0x60, 0x30, 0x70],
    [0x04, 0x64, 0x34, 0x74],
    [0x08, 0x68, 0x38, 0x78],
    [0, 0, 0, 0],
    [0x10, 0x20, 0x40, 0x50],
    [0x18, 0x28, 0x48, 0x58],
];
const RATE_PAIRS: [(u8, u8); 31] = [
    (1, 0),
    (1, 1),
    (1, 2),
    (1, 3),
    (0, 1),
    (0, 2),
    (0, 3),
    (2, 6),
    (2, 7),
    (2, 8),
    (2, 9),
    (2, 10),
    (2, 11),
    (2, 12),
    (2, 13),
    (5, 14),
    (5, 15),
    (5, 16),
    (5, 17),
    (5, 18),
    (5, 19),
    (5, 20),
    (5, 21),
    (4, 14),
    (4, 15),
    (4, 16),
    (4, 17),
    (4, 18),
    (4, 19),
    (4, 20),
    (4, 21),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MacStartupError {
    PacketDmaStopTimeout,
    PipeControllerTimeout,
}

/// A MAC register, or one of the MAC's control objects in packet RAM.
fn window(address: usize) -> crate::mmio::RegisterWindow {
    if address >= 0x09c6_0000 {
        crate::mmio::TX_RINGS
    } else if address >= 0x09c0_0000 {
        crate::mmio::MAC
    } else {
        crate::mmio::mac_objects(address)
    }
}

fn read_u32(address: usize) -> u32 {
    window(address).read32(address)
}

fn write_u16(address: usize, value: u16) {
    window(address).write16(address, value);
}

fn write_u32(address: usize, value: u32) {
    window(address).write32(address, value);
}

const fn ceil_div(numerator: u32, denominator: u32) -> u32 {
    numerator.wrapping_add(denominator - 1) / denominator
}

pub const fn rate_phy_class(cfg: u16, rate: u8, stream: bool) -> u8 {
    if rate < 4 {
        if cfg & 0x100 != 0 || rate == 0 || (cfg & 0x400 != 0 && rate == 1) {
            1
        } else {
            0
        }
    } else if rate < 14 {
        2
    } else if stream {
        5
    } else {
        4
    }
}

pub fn base_airtime(cfg: u16, rate: u8, length: u16, stream: bool) -> u16 {
    let class = rate_phy_class(cfg, rate, stream);
    let value = if class < 2 {
        ceil_div(
            u32::from(length) * 16,
            u32::from(CCK_DIVISORS[(rate & 3) as usize]),
        ) + if class == 1 { 0xc0 } else { 0x60 }
    } else {
        let numerator = u32::from(length) * 8 + 0x16;
        let mut value = 4 * ceil_div(
            numerator,
            u32::from(OFDM_DIVISORS[((rate - 6) & 15) as usize]),
        );
        if cfg & 0x10 != 0 {
            value += 6;
        }
        value += match class {
            2 => 0x14,
            4 => 0x18,
            _ => 0x24,
        };
        if class == 2 && cfg & 0x40 != 0 {
            value *= 2;
        }
        value
    };
    value as u16
}

pub fn extended_airtime(cfg: u16, rate: u8, length: u16, stream: bool) -> u16 {
    let class = rate_phy_class(cfg, rate, stream);
    let mut value = u32::from(base_airtime(cfg, rate, length, stream));
    value += if cfg & 0x40 != 0 {
        0x20
    } else if cfg & 0x10 != 0 && class < 3 {
        0x0a
    } else {
        0x10
    };
    value as u16
}

pub fn ofdm_duration(rate: u8, length: u16) -> u16 {
    if rate < 6 {
        return 0;
    }
    let value = ceil_div(
        u32::from(length) * 8 + 0x16,
        u32::from(OFDM_DIVISORS[((rate - 6) & 15) as usize]),
    );
    ((value * 3 + 9) & 0xfff) as u16
}

fn fill_fallbacks(mut basic: u32) -> [u8; 22] {
    for rate in 0..4 {
        let bit = 1_u32 << rate;
        if basic & bit != 0 {
            break;
        }
        basic |= bit & 0x0f;
    }
    for rate in 6..11 {
        let bit = 1_u32 << rate;
        if basic & bit != 0 {
            break;
        }
        basic |= bit & 0x540;
    }
    let mut result = [0_u8; 22];
    let mut fallback = 0_u8;
    for rate in 0..14 {
        if 0x003f_ffcf & (1_u32 << rate) != 0 {
            if basic & (1_u32 << rate) != 0 {
                fallback = rate as u8;
            }
            result[rate] = fallback;
        }
    }
    fallback = 6;
    for rate in 14..22 {
        let candidate = PRIMARY_MCS_FALLBACK[rate - 14];
        if basic & (1_u32 << candidate) != 0 {
            fallback = candidate;
        }
        result[rate] = fallback;
    }
    result
}

fn secondary_fallbacks(basic: u32, initial: u8) -> [u8; 8] {
    let mut result = [initial; 8];
    let mut fallback = initial;
    for (index, candidate) in SECONDARY_MCS_FALLBACK.into_iter().enumerate() {
        if basic & (1_u32 << candidate) != 0 {
            fallback = candidate;
        }
        result[index] = fallback;
    }
    result
}

fn build_rate_entry(cfg: u16, hardware_class: u8, fallback: u8) -> [u32; 4] {
    let d14 = extended_airtime(cfg, fallback, 14, true);
    let d32 = extended_airtime(cfg, fallback, 32, true);
    let mut class = hardware_class;
    if hardware_class < 3 && (fallback == 0 || (fallback == 1 && cfg & 0x400 != 0)) {
        class = 1;
    } else if hardware_class >= 3 {
        class = rate_phy_class(cfg, fallback, true);
    }
    let duration14 = u32::from(ofdm_duration(fallback, 14));
    let duration32 = u32::from(ofdm_duration(fallback, 32));
    [
        u32::from(d14) | (u32::from(RATE_ATTRIBUTE[fallback as usize]) << 24),
        u32::from(d32)
            | (u32::from(RATE_ENCODING[fallback as usize]) << 24)
            | (u32::from(class) << 28),
        duration14 | (duration14 << 12),
        duration32 | (duration32 << 12),
    ]
}

fn program_rate_tables(pas: &Pas) {
    let low_mac = low_mac();
    let cfg = low_mac.rate_config.get();
    let basic = pas.basic_rates.get();
    let fallbacks = fill_fallbacks(basic);
    for (rate, fallback) in fallbacks.iter().copied().enumerate() {
        if 0x003f_ffcf & (1_u32 << rate) == 0 {
            continue;
        }
        pas.rate_map[rate].set(fallback);
        low_mac.short_airtimes[rate].set(extended_airtime(cfg, fallback, 14, true));
        low_mac.long_airtimes[rate].set(extended_airtime(cfg, fallback, 32, true));
        low_mac.retry_rate_map[rate].set(rate_phy_class(cfg, fallback, true));
    }
    let secondary = secondary_fallbacks(basic, fallbacks[21]);

    let column = if pas.rate_table_column.get() == 0 {
        0
    } else {
        2
    };
    for (hardware_class, rate) in RATE_PAIRS {
        let fallback = fallbacks[rate as usize];
        let mut index = OFFSET_TABLE[hardware_class as usize][column]
            .wrapping_add(RATE_ATTRIBUTE[rate as usize]);
        if hardware_class == 2 {
            index = index.wrapping_sub(8);
        }
        let entry = build_rate_entry(cfg, hardware_class, fallback);
        for (word, value) in entry.into_iter().enumerate() {
            write_u32(packet_ram::rate_entry(usize::from(index)) + word * 4, value);
        }
        if hardware_class >= 4 {
            let secondary_index = OFFSET_TABLE[hardware_class as usize][1]
                .wrapping_add(RATE_ATTRIBUTE[rate as usize]);
            let entry = build_rate_entry(cfg, hardware_class, secondary[usize::from(rate) - 14]);
            for (word, value) in entry.into_iter().enumerate() {
                write_u32(
                    packet_ram::rate_entry(usize::from(secondary_index)) + word * 4,
                    value,
                );
            }
        }
    }
}

/// Rebuild the selected active PAS rate tables after JOIN activates the
/// record. Vendor `mac_apply_channel_and_vif_config` activates the PAS record
/// before `pas_reprogram_all_vif_rate_tables`; the rebuilt JOIN path performs
/// the channel transition first, so it must make this call after activation.
///
/// The shared rate RAM must be exclusively owned.
/// Pause the MAC's receive side for about 2 µs (vendor
/// `mac_rx_pause_briefly`, 0x30a): bits 2..5 of 0x09c00e8c cleared, then
/// restored. The pipe watchdog does this when a pipe nears expiry, so a
/// receiver busy with a frame stream lets the pipe's transmission finish.
#[cfg(target_arch = "arm")]
pub fn pause_receive_briefly() {
    let register = platform::mac_register(0x0e8c);
    let saved = read_u32(register);
    write_u32(register, saved & 0xffff_ffc3);
    let start = platform::timer_now();
    while platform::timer_now().wrapping_sub(start) < 2 {
        core::hint::spin_loop();
    }
    write_u32(register, saved);
}

pub fn program_active_vif_rate_tables(interface: u8) -> bool {
    if usize::from(interface) >= PAS_COUNT {
        return false;
    }
    program_rate_tables(low_mac().pas(interface));
    true
}

/// Restore joined-station slot timing after the synthetic scan channel
/// transition. The scan PAS timing word is zero in the open runtime; JOIN only
/// supports 2.4 GHz and must restore its 9 us slot-time base.
pub fn program_joined_station_slot_timing(interface: u8) {
    let low_mac = low_mac();
    program_slot_timings(
        low_mac.rate_config.get(),
        low_mac.pas(interface).slot_timing.get(),
    );
}

fn program_pipe_slot(pointer: usize, slot: u8) {
    let Some(bus_address) = packet_ram::response_command_bus_address(pointer) else {
        return;
    };
    if usize::from(slot) >= packet_ram::RESPONSE_POINTER_COUNT {
        return;
    }
    let logical = if slot == 0x1b {
        30
    } else if slot == 0x1c {
        31
    } else if let Some(logical) = slot.checked_sub(2) {
        logical
    } else {
        return;
    };
    write_u32(packet_ram::response_pointer(usize::from(slot)), bus_address);
    let register_bit = if logical == 30 { 31 } else { logical };
    for address in [
        crate::platform::mac_register(0x0a08),
        crate::platform::mac_register(0x0a0c),
    ] {
        let value = read_u32(address) | (1_u32 << register_bit);
        write_u32(address, value);
    }
    let value = read_u32(crate::platform::mac_register(0x0a10)) & !(1_u32 << logical) & 0x00ff_ffff;
    write_u32(crate::platform::mac_register(0x0a10), value);
}

/// The TSF: the MAC's 64-bit microsecond clock (vendor `tsf_read`).
pub fn tsf() -> u64 {
    loop {
        let high = read_u32(platform::mac_register(0x0e3c));
        let low = read_u32(platform::mac_register(0x0e38));
        if read_u32(platform::mac_register(0x0e3c)) == high {
            return u64::from(high) << 32 | u64::from(low);
        }
    }
}

/// The descriptor the MAC sends a beacon from: vendor `beacon_build_tx_desc`
/// (`0x2eae`) and `txp_build_beacon_pipe_words` (`0x2f38`), in response
/// command 3. The frame lies at `frame` in packet RAM; its sequence number
/// comes from the hardware counter `sequence_slot`, and `0x0700_8400` before
/// the body has the MAC fill in the timestamp (octets 24..32).
///
/// The MAC must not be sending from the descriptor (beacon disarmed).
pub fn build_beacon_descriptor(
    frame: u32,
    length: u16,
    rate: u8,
    frame_control: u16,
    sequence_slot: u8,
) -> bool {
    let Some(body) =
        packet_ram::RuntimePacketAddress::new(frame + 0x20, usize::from(length) - 0x20)
    else {
        return false;
    };
    let low_mac = low_mac();
    let pointer = packet_ram::response_command(3);
    // Vendor `+0x38` of the slot timings: 16 slots plus 2.
    let gap = low_mac
        .slot_time_base
        .get()
        .wrapping_mul(16)
        .wrapping_add(low_mac.slot_time_constant.get())
        & 0x1fff;
    write_u32(pointer, gap | gap << 16 | 0xe000_e000);
    write_u16(pointer + 4, 0);
    // A fixed backoff: an access point beacons at TBTT (IBSS stations
    // randomize theirs).
    write_u16(pointer + 6, 0xbc01);
    write_u32(pointer + 8, 0x5800_0000);
    let phy = crate::tx::build_phy_rate_words(
        rate,
        0,
        0,
        0,
        RATE_ENCODING.get(usize::from(rate)).copied().unwrap_or(0),
    );
    let hardware_rate = RATE_ATTRIBUTE.get(usize::from(rate)).copied().unwrap_or(0);
    let words = [
        0x5100_0000 | (phy.rate & 0x00ff_ffff),
        0x5000_0000 | (crate::tx::finalize_phy_control(phy, rate, length) & 0x00ff_ffff),
        0x5200_0000 | u32::from(hardware_rate) << 16 | u32::from(length + 4),
        0x3200_0000 | u32::from(frame_control),
        // Duration 0.
        0x3200_0000,
        0x2900_0000 | packet_ram::encode_mac_packet_offset_u32(frame + 4),
        crate::tx::single_frame_secondary_command(0, 0, sequence_slot),
        0x0700_8400,
        0x4000_0000 | body.tx_payload_bus_address(0x007f_fffc).raw(),
        (u32::from(length - 0x20) & 0x0fff) << 12 | (body.raw() & 3),
        0x0700_4600,
        0xf000_0000,
    ];
    for (index, word) in words.into_iter().enumerate() {
        write_u32(pointer + 0x0c + index * 4, word);
    }
    true
}

/// Prepare beaconing (vendor `txp_program_duration`, `0x100dc`): slot 0x10
/// gets an idle descriptor, and `0x0e94` the airtime of 24 octets at
/// `rate`.
pub fn start_beaconing(rate: u8) {
    program_pipe_slot(packet_ram::response_command(8), 0x10);
    let cfg = low_mac().rate_config.get();
    let airtime = u32::from(base_airtime(cfg, rate, 0x18, false));
    let airtime = if cfg & 0x12 == 0x12 {
        airtime - 5
    } else if cfg & 0x41 == 0x40 {
        airtime + 3
    } else {
        airtime + 1
    };
    write_u32(platform::mac_register(0x0e94), airtime & 0xffff);
}

tock_registers::register_bitfields! {
    u32,
    BeaconTimer [
        /// Low 24 bits of the target beacon transmission time, in TSF
        /// microseconds.
        TBTT OFFSET(0) NUMBITS(24) [],
        /// Set when arming.
        ARM OFFSET(30) NUMBITS(1) [],
    ],
    /// The firmware starts from `0x07e3_b85c`, whose other set bits are not
    /// described, and adds one group of four bits per active interface
    /// record: path 0 when the record's address is interface 0's, path 1
    /// otherwise. Scanning uses the base with path 1, `0x07f3_bd5e`.
    MacMode [
        /// In path 0's mask, `0x0008_0281`.
        PATH0_0 OFFSET(0) NUMBITS(1) [],
        /// In path 1's mask, `0x0010_0502`.
        PATH1_1 OFFSET(1) NUMBITS(1) [],
        /// In path 0's mask, `0x0008_0281`.
        PATH0_7 OFFSET(7) NUMBITS(1) [],
        /// In path 1's mask, `0x0010_0502`.
        PATH1_8 OFFSET(8) NUMBITS(1) [],
        /// In path 0's mask, `0x0008_0281`.
        PATH0_9 OFFSET(9) NUMBITS(1) [],
        /// In path 1's mask, `0x0010_0502`.
        PATH1_10 OFFSET(10) NUMBITS(1) [],
        /// Set if any active interface record has mode 2.
        MODE2 OFFSET(14) NUMBITS(1) [],
        /// In path 0's mask, `0x0008_0281`.
        PATH0_19 OFFSET(19) NUMBITS(1) [],
        /// In path 1's mask, `0x0010_0502`.
        PATH1_20 OFFSET(20) NUMBITS(1) [],
    ],
    /// Shared by `OWN_MAC_MODE`, `BSSID_MODE` and `SECOND_ADDR_MODE`.
    MatchMode [
        /// Set to use the entry; 0 disables it.
        ENABLE OFFSET(0) NUMBITS(1) [],
        /// Match role.
        ROLE OFFSET(8) NUMBITS(2) [
            /// Own address, or the BSSID of an AP or IBSS interface.
            OWN = 1,
            /// The BSSID of the AP a station is joined to. With `ROLE` = 1 in
            /// `BSSID_MODE` instead, the MAC answers the AP's aggregates from an
            /// unused pipe.
            STATION = 3
        ],
    ],
    /// One bit per response pointer slot in `RESPONSE_ENABLE`,
    /// `RESPONSE_BITMAP_SET` and `RESPONSE_BITMAP_CLEAR`. The firmware masks
    /// its `RESPONSE_STATE` writes to bits 23:0. Bits 30:25 are never
    /// written; no slot above 22 is installed, so bits 24:21 stay clear too.
    ResponseSlots [
        /// Bit *n* stands for pointer slot *n* + 2, slots 2 to 26.
        SLOT OFFSET(0) NUMBITS(25) [],
        /// Slots 27 and 28, both, in the two bitmaps. Never installed.
        SLOT_27_28 OFFSET(31) NUMBITS(1) [],
    ],
    /// The firmware writes `0x1030_0000` plus this bit; the other set bits
    /// are not described.
    BeaconControl [
        /// Send the beacon descriptor in pointer slot 0 at the next TBTT.
        ARM OFFSET(0) NUMBITS(1) [],
    ],
    EventReadiness [
        /// Set when the event FIFO holds no more events. The firmware drains
        /// the FIFO only while this bit is clear.
        EMPTY OFFSET(31) NUMBITS(1) [],
    ],
}

/// Have the MAC send the beacon descriptor at `tbtt` (vendor
/// `tsf_align_to_beacon_interval`, `0x2d38`): pointer slot 0, bit 0 of
/// `0x0a00`, and the TBTT's low 24 bits in `0x0e08`.
pub fn arm_beacon(tbtt: u64) {
    write_u32(
        packet_ram::response_pointer(0),
        packet_offset(packet_ram::response_command(3)),
    );
    crate::cpu::drain_write_buffer();
    write_u32(platform::mac_register(0x0a00), 0x1030_0001);
    write_u32(
        platform::mac_register(0x0e08),
        0x4000_0000 | (tbtt as u32 & 0x00ff_ffff),
    );
}

/// Stop beaconing (vendor `mac_disable_beacon_hw`, `0x10172`).
pub fn disarm_beacon() {
    write_u32(platform::mac_register(0x0e08), 0);
    write_u32(platform::mac_register(0x0a00), 0x1030_0000);
    write_u32(
        packet_ram::response_pointer(0),
        packet_offset(packet_ram::response_command(8)),
    );
}

/// An access point matches frames on its own address only: no BSSID entry.
pub fn clear_bssid() {
    write_u32(platform::mac_register(0x0044), 0);
}

fn build_control_frame(if_id: u8, pointer: usize, ack: bool) {
    write_u32(
        pointer,
        low_mac().slot_time_constant.get().wrapping_shl(16) | 0x2000_0000,
    );
    write_u16(pointer + 4, 0);
    write_u16(pointer + 6, 0);
    write_u32(pointer + 8, 0x5800_0000);
    let words = [
        0x5900_0000,
        0x5800_0003,
        0x5a00_000e,
        if ack { 0x3100_00d4 } else { 0x3100_00c4 },
        0x4700_0000,
        metadata_source_word(if_id),
        if ack { 0x0700_2008 } else { 0x0700_2004 },
        0x0700_6828,
        0x0700_4600,
        0xf000_0000,
    ];
    for (index, value) in words.into_iter().enumerate() {
        write_u32(pointer + 0x0c + index * 4, value);
    }
    let slot = if ack { 0x15 + if_id } else { 9 + if_id };
    program_pipe_slot(pointer, slot);
}

/// Build and install the vendor immediate ACK/CTS and response descriptors.
/// Channel transition can skip `mac_reprogram_after_channel` when its retained
/// wake flags select the already-restored branch, so JOIN must explicitly
/// republish these slots after activating the STA record.
///
/// Packet RAM and the MAC pipe-controller registers must be exclusively owned.
pub fn program_immediate_response_descriptors() {
    build_control_frame(0, packet_ram::response_command(4), true);
    build_control_frame(1, packet_ram::response_command(5), true);
    build_control_frame(0, packet_ram::response_command(6), false);
    build_control_frame(1, packet_ram::response_command(7), false);
    install_response_descriptors();
}

fn save_register_context() {
    // The vendor restores these from a register context nothing fills in.
    write_u32(crate::platform::mac_register(0x1404), 0);
    write_u32(crate::platform::mac_register(0x1408), 0);
    write_u32(crate::platform::mac_register(0x140c), 0);
    write_u32(crate::platform::mac_register(0x1410), 0);
    write_u32(crate::platform::mac_register(0x1400), 0);
}

/// Build the temporary mode-zero channel/VIF image used by
/// `syn_scan_program_channel()` before `mac_apply_channel_and_vif_config()`.
pub fn prepare_scan_context(_channel: u16) {
    let low_mac = low_mac();
    write_u32(crate::platform::mac_register(0x0200), 0);
    low_mac.rate_config.set(0x0117);

    // `syn_scan_program_channel` publishes the temporary VIF index and
    // active-record mask before entering `mac_apply_channel_and_vif_config`.
    crate::vif::publish_synthetic_scan_record();

    // Synthetic scan record 2 is marked as scan-active by
    // `phy_set_band_reg`/`mac_apply_channel_and_vif_config`.
    let scan_pas = low_mac.pas(2_usize);
    scan_pas.activity.set(2);
    scan_pas.mode.set(0x0f);
    scan_pas.basic_rates.set(1);
    // Base 0x07e3b85c plus the non-matching temporary-record mask 0x00100502
    // from the vendor scan path.
    low_mac.mode.set(0x07f3_bd5e);
}

/// Temporary station-mode register image installed by the vendor synthetic
/// scan path after `phy_do_channel_switch()` returns.
pub fn program_before_scan_channel(channel: u16) {
    let low_mac = low_mac();
    write_u32(
        crate::platform::mac_register(0x0800),
        packet_offset(packet_ram::rate_ram().start),
    );
    program_slot_timings(
        low_mac.rate_config.get(),
        low_mac.pas(2_usize).slot_timing.get(),
    );

    let mut first_active = false;
    let mut second_active = false;
    for pas in low_mac.pas.iter().filter(|pas| pas.activity.get() == 2) {
        pas.rate_table_column.set(0);
        if first_active && !second_active {
            second_active = true;
            pas.rate_table_column.set(1);
            write_u32(
                crate::platform::mac_register(0x0270),
                if pas.path_selector.get() == 0 {
                    0x0100_0000
                } else {
                    1 << 26
                },
            );
        } else {
            first_active = true;
        }
        program_rate_tables(pas);
    }
    write_u32(
        crate::platform::mac_register(0x0314),
        if second_active { 0x0100_0000 } else { 0 },
    );
    low_mac.current_channel.set(channel);
}

pub fn program_scan_station_mode() {
    let low_mac = low_mac();
    // Final publication at 0xfa34 in `mac_apply_channel_and_vif_config`.
    low_mac.beacon_selector.set(0x0018_0180);
    write_u32(crate::platform::mac_register(0x0a04), 0x0018_0180);
    write_u32(crate::platform::mac_register(0x0a1c), 0x827b_ffdf);
    write_u32(crate::platform::mac_register(0x0204), 0x0019_8000);
    write_u32(crate::platform::mac_register(0x0200), low_mac.mode.get());
    write_u32(crate::platform::mac_register(0x0310), 0x7800_0000);
}

fn active_station_mode_word() -> u32 {
    let low_mac = low_mac();
    let reference_path = low_mac.own_mac[0][5].get();
    low_mac
        .pas
        .iter()
        .filter(|pas| pas.activity.get() == 2)
        .fold(0x07e3_b85c_u32, |mode, pas| {
            let path_mask = if pas.own_mac[5].get() == reference_path {
                0x0008_0281
            } else {
                0x0010_0502
            };
            mode | path_mask | if pas.mode.get() == 2 { 0x4000 } else { 0 }
        })
}

pub fn program_joined_station_mode() {
    let low_mac = low_mac();
    let mode = active_station_mode_word();
    low_mac.mode.set(mode);
    // The live joined record selects vendor `mac_program_mode_regs`, not the
    // synthetic-scan `mac_program_mode_sta` tail. The latter's 0x00198000
    // filter excludes received compressed BlockAck frames from packet DMA even
    // though the AP transmits them and TX status completes.
    low_mac.beacon_selector.set(0x0018_0783);
    write_u32(crate::platform::mac_register(0x0a04), 0x0018_0783);
    write_u32(crate::platform::mac_register(0x0a1c), 0x827b_ffdf);
    write_u32(crate::platform::mac_register(0x0204), 0x0279_fe00);
    write_u32(crate::platform::mac_register(0x0200), mode);
    write_u32(crate::platform::mac_register(0x0310), 0x7800_0000);
}

/// Station BSSID publication from vendor `mac_program_bssid` (`0x10b1a`).
/// The vendor passes mode 3 (`0x301`) for a station and mode 1 (`0x101`)
/// only for AP and IBSS interfaces; with mode 1 the MAC answers the AP's
/// A-MPDUs from an unused receive BlockAck pipe.
///
/// Address-match registers must be exclusively owned.
#[cfg(target_arch = "arm")]
pub fn program_joined_bssid(bssid: [u8; 6]) {
    write_u32(
        crate::platform::mac_register(0x003c),
        u32::from_le_bytes([bssid[0], bssid[1], bssid[2], bssid[3]]),
    );
    write_u32(
        crate::platform::mac_register(0x0040),
        u32::from(u16::from_le_bytes([bssid[4], bssid[5]])),
    );
    write_u32(crate::platform::mac_register(0x0044), 0x301);
}

pub fn reprogram_after_channel() {
    let low_mac = low_mac();
    write_u32(
        crate::platform::mac_register(0x0800),
        packet_offset(packet_ram::rate_ram().start),
    );
    for pas in low_mac.pas.iter().filter(|pas| pas.activity.get() == 2) {
        program_rate_tables(pas);
    }
    program_immediate_response_descriptors();
    write_u32(crate::platform::mac_register(0x0200), low_mac.mode.get());
    save_register_context();
}

fn rebuild_pipe_state() {
    // `prepare_packet_dma` already programmed these once, before MAC core
    // enable, which is where vendor's `mac_hw_init_pipes` does it and the only
    // time vendor does it (`annotated-main.c:18541-18587`). Repeating it here
    // re-writes ring+0x14 (GO) while the MAC is live, and cycles 0x09c00e8c
    // 0xbf -> 0 -> 0xbf underneath a running command-fetch engine. Final
    // register values are identical, so this only matters if any of it
    // disturbs fetch state.
    write_u32(crate::platform::mac_register(0x0e8c), 0);
    write_u32(crate::platform::mac_register(0x0e60), 2);
    write_u32(crate::platform::mac_register(0x0e80), 0x100);
    write_u32(crate::platform::mac_register(0x0e88), 0xff);
    let descriptors = [
        crate::platform::tx_ring_register_offset(0x0000),
        crate::platform::tx_ring_register_offset(0x0080),
        crate::platform::tx_ring_register_offset(0x0100),
        crate::platform::tx_ring_register_offset(0x0180),
    ];
    for (pipe, base) in descriptors.into_iter().enumerate() {
        write_u32(base + 0x0c, packet_offset(packet_ram::tx_command(pipe, 0)));
        write_u32(base + 0x10, packet_ram::TX_COMMAND_SIZE as u32);
        write_u32(base + 0x14, 1);
    }
    for index in 0..4 {
        let descriptor = descriptors[index];
        let slot = ((read_u32(descriptor + 0x20) & 0x07ff_ffff) >> 24) as u8;
        crate::tx::pipes().reset(index as u8, slot, descriptor as u32);
    }
    write_u32(crate::platform::mac_register(0x0e8c), 0xbf);
    let list = packet_ram::automatic_response_list().start;
    write_u32(list, 0x4e14_0000);
    for index in 0..33 {
        write_u32(list + 4 + index * 4, 0x2200_0000 | packet_offset(list));
    }
    write_u32(list + 4 + 33 * 4, 0xf000_0000);
}

/// Builds the vendor pipe-record pointers needed before any TX descriptor can
/// be prepared for hardware ownership.
///
/// Packet DMA and the four hardware pipe blocks must already be initialized.
pub fn initialize_tx_pipe_state() {
    rebuild_pipe_state();
    // Vendor packet-controller startup publishes both global retry timing
    // terms. The second survives retained startup on this target, while the
    // first otherwise remains zero and shortens every retry by 18 us.
    low_mac().slot_time_base.set(9);
    low_mac().slot_time_initial.set(10);
    // `txp_submit_to_pipe` emits a 0x20800000 descriptor command sourcing
    // one byte from this per-interface packet-SRAM metadata vector. Hardware
    // ORs that byte into the high frame-control octet. Vendor startup clears
    // the vector; leaving retained packet data here turned 0x00b0
    // authentication frames into protected/ToDS/more-data 0x61b0 frames.
    write_u32(packet_ram::interface_metadata(), 0);
}

/// Hardware-owning prefix of vendor `mac_radio_stop` for the current
/// no-active-VIF scan branch. TX ownership must already be proven empty.
#[cfg(target_arch = "arm")]
pub fn begin_unjoined_scan_radio_stop() {
    write_u32(crate::platform::mac_register(0x0a28), 0);
    write_u32(crate::platform::mac_register(0x0a00), 0x1030_0000);
    write_u32(
        crate::platform::mac_register(0x0a04),
        low_mac().beacon_selector.get(),
    );
    for index in 0..32 {
        write_u32(
            packet_ram::response_pointer(index),
            packet_offset(packet_ram::response_command(8)),
        );
    }
}

/// Final state publication from vendor `mac_radio_stop`, after packet RX,
/// PHY command 7, and software RX draining have completed.
pub fn finish_unjoined_scan_radio_stop() {
    let low_mac = low_mac();
    low_mac.current_channel.set(0);
    low_mac.receive_state.set(0);
    low_mac.pas.iter().for_each(|pas| pas.activity.set(1));
}

fn build_tbtt(pointer: usize) {
    let low_mac = low_mac();
    let n = low_mac
        .slot_time_constant
        .get()
        .wrapping_add(low_mac.slot_time_base.get().wrapping_mul(8))
        & 0x1fff;
    write_u32(pointer, n | (n << 16) | 0x2000_0000);
    write_u16(pointer + 6, 0xdc01);
    write_u32(pointer + 8, 0x8000_0000);
}

/// Uninterrupted hardware and shared-state setup inspired by vendor `0x0000014c`.
///
/// Packet DMA and the MAC clock domain must already be initialized.
pub fn initialize_startup_state(max_polls: u32) -> Result<(), MacStartupError> {
    // 0x14e -> 0x7e50: stop packet DMA and wait for bit 23 to clear.
    use crate::platform::RxDmaControl::{ACTIVE, ENABLE};
    let dma = crate::platform::rx_dma();
    if dma.control.is_set(ENABLE) {
        dma.control.modify(ENABLE::CLEAR);
        let mut polls = 0;
        while dma.control.is_set(ACTIVE) {
            if polls >= max_polls {
                return Err(MacStartupError::PacketDmaStopTimeout);
            }
            polls += 1;
            core::hint::spin_loop();
        }
    }
    let low_mac = low_mac();
    low_mac.receive_gate.set(low_mac.receive_gate.get() | 1);

    // 0x152 -> 0x10024: collapse producer, consumer, scan, and release.
    dma.consumer.set(dma.producer.get());
    dma.control.set(dma.control.get());

    // Normal startup clears these two descriptor-source halfwords here;
    // `fw_subsystem_init` never copies them from PAS_BASE-8.
    write_u16(packet_ram::duration_word(0), 0);
    write_u16(packet_ram::duration_word(1), 0);
    // Vendor initialized DTCM supplies zero for the EDCA hardware cache.
    low_mac.edca_slot_timing.set(0);
    low_mac.rate_config.set(0x13);
    low_mac.beacon_selector.set(0);

    for pointer in [
        packet_ram::response_command(8),
        packet_ram::response_command(0),
        packet_ram::response_command(1),
        packet_ram::response_command(2),
        packet_ram::response_command(3),
        packet_ram::response_command(4),
        packet_ram::response_command(5),
        packet_ram::response_command(6),
        packet_ram::response_command(7),
        packet_ram::response_command(11),
        packet_ram::response_command(12),
    ] {
        build_tbtt(pointer);
    }

    write_u32(
        crate::platform::mac_register(0x0c00),
        packet_offset(packet_ram::response_pointers().start),
    );
    for index in 0..32 {
        write_u32(
            packet_ram::response_pointer(index),
            packet_offset(packet_ram::response_command(8)),
        );
    }
    for index in 0..23 {
        write_u32(crate::platform::mac_register(0x0210) + index * 4, 0);
    }

    let mut polls = 0;
    while read_u32(crate::platform::mac_register(0x0a20)) & 0x8000_0000 == 0 {
        if polls >= max_polls {
            return Err(MacStartupError::PipeControllerTimeout);
        }
        polls += 1;
        core::hint::spin_loop();
    }

    // 0x26a -> 0x52c.
    platform::prepare_mac_receive_hardware();

    low_mac.receive_state.set(0);

    crate::tx::pipes().reset_control();
    Ok(())
}

fn program_slot_timings(cfg: u16, base: u32) {
    let low_mac = low_mac();
    let initial = if cfg & 0x20 != 0 { 0x10 } else { 10 };
    low_mac.slot_time_initial.set(initial);
    low_mac.slot_time_constant.set(2);
    low_mac.slot_time_base.set(base);
    let x16 = base.wrapping_mul(0x10).wrapping_add(2);
    let x24 = base.wrapping_mul(0x18).wrapping_add(2);
    write_u32(
        crate::platform::mac_register(0x0e30),
        base.wrapping_mul(8).wrapping_sub(1),
    );
    write_u32(crate::platform::mac_register(0x0e58), x16);
    write_u32(crate::platform::mac_register(0x0e5c), x24);
    for (address, value) in [
        (crate::platform::mac_register(0x0618), 0x0000_004d),
        (crate::platform::mac_register(0x0614), 0x0000_01cd),
        (crate::platform::mac_register(0x0428), 0x0000_049d),
        (crate::platform::mac_register(0x042c), 0x0000_049d),
        (crate::platform::mac_register(0x0444), 0x0000_0258),
        (crate::platform::mac_register(0x0434), 0x0000_003d),
        (crate::platform::mac_register(0x0438), 0x0000_003d),
        (crate::platform::mac_register(0x0448), 0x0000_0258),
        (crate::platform::mac_register(0x043c), 0x0000_0298),
        (crate::platform::mac_register(0x0440), 0x0000_0298),
        (crate::platform::mac_register(0x044c), 0x0000_0134),
    ] {
        write_u32(address, value);
    }
    write_u32(crate::platform::mac_register(0x0810), 0);
}

/// Descriptor command sourcing interface `if_id`'s metadata byte, which the
/// MAC ORs into the high frame-control octet. The vendor's vector sits at
/// packet RAM 0x800c, ours elsewhere: the hard-coded vendor address sent
/// responses with flags taken from host data.
fn metadata_source_word(if_id: u8) -> u32 {
    packet_ram::RuntimePacketAddress::new(
        packet_ram::interface_metadata_byte(usize::from(if_id)) as u32,
        1,
    )
    .map(|address| 0x2080_0000 | address.mac_offset().raw())
    .unwrap_or_else(|| crate::halt_always!())
}

/// BlockAck response template (vendor `txp_build_ba_desc`, 0x10758). Words
/// 7, 9 and 10 take the caller's third argument, which
/// `txp_build_resp_descs` passes as 0; the decompiler shows the frame length
/// (0x1c) there, and with it the MAC sent malformed BlockAcks from an unused
/// receive pipe.
fn build_ba_descriptor(if_id: u8, pointer: usize) {
    let words = [
        0x5900_0000,
        0x5800_0003,
        0x5a00_0020,
        0x3100_0094,
        0x4700_0000,
        metadata_source_word(if_id),
        0x3200_0000,
        0x6900_0000,
        0x0700_6820,
        0x6800_0000,
        0x6000_0000,
        0x0700_4600,
        0xf000_0000,
    ];
    for (index, value) in words.into_iter().enumerate() {
        write_u32(pointer + index * 4, value);
    }
}

/// Match vendor `txp_program_pipe_slot_ex(slot, 1, 0, 0)`.
///
/// The first boolean selects the response bitmap at `+0x08`; it does not
/// request that all slot bitmaps be cleared.  Keeping the programmed BA slots
/// in that bitmap is required for the MAC to recognize their response path.
fn configure_response_pipe_slot(slot: u8) {
    let bit = slot - 2;
    write_u32(
        crate::platform::mac_register(0x0a08),
        read_u32(crate::platform::mac_register(0x0a08)) | (1_u32 << bit),
    );
    write_u32(
        crate::platform::mac_register(0x0a0c),
        read_u32(crate::platform::mac_register(0x0a0c)) & !(1_u32 << bit),
    );
    write_u32(
        crate::platform::mac_register(0x0a10),
        read_u32(crate::platform::mac_register(0x0a10)) & !(1_u32 << bit) & 0x00ff_ffff,
    );
    let field_register = if slot > 0x11 {
        crate::platform::mac_register(0x0a18)
    } else {
        crate::platform::mac_register(0x0a14)
    };
    write_u32(field_register, 0);
}

fn set_pipe_enabled(slot: u8) {
    let bit = slot - 2;
    write_u32(
        crate::platform::mac_register(0x0a04),
        read_u32(crate::platform::mac_register(0x0a04)) | (1_u32 << bit),
    );
}

fn install_response_descriptors() {
    configure_response_pipe_slot(2);
    configure_response_pipe_slot(3);
    for (if_id, pointer) in [
        (0_u8, packet_ram::response_command(11)),
        (1, packet_ram::response_command(12)),
    ] {
        write_u32(
            pointer,
            low_mac().slot_time_constant.get().wrapping_shl(16) | 0x2000_0000,
        );
        write_u16(pointer + 4, 0);
        write_u16(pointer + 6, 0);
        write_u32(pointer + 8, 0x5800_0000);
        build_ba_descriptor(if_id, pointer + 0x0c);
        for slot in if if_id == 0 {
            [2_u8, 0x0b]
        } else {
            [3_u8, 0x0c]
        } {
            program_pipe_slot(pointer, slot);
        }
    }
    set_pipe_enabled(2);
    set_pipe_enabled(3);
    configure_response_pipe_slot(0x0b);
    configure_response_pipe_slot(0x0c);
    set_pipe_enabled(0x0b);
    set_pipe_enabled(0x0c);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_classes_match_vendor_groups() {
        assert_eq!(rate_phy_class(0, 0, true), 1);
        assert_eq!(rate_phy_class(0, 1, true), 0);
        assert_eq!(rate_phy_class(0, 6, true), 2);
        assert_eq!(rate_phy_class(0, 14, false), 4);
        assert_eq!(rate_phy_class(0, 14, true), 5);
    }

    #[test]
    fn duration_and_fallback_tables_are_bounded() {
        assert_eq!(TX_DURATION_TIMING[0], 0x00c2);
        assert_eq!(TX_DURATION_TIMING[9], 0x0016);
        assert_eq!(ofdm_duration(0, 14), 0);
        assert!(extended_airtime(0x17, 0, 14, true) != 0);
        let fallback = fill_fallbacks(0);
        assert_eq!(fallback[0], 0);
        assert_eq!(fallback[13], 10);
        assert_eq!(fallback[21], 10);
        assert_eq!(secondary_fallbacks(0, fallback[21]), [10; 8]);
        assert_eq!(
            secondary_fallbacks(1 << 16, 10),
            [10, 10, 16, 16, 16, 16, 16, 16]
        );
    }

    #[test]
    fn large_ampdu_airtime_does_not_overflow_u16() {
        // Above 8190 bytes, `length * 8` overflows u16; verify monotonic airtime and L-SIG length.
        let small = extended_airtime(0x17, 21, 8_000, true);
        let medium = extended_airtime(0x17, 21, 16_000, true);
        let large = extended_airtime(0x17, 21, 32_000, true);
        assert!(medium > small, "expected {medium} > {small}");
        assert!(large > medium, "expected {large} > {medium}");
        assert!(ofdm_duration(21, 16_000) > ofdm_duration(21, 8_000));
        assert!(ofdm_duration(21, 32_000) > ofdm_duration(21, 16_000));
    }
}
