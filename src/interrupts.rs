//! The interrupt controller (0x0a88_0000) and the IRQ handler.
//!
//! The handler only takes note: it masks every pending source, acknowledges
//! it and records it for its owner. The owner takes the record in the main
//! loop, clears the cause in its device and re-arms the source. A level
//! source (HIF) therefore cannot re-enter the handler before its owner ran,
//! and a latched one (AES, timer) is not lost while its owner is busy.
//! Main-loop state is never touched from the handler.
//!
//! FIQ, the MAC's event line, stays masked. It still ends a wait for
//! interrupt, and the main loop drains the MAC event FIFO.

use core::cell::Cell;
use critical_section::Mutex;
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::registers::{ReadOnly, ReadWrite, WriteOnly};
use tock_registers::{register_bitfields, register_structs};

register_bitfields! {
    u32,
    /// One bit per source in `ACKNOWLEDGE`, `ENABLE`, `PENDING` and
    /// `RAW_STATE`. Names of the form `IRQn` mark sources whose owner is not
    /// known. Unlisted sources were never seen and are never enabled.
    InterruptSources [
        /// Debug UART transmit-empty. Level, and pending whenever nothing is
        /// being sent: an image that enables it and prints nothing never
        /// sleeps. Not enabled; the UART is not wired out.
        UART_TX OFFSET(0) NUMBITS(1) [],
        /// Debug UART receive. Not enabled.
        UART_RX OFFSET(2) NUMBITS(1) [],
        /// Owner unknown. Not enabled.
        IRQ4 OFFSET(4) NUMBITS(1) [],
        /// Timer 1 one-shot expiry. Acknowledged after every sleep.
        TIMER1 OFFSET(6) NUMBITS(1) [],
        /// Owner unknown; pending as soon as it is enabled. Not enabled.
        IRQ9 OFFSET(9) NUMBITS(1) [],
        /// Host interface, level. The causes are in HIF `STATUS`; the source
        /// is re-armed only after every cause is acknowledged.
        HIF OFFSET(13) NUMBITS(1) [],
        /// AES engine completion. Which direction uses which of the two lines
        /// is not established.
        AES_18 OFFSET(18) NUMBITS(1) [],
        /// AES engine completion, second line.
        AES_20 OFFSET(20) NUMBITS(1) [],
        /// Owner unknown. Not enabled.
        IRQ21 OFFSET(21) NUMBITS(1) [],
        /// The MAC event line, routed to FIQ by `CONFIGURATION`. It ends a
        /// wait for interrupt but never shows in `PENDING` or `RAW_STATE`,
        /// and enabling it as an IRQ changes nothing.
        MAC_FIQ OFFSET(22) NUMBITS(1) [],
        /// Owner unknown; pending as soon as it is enabled. Not enabled.
        IRQ24 OFFSET(24) NUMBITS(1) [],
        /// TSF adjustment (inferred). Enabled; its handler writes back
        /// `TSF_CONTROL` masked with `0x1f00_0003`. Never seen pending,
        /// because the firmware never adjusts the TSF.
        TSF OFFSET(26) NUMBITS(1) [],
        /// TX error events. Not enabled.
        TX_ERROR OFFSET(27) NUMBITS(1) [],
        /// Set while the MAC event FIFO holds events (inferred): a level view
        /// of the same events the FIQ line signals. Acknowledging it stopped
        /// reception, so it is never enabled; the main loop drains the FIFO.
        MAC_EVENTS OFFSET(28) NUMBITS(1) [],
    ],
    /// The firmware writes `0x1600_a037`; the low 24 bits are not
    /// described.
    InterruptConfiguration [
        /// Source routed to the FIQ output.
        FIQ_SOURCE OFFSET(24) NUMBITS(8) [
            /// The MAC event line.
            MAC = 0x16
        ],
    ],
}

