//! XR819 platform setup performed before the vendor HIF initializer.
//!
//! This is an intentionally literal translation of the startup routines at
//! 0x000009e0, 0x000056d8, 0x0000572a, 0x000058b6, and 0x00000bb0. Interrupt
//! callback registration is kept separate because the Rust handlers do not
//! exist yet.

use crate::global::Global;
use crate::packet_ram;
use core::cell::Cell;
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::{register_bitfields, register_structs};
use tock_registers::registers::{ReadOnly, ReadWrite};

register_bitfields! {
    u32,
    pub(crate) RxDmaControl [
        /// Packet DMA writes received frames into the RX FIFO.
        ENABLE OFFSET(0) NUMBITS(1) [],
        /// Still moving a frame; clears after ENABLE is dropped.
        ACTIVE OFFSET(23) NUMBITS(1) [],
    ],
}

register_structs! {
    /// The MAC's RX packet DMA (MAC block 0x0600).
    pub(crate) RxDma {
        (0x00 => pub(crate) control: ReadWrite<u32, RxDmaControl::Register>),
        /// FIFO offset just past the newest frame written.
        (0x04 => pub(crate) producer: ReadWrite<u32>),
        /// FIFO offset up to which the firmware has released frames.
        (0x08 => pub(crate) consumer: ReadWrite<u32>),
        (0x0c => configuration_0c: ReadWrite<u32>),
        (0x10 => _reserved0),
        (0x1c => configuration_1c: ReadWrite<u32>),
        (0x20 => configuration_20: ReadWrite<u32>),
        (0x24 => @END),
    }
}

pub(crate) fn rx_dma() -> &'static RxDma {
    unsafe { &*(mac_register(0x0600) as *const RxDma) }
}

register_structs! {
    ClockReset {
        (0x00 => _reserved0),
        (0x04 => control_04: ReadWrite<u32>),
        (0x08 => window_selector: ReadWrite<u32>),
        (0x0c => _reserved1),
        (0x18 => feature_control: ReadWrite<u32>),
        (0x1c => _reserved2),
        (0x2c => window_value: ReadWrite<u32>),
        (0x30 => _reserved3),
        (0x40 => mask_40: ReadWrite<u32>),
        (0x44 => mask_44: ReadWrite<u32>),
        (0x48 => mask_48: ReadWrite<u32>),
        (0x4c => @END),
    },

    SystemControl {
        (0x00 => control: ReadWrite<u32>),
        (0x04 => status: ReadWrite<u32>),
        (0x08 => secondary_control: ReadWrite<u32>),
        (0x0c => @END),
    },

    /// Two timers on a 80 MHz clock divided down to 1 MHz (`prescale`
    /// 0x4f). Timer 0 runs free and is the firmware's clock; timer 1 counts
    /// down `load` microseconds once and then raises IRQ 6.
    Timers {
        (0x00 => timer0_load: ReadWrite<u32>),
        /// Microseconds, counting up.
        (0x04 => now: ReadOnly<u32>),
        (0x08 => timer0_control: ReadWrite<u32>),
        (0x0c => _reserved0),
        (0x10 => timer0_prescale: ReadWrite<u32>),
        (0x14 => timer1_load: ReadWrite<u32>),
        (0x18 => _reserved1),
        /// 0xc1 starts a one-shot count; bit 1 reads set once it fired.
        /// Writing 0 stops it, and must come first to start it again.
        (0x1c => timer1_control: ReadWrite<u32>),
        (0x20 => timer1_mode: ReadWrite<u32>),
        (0x24 => timer1_prescale: ReadWrite<u32>),
        (0x28 => @END),
    },

    PlatformControl {
        (0x00 => _reserved0),
        (0x58 => shared_pointer: ReadWrite<u32>),
        (0x5c => _reserved1),
        (0x64 => power_control: ReadWrite<u32>),
        (0x68 => _reserved2),
        (0x80 => ready_status: ReadWrite<u32>),
        (0x84 => clock_flags: ReadWrite<u32>),
        (0x88 => _reserved3),
        (0x94 => clock_status: ReadWrite<u32>),
        (0x98 => _reserved4),
        (0x9c => parameter_9c: ReadWrite<u32>),
        (0xa0 => parameter_a0: ReadWrite<u32>),
        (0xa4 => _reserved5),
        (0xbc => configuration: ReadWrite<u32>),
        (0xc0 => @END),
    },

    InterruptRouting {
        (0x00 => _reserved0),
        (0x10 => route_10: ReadWrite<u32>),
        (0x14 => route_14: ReadWrite<u32>),
        (0x18 => route_18: ReadWrite<u32>),
        (0x1c => route_1c: ReadWrite<u32>),
        (0x20 => route_20: ReadWrite<u32>),
        (0x24 => route_24: ReadWrite<u32>),
        (0x28 => route_28: ReadWrite<u32>),
        (0x2c => route_2c: ReadWrite<u32>),
        (0x30 => route_30: ReadWrite<u32>),
        (0x34 => @END),
    }
}

