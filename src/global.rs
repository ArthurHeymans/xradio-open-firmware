//! Firmware-wide state in statics.
//!
//! The firmware runs one thread, and its only interrupt handler touches
//! nothing but its own records (`interrupts`), so state in a static needs
//! no lock. `Global` says so once, instead of an
//! `unsafe impl Sync` on a wrapper type per static. It dereferences to the
//! value: wrap a `Cell` for plain values, an `UnsafeCell` where callers
//! take references and keep them from overlapping.
//!
//! Host tests run on several threads; tests that share a static serialize
//! themselves.

#[repr(transparent)]
pub struct Global<T>(T);

// SAFETY: the firmware is single threaded; the IRQ handler never touches a
// `Global`, and FIQ stays masked.
unsafe impl<T> Sync for Global<T> {}

impl<T> Global<T> {
    pub const fn new(value: T) -> Self {
        Self(value)
    }
}

impl<T> core::ops::Deref for Global<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}