register_structs! {
    InterruptController {
        /// Controller enable. Written 1 at reset.
        (0x00 => control: WriteOnly<u32>),
        /// Write one bits to acknowledge pending sources.
        (0x04 => acknowledge: WriteOnly<u32, InterruptSources::Register>),
        /// Written `BASE << 9`: an encoded word, not a CPU address.
        (0x08 => vector_base: WriteOnly<u32>),
        /// Bit n lets source n reach the CPU.
        (0x0c => enable: ReadWrite<u32, InterruptSources::Register>),
        (0x10 => _reserved0),
        /// Written all-ones at reset, together with an all-ones acknowledge.
        /// Function unknown.
        (0x14 => clear_14: WriteOnly<u32>),
        /// The top byte selects the FIQ source: 22 (`0x16`), the MAC.
        (0x18 => configuration: WriteOnly<u32, InterruptConfiguration::Register>),
        (0x1c => _reserved1),
        /// Pending sources among the enabled ones.
        (0x20 => pending: ReadOnly<u32, InterruptSources::Register>),
        /// Raw state of the sources, before the enable mask: the only way to
        /// see a source that is pending but disabled. Source 22, routed to
        /// FIQ, does not appear. The firmware does not read it.
        (0x24 => raw_state: ReadOnly<u32, InterruptSources::Register>),
        (0x28 => @END),
    }
}

const BASE: usize = 0x0a88_0000;

fn controller() -> &'static InterruptController {
    // SAFETY: the controller's registers live at BASE for the firmware's
    // lifetime; every access goes through volatile register reads/writes.
    unsafe { &*(BASE as *const InterruptController) }
}

/// Sources the handler masked and recorded, not yet taken by their owner.
static RAISED: Mutex<Cell<u32>> = Mutex::new(Cell::new(0));

/// The vendor's setup (`tsf_timer_reload`, `0x56fc`): every source
/// disabled and acknowledged, source 0x16 as FIQ.
pub(crate) fn reset() {
    let controller = controller();
    controller.configuration.set(0x1600_a037);
    controller.clear_14.set(u32::MAX);
    controller.acknowledge.set(u32::MAX);
    controller.vector_base.set((BASE as u32) << 9);
    controller.enable.set(0);
    controller.control.set(1);
}

/// Lets `irq` interrupt the CPU.
pub(crate) fn enable(irq: u32) {
    rearm(1 << irq);
}

/// Re-enables sources whose owner has taken their record and cleared the
/// cause in its device.
pub(crate) fn rearm(sources: u32) {
    critical_section::with(|_| {
        let controller = controller();
        controller.enable.set(controller.enable.get() | sources);
    });
}

/// Takes the records of `sources` the handler made since the last take,
/// and those pending now that it has not seen yet (it never runs while IRQ
/// is masked, as during startup). Recorded sources stay masked until
/// [`rearm`]ed.
pub(crate) fn take(sources: u32) -> u32 {
    critical_section::with(|cs| {
        let controller = controller();
        let pending = controller.pending.get() & sources;
        controller.acknowledge.set(pending);
        let raised = RAISED.borrow(cs);
        let taken = raised.get() & sources;
        raised.set(raised.get() & !taken);
        taken | pending
    })
}

/// Records are waiting for their owners.
pub(crate) fn raised() -> bool {
    critical_section::with(|cs| RAISED.borrow(cs).get() != 0)
}

/// Acknowledges sources the handler never saw. Only for sleep.
pub(crate) fn acknowledge(sources: u32) {
    controller().acknowledge.set(sources);
}

/// Called in IRQ mode, on the IRQ stack, with IRQ and FIQ masked
/// (`_asm_irq_handler` in the startup binary).
#[cfg(target_arch = "arm")]
#[unsafe(no_mangle)]
extern "C" fn _irq_handler() {
    let controller = controller();
    let pending = controller.pending.get();
    controller.enable.set(controller.enable.get() & !pending);
    controller.acknowledge.set(pending);
    critical_section::with(|cs| {
        let raised = RAISED.borrow(cs);
        raised.set(raised.get() | pending);
    });
}