const CLOCK_RESET_BASE: usize = 0x0aa8_0000;
const SYSTEM_CONTROL_BASE: usize = 0x0a98_0000;
const TIMERS_BASE: usize = 0x0ac0_0000;
const PLATFORM_CONTROL_BASE: usize = 0x0ac8_0000;
const HIF_SHARED_BASE: usize = 0x0ab0_0100;

const MAC_REGISTER_BASE: usize = 0x09c0_0000;
const AES_REGISTER_BASE: usize = 0x09c5_0000;
const TX_RING_REGISTER_BASE: usize = 0x09c6_0000;
const TX_RING_STRIDE: usize = 0x80;

/// Address one register in the sparse MAC/packet-DMA register aperture.
pub const fn mac_register(offset: usize) -> usize {
    MAC_REGISTER_BASE + offset
}

/// Base of the decoded AES register block.
pub const fn aes_register_base() -> usize {
    AES_REGISTER_BASE
}

/// Address a register in one of the four hardware TX rings.
pub const fn tx_ring_register(pipe: usize, offset: usize) -> usize {
    debug_assert!(pipe < 4);
    TX_RING_REGISTER_BASE + pipe * TX_RING_STRIDE + offset
}

/// Address a register by its offset in the complete hardware TX-ring aperture.
pub const fn tx_ring_register_offset(offset: usize) -> usize {
    TX_RING_REGISTER_BASE + offset
}

#[unsafe(no_mangle)]
#[used]
pub static REMAP_DEBUG_INDEX: u32 = 0;

fn clock_reset() -> &'static ClockReset {
    unsafe { &*(CLOCK_RESET_BASE as *const ClockReset) }
}

fn system_control() -> &'static SystemControl {
    unsafe { &*(SYSTEM_CONTROL_BASE as *const SystemControl) }
}


fn timers() -> &'static Timers {
    unsafe { &*(TIMERS_BASE as *const Timers) }
}

fn platform_control() -> &'static PlatformControl {
    unsafe { &*(PLATFORM_CONTROL_BASE as *const PlatformControl) }
}

/// Startup facts sampled by `initialize_runtime_state` and
/// `prepare_memory_and_interrupts`. The vendor keeps them as words in its
/// scheduler island; the ones nothing reads (0x08..0x18) are not kept.
struct BootState {
    /// Vendor boot halfword `+0x12`: 1 from the reset word `0x0001_9600`,
    /// then 1 or 3 once memory setup has probed the system status.
    startup_mode: Cell<u16>,
    remap_windows_present: Cell<bool>,
    remap_windows: [Cell<u32>; 8],
}

static BOOT_STATE: Global<BootState> = Global::new(BootState {
    startup_mode: Cell::new(1),
    remap_windows_present: Cell::new(false),
    remap_windows: [const { Cell::new(0) }; 8],
});

fn boot() -> &'static BootState {
    &BOOT_STATE
}

fn register32(address: usize) -> &'static ReadWrite<u32> {
    unsafe { &*(address as *const ReadWrite<u32>) }
}

