#![no_std]
#![no_main]

use aarch32_rt::entry;
use core::arch::global_asm;
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::panic::PanicInfo;
use xr819_firmware::configuration::Configuration;
use xr819_firmware::crypto::Crypto;
use xr819_firmware::firmware::Firmware;
use xr819_firmware::hif::{HifQueues, HifRingState, SHARED_BUFFER_SIZE, Transport};
use xr819_firmware::mac;
use xr819_firmware::phy::{initialize_mac_core_mode0, initialize_mac_software_state};
use xr819_firmware::platform::{
    enable_packet_controller, initialize_runtime_state, prepare_dma_and_clocks,
    prepare_high_platform_support, prepare_mac_receive_hardware, prepare_main_control,
    prepare_memory_and_interrupts, prepare_packet_dma, register_packet_dma_interrupts,
    register_post_activation_interrupts, try_activate_hif,
};
use xr819_firmware::radio::Radio;
use xr819_firmware::scan::Scan;
use xr819_firmware::tx;
use xr819_firmware::tx::HostTxDriver;
use xr819_firmware::wsm::StartupIndication;

/// Const-initialized storage taken once by the single reset-time owner.
///
/// Unlike a general static cell this needs no atomics: the references never
/// escape `rust_main`, and reset initializes each cell exactly once before
/// entering the cooperative loop.
struct SingleBootCell<T>(UnsafeCell<MaybeUninit<T>>);

unsafe impl<T> Sync for SingleBootCell<T> {}

impl<T> SingleBootCell<T> {
    const fn new() -> Self {
        Self(UnsafeCell::new(MaybeUninit::uninit()))
    }

    /// # Safety
    /// This may be called only once for each cell during one firmware boot.
    unsafe fn init_with(&'static self, value: impl FnOnce() -> T) -> &'static mut T {
        unsafe { (&mut *self.0.get()).write(value()) }
    }
}

/// A compile-time value handed out once as `&'static mut`. Unlike
/// [`SingleBootCell`], nothing is built on the stack at startup.
struct BootValue<T>(UnsafeCell<T>);

unsafe impl<T> Sync for BootValue<T> {}

impl<T> BootValue<T> {
    const fn new(value: T) -> Self {
        Self(UnsafeCell::new(value))
    }

