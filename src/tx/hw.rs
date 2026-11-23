//! The TX engine's only raw hardware access.
//!
//! Every address the TX engine touches is a MAC/PHY/interrupt-controller
//! register or a packet-RAM TX object (descriptor, command slot, frame
//! buffer). Packet-RAM objects are `NOBITS` statics that Rust never borrows,
//! so volatile access to them cannot alias Rust-managed memory. (The one
//! borrow, the retained host frame that `classify` and `start_encryption`
//! work on in place, is not accessed through these helpers while it lives.
//! While the AES engine encrypts it, readers see only its header, which the
//! engine does not write.) These helpers
//! are therefore safe to call; the address arithmetic is what callers must
//! get right, and that is a correctness question, not a memory-safety one.

pub(crate) fn read_u8(address: usize) -> u8 {
    // SAFETY: see the module documentation.
    unsafe { (address as *const u8).read_volatile() }
}

pub(crate) fn write_u8(address: usize, value: u8) {
    // SAFETY: see the module documentation.
    unsafe { (address as *mut u8).write_volatile(value) }
}

pub(crate) fn read_u16(address: usize) -> u16 {
    // SAFETY: see the module documentation.
    unsafe { (address as *const u16).read_volatile() }
}

pub(crate) fn read_u32(address: usize) -> u32 {
    // SAFETY: see the module documentation.
    unsafe { (address as *const u32).read_volatile() }
}

pub(crate) fn write_u16(address: usize, value: u16) {
    // SAFETY: see the module documentation.
    unsafe { (address as *mut u16).write_volatile(value) }
}

pub(crate) fn write_u32(address: usize, value: u32) {
    // SAFETY: see the module documentation.
    unsafe { (address as *mut u32).write_volatile(value) }
}

/// Copy `source` into packet RAM at `destination` with word writes. A
/// trailing partial word keeps the bytes beyond `source`.
pub(crate) fn copy_to_packet_ram(destination: u32, source: &[u8]) {
    let destination = destination as usize;
    source.chunks(4).enumerate().for_each(|(index, chunk)| {
        let address = destination + index * 4;
        let mut bytes = if chunk.len() == 4 {
            [0; 4]
        } else {
            read_u32(address).to_le_bytes()
        };
        bytes[..chunk.len()].copy_from_slice(chunk);
        write_u32(address, u32::from_le_bytes(bytes));
    });
}

/// The TX engine's access to MAC registers and packet-RAM descriptors. The
/// firmware uses [`Mmio`]; tests substitute a recorder to check the exact
/// write order.
pub trait Hw {
    fn read(&mut self, address: u32) -> u32;
    fn write(&mut self, address: u32, value: u32);
}

/// Volatile access to the real hardware.
pub(crate) struct Mmio;

impl Hw for Mmio {
    fn read(&mut self, address: u32) -> u32 {
        read_u32(address as usize)
    }

    fn write(&mut self, address: u32, value: u32) {
        write_u32(address as usize, value)
    }
}
