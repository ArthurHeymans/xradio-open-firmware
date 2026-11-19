//! Register blocks known only as vendor address/value sequences.
//!
//! Where the firmware knows a block's layout it uses tock-registers. Much of
//! the PHY and RF setup, though, is vendor writes of values whose meaning is
//! unknown to addresses whose layout is unknown. A [`RegisterWindow`] keeps
//! those as the vendor's absolute addresses and checks each access instead:
//! inside the window, aligned to its width.

/// A range of device registers accessed by absolute address.
#[derive(Clone, Copy)]
pub struct RegisterWindow {
    start: usize,
    end: usize,
}

impl RegisterWindow {
    /// # Safety
    /// `start..start + len` must hold device registers, mapped for the whole
    /// run of the firmware, whose reads and writes cannot change memory that
    /// Rust owns: no DMA addresses, lengths or triggers.
    pub const unsafe fn new(start: usize, len: usize) -> Self {
        Self { start, end: start + len }
    }

    fn checked<T>(&self, address: usize) -> *mut T {
        let width = size_of::<T>();
        if address < self.start || address > self.end - width || !address.is_multiple_of(width) {
            crate::halt_always!();
        }
        address as *mut T
    }

    pub fn read8(&self, address: usize) -> u8 {
        // SAFETY: a checked register of this window (see `new`).
        unsafe { self.checked::<u8>(address).read_volatile() }
    }

    pub fn read32(&self, address: usize) -> u32 {
        // SAFETY: a checked register of this window (see `new`).
        unsafe { self.checked::<u32>(address).read_volatile() }
    }

    pub fn write32(&self, address: usize, value: u32) {
        // SAFETY: a checked register of this window (see `new`).
        unsafe { self.checked::<u32>(address).write_volatile(value) }
    }

    pub fn write16(&self, address: usize, value: u16) {
        // SAFETY: a checked register of this window (see `new`).
        unsafe { self.checked::<u16>(address).write_volatile(value) }
    }

    pub fn modify32(&self, address: usize, f: impl FnOnce(u32) -> u32) {
        self.write32(address, f(self.read32(address)));
    }
}

/// The PHY and RF registers.
// SAFETY: baseband, RF and calibration control; no DMA.
pub static PHY: RegisterWindow = unsafe { RegisterWindow::new(0x0ab8_0000, 0x6_0000) };

/// Platform clock and analog control.
// SAFETY: clock gating and analog settings; no DMA.
pub static PLATFORM_CONTROL: RegisterWindow = unsafe { RegisterWindow::new(0x0ac8_0000, 0x1000) };

/// The MAC and packet-DMA registers.
///
/// Unlike the PHY's, some of these hold packet-RAM addresses the MAC then
/// reads and writes by DMA: the rate RAM, TX command and response
/// descriptor bases. The firmware writes those only with the addresses of
/// the MAC's control objects in packet RAM ([`mac_objects`]), which Rust
/// code never borrows; a wrong value there is a firmware bug the window
/// cannot catch.
// SAFETY: see above.
pub static MAC: RegisterWindow = unsafe { RegisterWindow::new(0x09c0_0000, 0x2000) };

/// The four hardware TX rings (0x80 each): TX command DMA bases and
/// lengths, under the same contract as [`MAC`].
// SAFETY: see `MAC`.
pub static TX_RINGS: RegisterWindow = unsafe { RegisterWindow::new(0x09c6_0000, 0x200) };

/// The MAC's control objects in packet RAM holding `address`: the
/// descriptors, rate RAM and response frames from the response pointers to
/// the interface metadata, or the automatic response list. Never the HIF
/// buffers between them, which Rust borrows.
pub fn mac_objects(address: usize) -> RegisterWindow {
    use crate::packet_ram;
    let (start, end) = if packet_ram::automatic_response_list().contains(&address) {
        let list = packet_ram::automatic_response_list();
        (list.start, list.end)
    } else {
        (packet_ram::response_pointers().start, packet_ram::interface_metadata() + packet_ram::INTERFACE_METADATA_SIZE)
    };
    // SAFETY: linker-placed packet-RAM objects only the MAC and these
    // volatile accesses touch.
    unsafe { RegisterWindow::new(start, end - start) }
}

/// The window holding `address`: the PHY's, or platform control's.
pub fn phy_window(address: usize) -> &'static RegisterWindow {
    if address >= 0x0ac8_0000 { &PLATFORM_CONTROL } else { &PHY }
}
