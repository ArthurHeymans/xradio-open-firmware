//! The AES-CCM engine: one transfer at a time, in place. The hardware engine
//! moves the payload by DMA and signals completion on IRQ 18/20; host builds
//! run the same transfer through RustCrypto at once.

#[cfg(target_arch = "arm")]
use super::qos_tid;
use super::{CCMP_HEADER_LEN, CCMP_MIC_LEN, CcmpError};
#[cfg(target_arch = "arm")]
use tock_registers::{
    interfaces::{Readable, Writeable},
    register_bitfields, register_structs,
    registers::{ReadWrite, WriteOnly},
};

/// One CCMP transfer, apart from the frame it works on.
pub(super) struct Transfer<'a> {
    pub frame_control: u16,
    /// Length of the 802.11 header; the CCMP header follows it.
    pub header: usize,
    pub key: &'a [u8; 16],
    pub pn: [u8; 6],
    pub aad: &'a [u8; 30],
    pub aad_length: usize,
    pub decrypt: bool,
}

impl Transfer<'_> {
    /// The payload between the CCMP header and the MIC.
    fn payload(&self, frame_len: usize) -> Result<core::ops::Range<usize>, CcmpError> {
        let start = self.header + CCMP_HEADER_LEN;
        let end = frame_len
            .checked_sub(CCMP_MIC_LEN)
            .ok_or(CcmpError::MalformedFrame)?;
        if end < start {
            return Err(CcmpError::MalformedFrame);
        }
        Ok(start..end)
    }
}

#[cfg(target_arch = "arm")]
pub(super) const fn hardware_aad_tail_command(aad_length: usize) -> Option<u32> {
    match aad_length {
        22 => Some(0x148b),
        24 => Some(0x14ab),
        28 => Some(0x14eb),
        30 => Some(0x140b),
        _ => None,
    }
}

#[cfg(target_arch = "arm")]
fn hardware_ccm_context(
    frame: &[u8],
    frame_control: u16,
    pn: [u8; 6],
    payload_length: usize,
) -> Result<[u8; 16], CcmpError> {
    if payload_length > u16::MAX as usize || frame.len() < 16 {
        return Err(CcmpError::MalformedFrame);
    }
    let mut context = [0_u8; 16];
    context[0] = 1;
    context[1] = qos_tid(frame, frame_control)?;
    context[2..8].copy_from_slice(&frame[10..16]);
    context[8..14].copy_from_slice(&pn);
    context[14..16].copy_from_slice(&(payload_length as u16).to_be_bytes());
    Ok(context)
}

#[cfg(target_arch = "arm")]
fn hardware_aad_stream(aad: &[u8; 30], aad_length: usize) -> Result<[u8; 32], CcmpError> {
    if hardware_aad_tail_command(aad_length).is_none() {
        return Err(CcmpError::MalformedFrame);
    }
    let mut stream = [0_u8; 32];
    stream[..2].copy_from_slice(&(aad_length as u16).to_be_bytes());
    stream[2..2 + aad_length].copy_from_slice(&aad[..aad_length]);
    Ok(stream)
}

#[cfg(target_arch = "arm")]
register_bitfields! {
    u32,
    /// Read side of `COMMAND_STATUS`. Writes are command words.
    AesStatus [
        /// Authenticated result. Sampled after the completion interrupt of a
        /// decryption: set when the MIC matched.
        AUTH_OK OFFSET(0) NUMBITS(1) [],
        /// Busy: a command written now is not accepted. Wait for it to clear
        /// before every command.
        BUSY OFFSET(12) NUMBITS(1) [],
        /// The FIFO accepts words. Later stages wait for it as well.
        FIFO_READY OFFSET(13) NUMBITS(1) [],
    ],
}

#[cfg(target_arch = "arm")]
register_structs! {
    AesRegisters {
        /// Write a command word; read status.
        (0x00 => command_status: ReadWrite<u32, AesStatus::Register>),
        /// Little-endian key, CCM context and AAD words.
        (0x04 => fifo: WriteOnly<u32>),
        /// Initialization stream; see the start-up self-test.
        (0x08 => init_port: WriteOnly<u32>),
        (0x0c => _reserved),
        /// DMA source, `cpu & 0xf6ff_ffff`.
        (0x10 => source: ReadWrite<u32>),
        /// DMA destination, same encoding.
        (0x14 => destination: ReadWrite<u32>),
        /// Payload length in octets, excluding CCMP header and MIC.
        (0x18 => length: ReadWrite<u32>),
        (0x1c => @END),
    }
}

#[cfg(target_arch = "arm")]
use crate::init_data::ENGINE_MODE1_INIT as MODE1_INIT_PAYLOAD;

/// A transfer gives up after 50 ms.
#[cfg(target_arch = "arm")]
const TIMEOUT_US: u32 = 50_000;

#[cfg(target_arch = "arm")]
const IRQ18: u32 = 1 << 18;
#[cfg(target_arch = "arm")]
const IRQ20: u32 = 1 << 20;