fn post_code(_value: u32) {}

/// Exact vendor IRQ 26 callback at `0x00007e3c`, run from the polled service
/// loop.
fn packet_dma_irq26() {
    let status = register32(0x09c0_0e34);
    status.set(status.get() & 0x1f00_0003);
}

/// Startup state the platform and HIF routines expect after `.bss` has been
/// zeroed.
pub fn initialize_runtime_state() {
    let boot = boot();
    boot.startup_mode.set(1);
    boot.remap_windows_present.set(false);
}

/// Free-running microsecond timer. The vendor adds a DTCM offset that is
/// always zero.
pub fn timer_now() -> u32 {
    timers().now.get()
}

/// Timer 1's interrupt source.
const TIMER1_IRQ: u32 = 6;

/// Sleeps until an interrupt source is pending, at most `limit_us`, unless
/// the IRQ handler already recorded one.
///
/// IRQ is masked across the check and the wait, as the vendor's scheduler
/// does: a source that becomes pending after the check still ends the wait,
/// and the handler records it once IRQ is unmasked again. Timer 1 wakes
/// the CPU without the handler ever seeing it.
#[cfg(target_arch = "arm")]
pub fn wait_for_event(limit_us: u32) {
    critical_section::with(|_| {
        if crate::interrupts::raised() {
            return;
        }
        let timers = timers();
        timers.timer1_control.set(0);
        timers.timer1_load.set(limit_us);
        timers.timer1_control.set(0xc1);
        crate::cpu::wait_for_interrupt();
        timers.timer1_control.set(0);
        crate::interrupts::acknowledge(1 << TIMER1_IRQ);
    });
}

/// One of the eight hardware remap windows sampled at startup. Windows 3..5
/// are stored relocated, as the vendor does.
pub fn remap_window(index: usize) -> u32 {
    assert!(index < 8);
    boot().remap_windows[index].get()
}

/// At least one remap window was nonzero at startup.
pub fn remap_windows_present() -> bool {
    boot().remap_windows_present.get()
}

/// Startup mode chosen by `prepare_memory_and_interrupts`: 1 or 3.
pub fn startup_mode() -> u16 {
    boot().startup_mode.get()
}

/// Samples and records the eight hardware remap windows (`FUN_00015944`).
fn sample_remap_windows() {
    let clock = clock_reset();
    let selector = clock.window_selector.get() & !0x1c;
    let mut any = 0;

    for index in 0..8 {
        clock.window_selector.set(selector | ((index as u32) << 2));
        let value = clock.window_value.get();
        any |= value;
        boot().remap_windows[index].set(value);
    }

    boot().remap_windows_present.set(any != 0);
    if any == 0 {
        return;
    }

    let windows = &boot().remap_windows;
    let window_3 = windows[3].get();
    let window_4 = windows[4].get();
    let window_5 = windows[5].get();
    let relocated_3 = ((window_3 & 0x0fff_ffff) >> 12).wrapping_add(0x166);
    let relocated_4 = ((window_4 & 0x0fff_ffff) >> 12).wrapping_add(0x166);
    let relocated_high = (window_4 >> 28 | window_5 << 4).wrapping_add(0x166);

    windows[3].set((window_3 & 0xf000_0fff) | (relocated_3 << 12));
    windows[4]
        .set((window_4 & 0x0000_0fff) | (relocated_4 << 12) | (relocated_high << 28));
    windows[5].set((window_5 & 0xffff_f000) | ((relocated_high & 0xffff) >> 4));
}

/// Applies the control update at `firmware_main_initialize + 0x0c` before
/// `FUN_000009e0`: clear bit 0 and set bit 23 at `0x0ac800bc`.
pub fn prepare_main_control() {
    let platform = platform_control();
    platform
        .configuration
        .set((platform.configuration.get() & !1) | 0x0080_0000);
}

