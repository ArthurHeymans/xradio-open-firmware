//! ARM966E-S operations `aarch32-cpu` has no ARMv5 form of: on ARMv5 the
//! wait for interrupt and the write-buffer drain are CP15 operations, not
//! instructions. Both are emitted as A32 code from this Thumb image.

/// Waits until an interrupt source is pending. IRQ and FIQ may stay masked
/// in the CPSR: a pending source ends the wait either way.
#[cfg(target_arch = "arm")]
#[instruction_set(arm::a32)]
#[inline(never)]
pub fn wait_for_interrupt() {
    // SAFETY: stops the core clock until a source is pending; touches no
    // memory.
    unsafe {
        core::arch::asm!("mcr p15, 0, {zero}, c7, c0, 4", zero = in(reg) 0_u32, options(nostack, preserves_flags));
    }
}

/// Waits until every buffered write has reached the bus, so a device sees
/// it before the firmware's next step.
#[cfg(target_arch = "arm")]
#[instruction_set(arm::a32)]
#[inline(never)]
pub fn drain_write_buffer() {
    // SAFETY: only orders memory accesses.
    unsafe {
        core::arch::asm!("mcr p15, 0, {zero}, c7, c10, 4", zero = in(reg) 0_u32, options(nostack, preserves_flags));
    }
}

#[cfg(not(target_arch = "arm"))]
pub fn drain_write_buffer() {
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

/// Masks IRQ; returns whether it was unmasked.
#[cfg(target_arch = "arm")]
#[instruction_set(arm::a32)]
#[inline(never)]
fn mask_irq() -> bool {
    let previous: u32;
    // SAFETY: only sets the CPSR's I bit.
    unsafe {
        core::arch::asm!(
            "mrs {previous}, cpsr",
            "orr {scratch}, {previous}, #0x80",
            "msr cpsr_c, {scratch}",
            previous = out(reg) previous,
            scratch = out(reg) _,
            options(nostack, preserves_flags),
        );
    }
    previous & 0x80 == 0
}

#[cfg(target_arch = "arm")]
#[instruction_set(arm::a32)]
#[inline(never)]
fn unmask_irq() {
    // SAFETY: only clears the CPSR's I bit.
    unsafe {
        core::arch::asm!(
            "mrs {scratch}, cpsr",
            "bic {scratch}, {scratch}, #0x80",
            "msr cpsr_c, {scratch}",
            scratch = out(reg) _,
            options(nostack, preserves_flags),
        );
    }
}

/// Critical sections mask IRQ (FIQ stays masked anyway). One ARM-state
/// call to enter and at most one to leave: aarch32-cpu's single-core
/// implementation takes three.
#[cfg(target_arch = "arm")]
struct SingleCore;

#[cfg(target_arch = "arm")]
critical_section::set_impl!(SingleCore);

#[cfg(target_arch = "arm")]
// SAFETY: one core, always privileged; masking IRQ excludes the only
// other context, the IRQ handler.
unsafe impl critical_section::Impl for SingleCore {
    unsafe fn acquire() -> critical_section::RawRestoreState {
        let unmasked = mask_irq();
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        unmasked
    }

    unsafe fn release(unmasked: critical_section::RawRestoreState) {
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        if unmasked {
            unmask_irq();
        }
    }
}