    /// # Safety
    /// This may be called only once during one firmware boot.
    unsafe fn take(&'static self) -> &'static mut T {
        unsafe { &mut *self.0.get() }
    }
}

static HIF_RING_STATE: SingleBootCell<HifRingState> = SingleBootCell::new();
static HIF_QUEUES: SingleBootCell<HifQueues> = SingleBootCell::new();
static TRANSPORT: SingleBootCell<Transport> = SingleBootCell::new();
static RESPONSE_SCRATCH: SingleBootCell<[u8; SHARED_BUFFER_SIZE]> = SingleBootCell::new();
static HOST_TX_DRIVER: SingleBootCell<HostTxDriver> = SingleBootCell::new();
static TX_ENGINE: BootValue<tx::TxEngine> = BootValue::new(tx::TxEngine::new());
static CRYPTO: BootValue<Crypto> = BootValue::new(Crypto::new());
static RADIO: BootValue<Radio> = BootValue::new(Radio::new());
static SCAN: BootValue<Scan> = BootValue::new(Scan::new());
static CONFIGURATION: BootValue<Configuration> = BootValue::new(Configuration::new());

// The ARM9 exception vectors execute in ARM state even though the firmware
// body is Thumb. Each terminal veneer saves the unmodified shared registers on
// its preinitialized mode stack before committing the fixed noinit record.
// Exception entries for aarch32-rt's vector table. Each one records the
// cw1200 exception layout and publishes it; none returns. FIQ stays
// masked, so its entry only runs if something unmasks it.
#[cfg(target_arch = "arm")]
global_asm!(
    r#"
    .syntax unified
    .arm
    .section .text.xr819_exceptions,"ax",%progbits
    .align 2

    .macro XR819_EXCEPTION name, kind
    .global \name
\name:
    stmdb sp!, {{r0-r12, lr}}
    mov r0, #\kind
    b xr819_exception_common
    .endm

    XR819_EXCEPTION _asm_undefined_handler, 1
    XR819_EXCEPTION _asm_svc_handler, 2
    XR819_EXCEPTION _asm_prefetch_abort_handler, 3
    XR819_EXCEPTION _asm_data_abort_handler, 4
    XR819_EXCEPTION _asm_hvc_handler, 5

    // IRQ runs `interrupts::_irq_handler` in IRQ mode, on the IRQ stack.
    // aarch32-rt's trampoline would run it in SYS mode with only IRQ
    // masked, and the MAC's FIQ line, asserted whenever events wait, would
    // be taken at once. The handler never re-enables IRQ, so nothing nests
    // and LR_irq needs no protection.
    .global _asm_irq_handler
_asm_irq_handler:
    sub lr, lr, #4
    stmdb sp!, {{r0-r3, r12, lr}}
    bl _irq_handler
    ldmia sp!, {{r0-r3, r12, pc}}^
    XR819_EXCEPTION _asm_fiq_handler, 7

xr819_exception_common:
    mrs r10, cpsr
    orr r11, r10, #192
    msr cpsr_c, r11

    ldr r1, =XR819_EXCEPTION_RECORD
    mov r2, #0
    str r2, [r1, #0]
    str r0, [r1, #4]

    add r2, r1, #8
    mov r3, sp
    mov r4, #13
1:
    ldr r5, [r3], #4
    str r5, [r2], #4
    subs r4, r4, #1
    bne 1b

    add r5, sp, #56
    str r5, [r1, #60]
    ldr r6, [sp, #52]
    str r6, [r1, #64]
    mrs r7, spsr
    str r7, [r1, #68]
    str r10, [r1, #72]

    cmp r0, #4
    subeq r8, r6, #8
    beq 2f
    cmp r0, #3
    cmpne r0, #7
    subeq r8, r6, #4
    beq 2f
    tst r7, #32
    subne r8, r6, #2
    subeq r8, r6, #4
2:
    str r8, [r1, #76]
    // ARM966E-S has no MMU/MPU and no CP15 c5/c6 fault status/address registers.
    mov r8, #0
    str r8, [r1, #80]
    str r8, [r1, #84]

    ldr r8, =0x58434651
    str r8, [r1, #0]
    ldr r3, =xr819_exception_terminal
    blx r3
3:
    b 3b
    "#
);

#[unsafe(no_mangle)]
#[used]
static STARTUP_DEBUG_STAGE: u32 = u32::MAX;

fn debug_stop(stage: u32, marker: u32) {
    let selected = unsafe { (&raw const STARTUP_DEBUG_STAGE).read_volatile() };
    if selected == stage {
        let _ = marker;
        loop {
            core::hint::spin_loop();
        }
    }
}

/// Runs in SYS mode on the SYS stack, after aarch32-rt zeroed .bss.
#[entry]
fn main() -> ! {
    initialize_runtime_state();

    debug_stop(0, 0x5354_4700);
    prepare_main_control();
    debug_stop(1, 0x5354_4701);
    prepare_memory_and_interrupts();
    debug_stop(2, 0x5354_4702);
    prepare_high_platform_support();
    debug_stop(3, 0x5354_4703);
    prepare_dma_and_clocks();
    debug_stop(4, 0x5354_4704);

    if !try_activate_hif(1_000_000) {
        loop {
            core::hint::spin_loop();
        }
    }

    debug_stop(5, 0x5354_4705);
    register_post_activation_interrupts();
    let crypto = unsafe { CRYPTO.take() };
    // SAFETY: HIF input buffer 0 carries nothing before the transport starts.
    #[cfg(target_arch = "arm")]
    unsafe {
        crypto.run_self_test();
    }
    let hif_ring_state = unsafe { HIF_RING_STATE.init_with(HifRingState::new) };
    let hif_queues = unsafe { HIF_QUEUES.init_with(HifQueues::new) };
    let transport =
        unsafe { TRANSPORT.init_with(|| Transport::initialize(hif_ring_state, hif_queues)) };
    let tx_engine = unsafe { TX_ENGINE.take() };
    debug_stop(6, 0x5354_4706);

    // Vendor 0x9ac calls packet-DMA initialization immediately after 0x94c.
    prepare_packet_dma();
    // Vendor programs the RX/MAC tables once, after MAC core enable, via
    // `rx_subsystem_init`; we also do it there (`mac.rs:831`). This earlier
    // copy has no vendor counterpart.
    prepare_mac_receive_hardware();
    let radio = unsafe { RADIO.take() };
    radio.initialize();
    register_packet_dma_interrupts();
    debug_stop(8, 0x5354_4708);

    // Vendor 0x16d24 performs the complete 0x16ac6 software and hardware
    // setup before setting MAC control bit 11. Policy remains inactive until a
    // real channel request is serviced.
    {
        initialize_mac_software_state();
        initialize_mac_core_mode0();
        if let Err(_error) = mac::initialize_startup_state(1_000_000) {
            loop {
                core::hint::spin_loop();
            }
        }
        mac::initialize_tx_pipe_state();
    }
    // Vendor `0xc80` is the final hardware-visible step before `0x158fc`.
    enable_packet_controller();
    debug_stop(9, 0x5354_4709);

    let startup_label: &[u8] = b"XR819 open Rust native";

    transport.publish_with(|buffer| {
        StartupIndication {
            input_buffers: 30,
            input_buffer_size: 1632,
            // Mainline rejects nonzero startup status and firmware types above 4.
            status: 0,
            hardware_id: 7,
            hardware_sub_id: 9,
            // Bit 0 advertises 2.4 GHz. XR819 has no 5 GHz radio, so bit 1 must
            // remain clear or cw1200 exposes a phantom 5 GHz band.
            firmware_capabilities: 1 | xr819_firmware::wsm::CAP_PEER_HT,
            firmware_type: 2,
            firmware_api: 1,
            firmware_build: 1,
            firmware_version: 1,
            label: startup_label,
            config: [0; 4],
        }
        .encode(buffer)
        .ok()
    });

    // Command responses and retained class-0 confirmations are copied into
    // their original 1632-byte request buffers before publication. This buffer
    // is scratch only and is never exposed through a HIF descriptor.
    let response_scratch = unsafe { RESPONSE_SCRATCH.init_with(|| [0; SHARED_BUFFER_SIZE]) };
    let host_tx_driver = unsafe { HOST_TX_DRIVER.init_with(HostTxDriver::new) };
    let firmware = Firmware::new(
        transport,
        tx_engine,
        response_scratch,
        host_tx_driver,
        crypto,
        radio,
        unsafe { SCAN.take() },
        unsafe { CONFIGURATION.take() },
    );
    firmware.run()
}

#[cfg(target_arch = "arm")]
fn panic_file_hash(file: &str) -> u32 {
    file.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    })
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    #[cfg(target_arch = "arm")]
    {
        let (line, column, file_hash) = info
            .location()
            .map(|location| {
                (
                    location.line(),
                    location.column(),
                    panic_file_hash(location.file()),
                )
            })
            .unwrap_or((0, 0, 0));
        xr819_firmware::exception::panic_terminal(line, column, file_hash)
    }
    #[cfg(not(target_arch = "arm"))]
    {
        let _ = info;
        loop {
            core::hint::spin_loop();
        }
    }
}