/// The hardware engine and the transfer it runs.
#[cfg(target_arch = "arm")]
pub(super) struct Engine {
    /// Start time and direction of the running transfer.
    running: Option<(u32, bool)>,
    /// Interrupt lines seen since the transfer started.
    irqs: u32,
    /// The command/status register when the transfer completed; bit 0 is
    /// the MIC check of a decryption.
    completion_status: u32,
    /// The key the engine holds: it keeps it across transfers, and loading
    /// it is over half of starting one. `None` after any failure.
    loaded_key: Option<[u8; 16]>,
}

#[cfg(target_arch = "arm")]
impl Engine {
    pub const fn new() -> Self {
        Self {
            loaded_key: None,
            running: None,
            irqs: 0,
            completion_status: 0,
        }
    }

    fn registers(&self) -> &'static AesRegisters {
        // SAFETY: the AES block is always-mapped MMIO at this address, and
        // `Engine` is its only user.
        unsafe { &*(crate::platform::aes_register_base() as *const AesRegisters) }
    }

    pub fn status(&self) -> u32 {
        self.registers().command_status.get()
    }

    pub fn irqs(&self) -> u32 {
        self.irqs
    }

    pub fn completion_status(&self) -> u32 {
        self.completion_status
    }

    fn wait_status(&mut self, predicate: impl Fn(u32) -> bool) -> Result<(), CcmpError> {
        let started = crate::platform::timer_now();
        while !predicate(self.status()) {
            if crate::platform::timer_now().wrapping_sub(started) >= TIMEOUT_US {
                self.loaded_key = None;
                return Err(CcmpError::HardwareTimeout);
            }
            core::hint::spin_loop();
        }
        Ok(())
    }

    fn write_fifo_block(&self, block: &[u8]) {
        for word in block.chunks_exact(4) {
            self.registers()
                .fifo
                .set(u32::from_le_bytes([word[0], word[1], word[2], word[3]]));
        }
    }

    /// Stream the vendor mode-1 initialization payload; its internal format is unknown.
    pub fn initialize(&mut self) -> Result<(), CcmpError> {
        self.loaded_key = None;
        let registers = self.registers();
        registers.command_status.set(0);
        self.wait_status(|status| status & (1 << 12) == 0)?;
        registers
            .init_port
            .set(0x8000_0000 | u32::from(MODE1_INIT_PAYLOAD[0]));
        for byte in &MODE1_INIT_PAYLOAD[1..] {
            registers.init_port.set(u32::from(*byte));
        }
        registers.command_status.set(0x9000);
        self.wait_status(|status| status & (1 << 12) == 0)
    }

    pub fn init_payload_checksum() -> u32 {
        MODE1_INIT_PAYLOAD.iter().fold(0_u32, |value, byte| {
            value.rotate_left(5).wrapping_add(u32::from(*byte))
        })
    }

    /// Load key, nonce and AAD and start the payload DMA without trace MMIO reads.
    pub fn start(&mut self, frame: &mut [u8], transfer: &Transfer<'_>) -> Result<(), CcmpError> {
        self.start_inner::<false>(frame, transfer, &mut |_, _| {})
    }

    /// Like [`Self::start`], sampling AES registers at the self-test's numbered points.
    pub fn start_traced(
        &mut self,
        frame: &mut [u8],
        transfer: &Transfer<'_>,
        trace: &mut impl FnMut(usize, u32),
    ) -> Result<(), CcmpError> {
        self.start_inner::<true>(frame, transfer, trace)
    }

    fn start_inner<const TRACE: bool>(
        &mut self,
        frame: &mut [u8],
        transfer: &Transfer<'_>,
        trace: &mut impl FnMut(usize, u32),
    ) -> Result<(), CcmpError> {
        debug_assert!(self.running.is_none());
        let payload = transfer.payload(frame.len())?;
        let payload_length = payload.len();
        let payload_address =
            crate::packet_ram::packet_dma_bus_address(frame[payload.start..].as_mut_ptr() as usize)
                .ok_or(CcmpError::InvalidDmaAddress)?;
        let context =
            hardware_ccm_context(frame, transfer.frame_control, transfer.pn, payload_length)?;
        let aad_stream = hardware_aad_stream(transfer.aad, transfer.aad_length)?;
        let tail =
            hardware_aad_tail_command(transfer.aad_length).ok_or(CcmpError::MalformedFrame)?;
        // Decryption uses the command one below encryption's.
        let direction = u32::from(transfer.decrypt);
        let registers = self.registers();

        if self.loaded_key != Some(*transfer.key) {
            self.loaded_key = None;
            self.wait_status(|status| status & (1 << 12) == 0)?;
            self.write_fifo_block(transfer.key);
            registers.command_status.set(0x1100);
            self.wait_status(|status| status & (1 << 13) != 0)?;
            if TRACE {
                trace(9, self.status());
            }
            self.loaded_key = Some(*transfer.key);
        }
        self.write_fifo_block(&context);
        self.wait_status(|status| status & (1 << 12) == 0)?;
        registers.command_status.set(0x1240);

        self.wait_status(|status| status & (1 << 13) != 0)?;
        if TRACE {
            trace(10, self.status());
        }
        self.write_fifo_block(&aad_stream[..16]);
        self.wait_status(|status| status & (1 << 12) == 0)?;
        registers.command_status.set(0x1403 - direction);

        self.wait_status(|status| status & (1 << 13) != 0)?;
        if TRACE {
            trace(11, self.status());
        }
        self.write_fifo_block(&aad_stream[16..]);
        self.wait_status(|status| status & (1 << 12) == 0)?;
        registers.command_status.set(tail - direction);

        registers.source.set(payload_address);
        registers.destination.set(payload_address);
        registers.length.set(payload_length as u32);
        if TRACE {
            trace(14, registers.source.get());
            trace(15, registers.destination.get());
            trace(16, registers.length.get());
        }
        self.irqs = 0;
        self.completion_status = 0;
        let _ = crate::interrupts::take_and_rearm(IRQ18 | IRQ20);
        crate::hif::drain_write_buffer();
        self.wait_status(|status| status & (1 << 12) == 0)?;
        if TRACE {
            trace(12, self.status());
        }
        registers.command_status.set(0x3008_1009 - direction);
        if TRACE {
            trace(13, self.status());
        }
        self.running = Some((crate::platform::timer_now(), transfer.decrypt));
        Ok(())
    }

    /// The running transfer's outcome once it is done. IRQs stay masked in
    /// the CPU, so acknowledge IRQ 18/20 here as the vendor's handler does
    /// (controller +0x04 only; it never writes +0x14 at runtime).
    pub fn poll(&mut self) -> Option<Result<(), CcmpError>> {
        let (started, decrypt) = self.running?;
        let pending = crate::interrupts::take_and_rearm(IRQ18 | IRQ20);
        if pending != 0 {
            self.completion_status = self.status();
            self.irqs |= pending;
        }
        let outcome = if self.irqs != 0 {
            // Encryption completion is established by the IRQ (and the
            // self-test's exact ciphertext and MIC); status bit 0 is the MIC
            // check of a decryption only.
            if decrypt && self.completion_status & 1 == 0 {
                Err(CcmpError::Authentication)
            } else {
                Ok(())
            }
        } else if crate::platform::timer_now().wrapping_sub(started) >= TIMEOUT_US {
            self.loaded_key = None;
            Err(CcmpError::HardwareTimeout)
        } else {
            return None;
        };
        self.running = None;
        Some(outcome)
    }

    pub fn wait(&mut self) -> Result<(), CcmpError> {
        loop {
            if let Some(outcome) = self.poll() {
                return outcome;
            }
            core::hint::spin_loop();
        }
    }
}