/// Translates `FUN_000009e0`, including interrupt-controller initialization.
///
/// This leaves CPU IRQ/FIQ masked and does not install any firmware handlers.
pub fn prepare_memory_and_interrupts() {
    sample_remap_windows();

    let clock = clock_reset();
    let system = system_control();
    let remap_control = if remap_windows_present() { remap_window(0) } else { 0 };

    if remap_control & 0x0800_0000 == 0 {
        boot().startup_mode.set(3);
        let saved_features = clock.feature_control.get();
        clock.feature_control.set(saved_features | 0x0018_0200);
        system.control.set((system.control.get() & !0x20) | 0x40);
        system
            .secondary_control
            .set(system.secondary_control.get() | 0x40);

        if system.status.get() & 0x20 != 0 {
            system
                .secondary_control
                .set(system.secondary_control.get() & !0x40);
            if system.status.get() & 0x20 == 0 {
                boot().startup_mode.set(1);
            }
        }
        clock.feature_control.set(saved_features);
    }

    let timers = timers();
    timers.timer0_load.set(u32::MAX);
    timers.timer0_prescale.set(0x4f);
    timers.timer0_control.set(0x80);
    timers.timer1_prescale.set(0x4f);
    timers.timer1_mode.set(0);

    crate::interrupts::reset();

    clock.mask_44.set(clock.mask_44.get() & 0x7fff_f777);
    clock.mask_48.set(clock.mask_48.get() & 0x777f_ffff);
}

/// Reproduces the hardware-visible part of `0x00000a74 -> 0xfff019aa`.
/// The vendor performs this between platform preparation and DMA/clock setup.
///
/// `0x0a90_0000` is the vendor's debug console UART, which the board does
/// not wire out. The vendor also takes its IRQs: 0, "transmit holding
/// register empty", which is pending for as long as nothing is sent, and
/// 2, receive. Its print routine enables IRQ 0 only while it has output.
/// This firmware prints nothing and leaves both disabled: a source that is
/// always pending would end every wait for interrupt at once.
pub fn prepare_high_platform_support() {
    // 0x00000a74 followed by the hardware effects of 0xfff019aa.
    register32(0xfff0_32ec).set(1);
    register32(0x0a90_0004).set(0x2b);
}

/// Translates the hardware portion of `FUN_00000bb0` and `FUN_000058b6`.
pub fn prepare_dma_and_clocks() {
    post_code(0x444d_4300);
    let system = system_control();
    let clock = clock_reset();
    let platform = platform_control();

    system.control.set((system.control.get() & !0x80) | 0x10);
    system
        .secondary_control
        .set(system.secondary_control.get() & !0x10);
    post_code(0x444d_4301);
    clock.mask_48.set(clock.mask_48.get() | 0xf0);
    clock.mask_40.set(clock.mask_40.get() | 0x00f0_0000);
    post_code(0x444d_4302);

    platform.shared_pointer.set(0x0004_0090);
    platform.power_control.set(0x10);
    if !remap_windows_present() {
        platform
            .configuration
            .set(platform.configuration.get() | 0x0012_0000);
    }
    post_code(0x444d_4303);

    // Preserve the exact 0x00000bb0 transition. Loss of the 0x0900ff98 alias
    // after this write is not evidence that the CPU stopped executing.
    post_code(0x444d_4331);
    clock.control_04.set(0x200);
    post_code(0x444d_4332);
    platform.parameter_a0.set(31);
    post_code(0x444d_4333);
    platform.parameter_9c.set(6);
    // The vendor also registers a handler for source 4 here. This firmware
    // has none, so the source stays disabled.
    post_code(0x444d_4304);

    let mut attempts = 0;
    while platform.ready_status.get() & 1 == 0 && attempts < 0x200 {
        attempts += 1;
    }
    post_code(0x444d_4305);

    post_code(0x444d_4306);

    // The vendor also records the chosen clock parameters in DTCM; nothing
    // reads them back.
    let alternate = platform.clock_status.get() & 8 == 0;
    platform.clock_flags.set(if alternate { 0x0c02 } else { 0x0042 });
    post_code(0x444d_4307);
}

