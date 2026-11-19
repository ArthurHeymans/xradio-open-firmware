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
use tock_registers::register_structs;
use tock_registers::registers::{ReadOnly, ReadWrite};

register_structs! {
    InterruptController {
        (0x00 => control: ReadWrite<u32>),
        /// Write one bits to acknowledge pending sources.
        (0x04 => acknowledge: ReadWrite<u32>),
        (0x08 => vector_base: ReadWrite<u32>),
        (0x0c => enable: ReadWrite<u32>),
        (0x10 => _reserved0),
        (0x14 => clear_14: ReadWrite<u32>),
        /// The top byte selects the FIQ source (0x16, the MAC).
        (0x18 => configuration: ReadWrite<u32>),
        (0x1c => _reserved1),
        /// Pending sources among the enabled ones.
        (0x20 => pending: ReadOnly<u32>),
        (0x24 => @END),
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