/// Host builds: RustCrypto, finished as soon as started.
#[cfg(not(target_arch = "arm"))]
pub(super) struct Engine {
    outcome: Option<Result<(), CcmpError>>,
    /// Tests: keep the transfer running.
    #[cfg(test)]
    pub held: bool,
}

#[cfg(not(target_arch = "arm"))]
impl Engine {
    pub const fn new() -> Self {
        Self {
            outcome: None,
            #[cfg(test)]
            held: false,
        }
    }

    pub fn start(&mut self, frame: &mut [u8], transfer: &Transfer<'_>) -> Result<(), CcmpError> {
        use aes::Aes128;
        use ccm::{
            Ccm,
            aead::{AeadInPlace, KeyInit, generic_array::GenericArray},
            consts::{U8, U13},
        };
        let payload = transfer.payload(frame.len())?;
        let nonce = super::build_nonce(frame, transfer.frame_control, transfer.pn)?;
        let cipher = Ccm::<Aes128, U8, U13>::new_from_slice(transfer.key)
            .map_err(|_| CcmpError::Authentication)?;
        let aad = &transfer.aad[..transfer.aad_length];
        let (data, mic) = frame.split_at_mut(payload.end);
        let data = &mut data[payload.start..];
        self.outcome = Some(if transfer.decrypt {
            let tag = GenericArray::clone_from_slice(mic);
            cipher
                .decrypt_in_place_detached(GenericArray::from_slice(&nonce), aad, data, &tag)
                .map_err(|_| CcmpError::Authentication)
        } else {
            cipher
                .encrypt_in_place_detached(GenericArray::from_slice(&nonce), aad, data)
                .map(|tag| mic.copy_from_slice(&tag))
                .map_err(|_| CcmpError::Authentication)
        });
        Ok(())
    }

    pub fn poll(&mut self) -> Option<Result<(), CcmpError>> {
        #[cfg(test)]
        if self.held {
            return None;
        }
        self.outcome.take()
    }

    pub fn wait(&mut self) -> Result<(), CcmpError> {
        self.poll().unwrap_or(Ok(()))
    }
}