/// Translates the fixed packet-memory/DMA register setup reached through
/// `FUN_000000f6`, `FUN_000000bc`, `FUN_0000f608`, `FUN_0000f4f4`, and
/// `FUN_0000fcfe` before the vendor startup indication is queued.
/// Vendor `0x00000c38`, `0x00000a88`, and `0x00000b88`, called in this
/// order after HIF activation and before `0x000009ac`.
pub fn register_post_activation_interrupts() {
    // Timer 1 and the AES engine. The vendor also registers source 21,
    // which this firmware has no handler for.
    for irq in [6, 18, 20] {
        crate::interrupts::enable(irq);
    }
}

/// Register translated sources from the tail of vendor `fw_subsystem_init`
/// after packet-DMA and MAC/RX setup is complete.
pub fn register_packet_dma_interrupts() {
    crate::interrupts::enable(26);
    // Vendor IRQ 27 calls `event_send_error_0x34`; it stays disabled until
    // that path is translated.
}

/// Services vendor IRQ 26 while this firmware intentionally keeps CPU IRQ/FIQ
/// masked. This reproduces the generic demultiplexer's acknowledge-before-
/// callback order instead of merely registering a callback that cannot run.
pub fn service_masked_packet_dma_interrupt() -> bool {
    const IRQ26: u32 = 1 << 26;
    if crate::interrupts::take(IRQ26) == 0 {
        return false;
    }
    packet_dma_irq26();
    crate::interrupts::rearm(IRQ26);
    true
}

pub fn prepare_mac_receive_hardware() {
    let control = 0x09c0_0040;
    for (offset, value) in [
        (0x000, 0x8000_ffd4),
        (0x004, 0x8000_ffc4),
        (0x008, 0x8000_ffb4),
        (0x00c, 0x8000_efe4),
        (0x010, 0x8000_ff84),
        (0x014, 0x8000_ff94),
        (0x018, 0x0000_ff80),
        (0x01c, 0x0000_8f88),
        (0x020, 0x0000_2020),
        (0x024, 0x0000_4040),
        (0x028, 0x0000_4000),
        (0x02c, 0x0004_0101),
    ] {
        register32(0x09c0_0000 + offset).set(value);
    }
    for (offset, value) in [
        (0x060, 0x0010_0101),
        (0x064, 0x8018_6000),
        (0x068, 0x0002_ff00),
        (0x06c, 0x0003_ff00),
        (0x070, 0x0000_ff74),
        (0x074, 0x0084_0604),
        (0x078, 0x0001_8080),
        (0x07c, 0x801a_0202),
        (0x080, 0x801d_0100),
        (0x084, 0x000c_0202),
        (0x088, 0x000f_0100),
        (0x08c, 0x0018_0202),
        (0x090, 0x8600_e3e2),
        (0x094, 0x860a_3f1d),
        (0x0b4, 0x001b_0100),
        (0x0b8, 0x0000_0f00),
        (0x0bc, 0x0084_2624),
        (0x0c0, 0x8000_0000),
        (0x0c4, 0x0002_0000),
        (0x0c8, 0x0016_0000),
        (0x0cc, 0xc018_0000),
        (0x0d0, 0xc010_0000),
        (0x0d4, 0xc012_0000),
        (0x0e0, 0x0004_0000),
        (0x0e8, 0x800a_0000),
        (0x0f0, 0x0010_0000),
    ] {
        register32(control + offset).set(value);
    }

    let timing = 0x09c0_0200;
    for (offset, value) in [
        (0x010, 0x1900_0000),
        (0x014, 0x1c00_0000),
        (0x018, 0x1000_0000),
        (0x01c, 0x4000_0000),
        (0x020, 0x8000_0000),
        (0x028, 0x2000_0002),
        (0x02c, 0x0100_0004),
        // 0x230 and 0x260 look like DTCM addresses, but they are timing
        // values: any other value slows association (13-41 s instead of 2 s).
        (0x030, 0x0400_0004),
        (0x034, 0x0180_0010),
        (0x038, 0x0480_0010),
        (0x03c, 0x2000_1010),
        (0x040, 0x2000_0020),
        (0x044, 0x2000_1020),
        (0x048, 0x0200_0040),
        (0x04c, 0x0000_0800),
        (0x050, 0x2000_2080),
        (0x054, 0x2000_0001),
        (0x05c, 0x0100_0000),
        (0x060, 0x0400_0000),
        (0x064, 0x0000_c380),
        (0x068, 8),
        (0x06c, 0),
        (0x074, 0x2080_0010),
        (0x078, 0x2082_0080),
        (0x07c, 0x0000_2000),
        (0x098, 0),
        (0x0c0, 0),
        (0x0e4, 0),
        (0x0e8, 0x80),
        (0x0f0, 0x0002_0004),
        (0x0f4, 0x001e_0080),
        (0x0f8, 0x0006_0000),
        (0x0fc, 0x0067_0000),
    ] {
        register32(timing + offset).set(value);
    }
    register32(0x09c0_0a14).set(0);
    register32(0x09c0_0a18).set(0x8000);
    register32(0x09c0_0308).set(0x70);
    register32(0x09c0_02ec).set(4);
}

