//! The hardware engine's known-answer test, run once at startup. Its record
//! answers MIB 0x100c in place of the counters if it failed.

use super::engine::{Engine, Transfer};
use super::hardware_kat::{
    HARDWARE_KAT_EXPECTED, HARDWARE_KAT_INPUT, HARDWARE_KAT_KEY, HARDWARE_KAT_MATRIX_CHECKSUMS,
    HARDWARE_KAT_MATRIX_LENGTHS, HARDWARE_KAT_MATRIX_MICS, HARDWARE_KAT_PN,
};
use super::{CCMP_HEADER_LEN, CCMP_MIC_LEN, CcmpError, Crypto, build_aad, header_length};

/// "HWCK": the record holds a self-test result.
const MAGIC: u32 = 0x4857_434b;
/// Longest frame the matrix builds.
const SCRATCH_LEN: usize = 26 + CCMP_HEADER_LEN + 1506 + CCMP_MIC_LEN;

fn checksum(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0_u32, |value, byte| {
        value.rotate_left(5).wrapping_add(u32::from(*byte))
    })
}

/// Run `frame` through the engine and wait, recording the trace points.
fn run(
    engine: &mut Engine,
    record: &mut [u32; 22],
    frame: &mut [u8],
    decrypt: bool,
) -> Result<(), CcmpError> {
    let control = u16::from_le_bytes([frame[0], frame[1]]);
    let (aad, aad_length) = build_aad(frame, control)?;
    let transfer = Transfer {
        frame_control: control,
        header: header_length(control),
        key: &HARDWARE_KAT_KEY,
        pn: HARDWARE_KAT_PN,
        aad: &aad,
        aad_length,
        decrypt,
    };
    let mut trace = |index: usize, value: u32| {
        if !decrypt {
            record[index] = value;
        }
    };
    engine.start_traced(frame, &transfer, &mut trace)?;
    engine.wait()
}

impl Crypto {
    /// Encrypt the fixed vector and a matrix of payload lengths, then
    /// decrypt the vector and a corrupted copy, all in HIF input buffer 0.
    ///
    /// # Safety
    /// Startup only: nothing else may use HIF input buffer 0 yet.
    pub unsafe fn run_self_test(&mut self) {
        const { assert!(SCRATCH_LEN <= crate::packet_ram::HIF_INPUT_SIZE) };
        // SAFETY: the caller guarantees exclusive use of HIF input buffer 0,
        // which is HIF_INPUT_SIZE (>= SCRATCH_LEN) octets of packet RAM.
        let scratch = unsafe {
            core::slice::from_raw_parts_mut(crate::packet_ram::hif_input(0) as *mut u8, SCRATCH_LEN)
        };
        let mut record = [0_u32; 22];
        record[0] = MAGIC;
        record[1] = self.self_test_on(&mut record, scratch);
        scratch.fill(0);
        self.self_test = record;
    }

    /// Returns the result word: 1 passed, `0xe0xx` failed at some step.
    fn self_test_on(&mut self, record: &mut [u32; 22], scratch: &mut [u8]) -> u32 {
        let engine = &mut self.engine;
        let started = crate::platform::timer_now();
        record[7] = engine.status();
        record[17] = Engine::init_payload_checksum();
        let elapsed = || crate::platform::timer_now().wrapping_sub(started);
        if engine.initialize().is_err() {
            record[8] = engine.status();
            record[4] = elapsed();
            return 0xe005;
        }
        record[8] = engine.status();

        // The fixed vector.
        let frame = &mut scratch[..HARDWARE_KAT_INPUT.len()];
        frame.copy_from_slice(&HARDWARE_KAT_INPUT);
        let operation = run(engine, record, frame, false);
        record[4] = elapsed();
        record[2] = engine.irqs();
        record[3] = engine.completion_status();
        record[18] = engine.status();
        let mismatch = HARDWARE_KAT_EXPECTED
            .iter()
            .zip(frame.iter())
            .position(|(expected, actual)| expected != actual);
        record[6] = checksum(frame);
        match (operation, mismatch) {
            (Err(CcmpError::HardwareTimeout), _) => return 0xe001,
            (Err(CcmpError::MalformedFrame), _) => return 0xe004,
            (_, Some(index)) => {
                record[5] = ((index as u32) << 16)
                    | (u32::from(HARDWARE_KAT_EXPECTED[index]) << 8)
                    | u32::from(frame[index]);
                return 0xe003;
            }
            (Err(CcmpError::Authentication), None) => return 2,
            (Err(_), None) => return 0xe002,
            (Ok(()), None) => {}
        }

        // Payload lengths around the AES block size and the longest frame.
        for (case, payload_length) in HARDWARE_KAT_MATRIX_LENGTHS.iter().copied().enumerate() {
            let frame = &mut scratch[..26 + CCMP_HEADER_LEN + payload_length + CCMP_MIC_LEN];
            frame[..34].copy_from_slice(&HARDWARE_KAT_INPUT[..34]);
            for (index, byte) in frame[34..34 + payload_length].iter_mut().enumerate() {
                *byte = (index as u8).wrapping_mul(17).wrapping_add(3);
            }
            frame[34 + payload_length..].fill(0);
            let operation = run(engine, record, frame, false);
            record[2] |= engine.irqs();
            let actual = checksum(frame);
            let mic = &frame[34 + payload_length..];
            if operation.is_err()
                || actual != HARDWARE_KAT_MATRIX_CHECKSUMS[case]
                || mic != &HARDWARE_KAT_MATRIX_MICS[case * 8..case * 8 + 8]
            {
                record[5] = HARDWARE_KAT_MATRIX_CHECKSUMS[case];
                record[6] = actual;
                record[20] = payload_length as u32;
                return if operation == Err(CcmpError::MalformedFrame) {
                    0xe004
                } else {
                    0xe006
                };
            }
            record[19] = (case + 1) as u32;
        }

        // Decryption, then a corrupted MIC that must fail.
        let frame = &mut scratch[..HARDWARE_KAT_EXPECTED.len()];
        frame.copy_from_slice(&HARDWARE_KAT_EXPECTED);
        let operation = run(engine, record, frame, true);
        record[2] |= engine.irqs();
        let plaintext = header_length(u16::from_le_bytes([frame[0], frame[1]])) + CCMP_HEADER_LEN
            ..frame.len() - CCMP_MIC_LEN;
        if operation.is_err() || frame[plaintext.clone()] != HARDWARE_KAT_INPUT[plaintext] {
            record[20] = 1;
            return 0xe007;
        }
        record[19] = 6;
        frame.copy_from_slice(&HARDWARE_KAT_EXPECTED);
        let last = frame.len() - 1;
        frame[last] ^= 1;
        if run(engine, record, frame, true) != Err(CcmpError::Authentication) {
            record[20] = 2;
            return 0xe007;
        }
        record[19] = 7;
        record[3] = engine.completion_status();
        record[18] = engine.status();
        record[4] = elapsed();
        1
    }

    /// The self-test record, if the test failed.
    pub fn failed_self_test(&self) -> Option<[u32; 22]> {
        (self.self_test[0] == MAGIC && self.self_test[1] != 1).then_some(self.self_test)
    }
}