pub fn program_station_address(address: [u8; 6]) {
    for (index, value) in address.into_iter().enumerate() {
        let low_mac = crate::mac::low_mac();
        low_mac.own_mac[0][index].set(value);
        low_mac.own_mac[1][index].set(value);
        crate::vif::set_all_own_mac_byte(index, value);
    }
    let low = u32::from_le_bytes([address[0], address[1], address[2], address[3]]);
    let high = u32::from(u16::from_le_bytes([address[4], address[5]]));
    register32(0x09c0_0030).set(low);
    register32(0x09c0_0034).set(high);
    register32(0x09c0_0038).set(0x101);
    register32(0x09c0_0048).set(low);
    register32(0x09c0_004c).set(high);
    register32(0x09c0_0050).set(0x101);
}

pub fn prepare_packet_dma() {
    register32(0x0ab9_8040).set(0);
    register32(0x09c0_080c).set(0);

    // Vendor order, from `mac_hw_reset_regs` (`annotated-main.c:469-477`):
    // the auxiliary configuration at +0x0c goes in BEFORE the main control word
    // at +0x00, and the producer/consumer cursors are cleared only afterwards.
    // Our previous order wrote the control word first, exposing the controller
    // to stale +0x0c and cursor values across that transition. Final register
    // image is identical either way, so this only matters if +0x00 latches
    // internal routing state -- which is exactly what we are testing, since
    // vendor never leaks TX command-fetch bytes into the RX FIFO and we do.
    let dma = rx_dma();
    dma.configuration_0c.set(0x80);
    dma.control.set(0x0102_0418);
    dma.configuration_1c.set(7);
    dma.producer.set(0);
    dma.consumer.set(0);
    dma.configuration_20.set(0x3000_0000);

    register32(0x09c0_0200).set(0);
    register32(0x09c0_0204).set(0);
    register32(0x09c0_020c).set(3);
    register32(0x09c1_0000).set(0);
    register32(0x09c0_0a00).set(0x1030_0000);
    register32(0x09c0_0a04).set(0);
    register32(0x09c0_0a1c).set(0);
    post_code(0x5044_4d01);

    register32(0x09c0_0e00).set(0x4f00_004f);
    register32(0x09c0_0e04).set(0x0909_0000);
    for offset in (0x08..=0x2c).step_by(4) {
        register32(0x09c0_0e00 + offset).set(0);
    }
    // Vendor `tsf_hw_init` (`annotated-main.c:18643-18659`) clears +0xe34 to 0
    // and only then writes 3, producing a 0 -> 3 edge. We wrote 3 directly.
    // Not equivalent if the register has sticky or write-one-to-set semantics.
    register32(0x09c0_0e34).set(0);
    register32(0x09c0_0e38).set(0);
    register32(0x09c0_0e3c).set(0);
    register32(0x09c0_0e34).set(3);
    post_code(0x5044_4d02);

    register32(0x09c0_0e60).set(2);
    register32(0x09c0_0e80).set(0x100);
    register32(0x09c0_0e88).set(0xff);
    post_code(0x5044_4d21);

    for (first_pipe, second_pipe) in [(0, 1), (2, 3)] {
        register32(tx_ring_register(first_pipe, 0x0c)).set(unsafe {
            packet_ram::mac_packet_offset_unchecked(packet_ram::tx_command(first_pipe, 0))
        });
        register32(tx_ring_register(first_pipe, 0x10)).set(packet_ram::TX_COMMAND_SIZE as u32);
        register32(tx_ring_register(first_pipe, 0x14)).set(1);
        register32(tx_ring_register(second_pipe, 0x0c)).set(unsafe {
            packet_ram::mac_packet_offset_unchecked(packet_ram::tx_command(second_pipe, 0))
        });
        register32(tx_ring_register(second_pipe, 0x10)).set(packet_ram::TX_COMMAND_SIZE as u32);
        register32(tx_ring_register(second_pipe, 0x14)).set(1);
    }
    post_code(0x5044_4d22);

    // Tail of FUN_0000f4f4. The earlier failure here was caused by keeping the
    // Rust stack in the 0xfffxxxxx window across the 0x0aa80004 transition;
    // with the vendor low SRAM stack, this packet-memory window is accessible.
    // Vendor `mac_hw_init_pipes` calls `phy_set_band_reg(1)` before it
    // publishes the automatic-response command list. Keep that ordering: the
    // final register/RAM image is identical, but +0xe8c may latch response-list
    // interpretation across the transition.
    register32(0x09c0_0e8c).set(0xbf);
    post_code(0x5044_4d03);
    let list_base = packet_ram::automatic_response_list().start;
    register32(list_base).set(0x4e14_0000);
    for index in 0..33 {
        register32(list_base + 4 + index * 4).set(
            0x2200_0000 | unsafe { packet_ram::mac_packet_offset_unchecked(list_base) },
        );
    }
    register32(list_base + 4 + 33 * 4).set(0xf000_0000);
    post_code(0x5044_4d23);

    // Hardware-visible head of FUN_00000044, called here by FUN_000000f6.
    register32(0x09c0_1300).set(0);
    register32(0x09c0_1300).set(0x0100_0000);
    for index in 0..4 {
        let slot = 0x09c0_0060 + index * 0x0c;
        register32(slot).set(u32::MAX);
        register32(slot + 4).set(0xff);
        register32(slot + 8).set(0xff);
    }
    post_code(0x5044_4d04);

    // Vendor `hif_dbg_ctx_init` (`annotated-main.c:19298-19313`) writes the
    // base/control register at +0x00 LAST, after its parameters. We wrote it
    // first, which transiently publishes whatever +0x04..+0x10 held.
    register32(0x09c0_1404).set(0);
    register32(0x09c0_1408).set(0);
    register32(0x09c0_140c).set(0x7ff);
    register32(0x09c0_1410).set(0);
    register32(0x09c0_1400).set(0);
    post_code(0x5044_4d05);

    post_code(0x5044_4d06);
}

/// Vendor `0x9ac -> 0xc80 -> 0x1a2b0`, immediately before startup
/// indication construction.
pub fn enable_packet_controller() {
    register32(0x09c0_1000).set(1);
}

/// Performs the side-effect read and ready check from `FUN_00000ab4`.
///
/// This must run after [`prepare_dma_and_clocks`]. A bounded form is used for
/// bring-up so a bad prerequisite cannot trap the experiment indefinitely.
pub fn try_activate_hif(max_polls: u32) -> bool {
    unsafe {
        let _ = (HIF_SHARED_BASE.wrapping_add(0x40) as *const u32).read_volatile();
        for _ in 0..max_polls {
            if (HIF_SHARED_BASE.wrapping_add(0x34) as *const u32).read_volatile() & (1 << 10) == 0 {
                return true;
            }
            core::hint::spin_loop();
        }
    }
    false
}
