#![no_std]
#![no_main]

use core::arch::naked_asm;
use core::panic::PanicInfo;
use xr819_firmware::download::Control;
use xr819_firmware::loader::{apply_register_initialization, load_phy_init_tables};

#[unsafe(naked)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub extern "C" fn _start() -> ! {
    naked_asm!(
        "mrs r0, cpsr",
        "orr r0, r0, #0xc0",
        "msr cpsr_c, r0",
        // TCM region writes in the ARM926 form. The CPU is an ARM966E-S,
        // whose TCMs sit at fixed addresses and which documents no c9
        // registers; the writes are kept as they are known to be harmless.
        "mov r2, #1",
        "mcr p15, 0, r2, c9, c1, 1",
        "mov r1, #0x00400000",
        "orr r2, r1, r2",
        "mcr p15, 0, r2, c9, c1, 0",
        "ldr r2, =0x00001f74",
        "mcr p15, 0, r2, c1, c0, 0",
        "ldr r0, =__relocated_load",
        "ldr r1, =__relocated_start",
        "ldr r2, =__relocated_end",
        "ldr r3, ={main}",
        "2:",
        "cmp r1, r2",
        "ldrlo r4, [r0], #4",
        "strlo r4, [r1], #4",
        "blo 2b",
        "ldr r1, =__relocated_bss_start",
        "ldr r2, =__relocated_bss_end",
        "mov r4, #0",
        "3:",
        "cmp r1, r2",
        "strlo r4, [r1], #4",
        "blo 3b",
        // The loader stack remains in the reserved DTCM stack window. Native
        // section records are linker-bounded below 0x0400b000 and therefore
        // cannot overlap these active bootstrap frames.
        "ldr sp, =0x0400c000",
        "bx r3",
        main = sym rust_main,
    );
}

#[unsafe(no_mangle)]
extern "C" fn rust_main() -> ! {
    let control = unsafe { Control::get() };
    control.advertise();
    let size = control.wait_for_image_size();
    if let Ok(entry_address) = control.load_sectioned_image(size) {
        // Preserve the vendor container ordering boundary: all TCM copy/fill
        // records complete before the register and MAC/PHY publication tables,
        // and firmware entry occurs only after every loader-owned side effect.
        apply_register_initialization();
        load_phy_init_tables();
        for _ in 0..10_000 {
            #[cfg(target_arch = "arm")]
            unsafe {
                core::arch::asm!("nop", options(nomem, nostack));
            }
            #[cfg(not(target_arch = "arm"))]
            core::hint::spin_loop();
        }
        #[cfg(target_arch = "arm")]
        xr819_firmware::cpu::drain_write_buffer();
        let entry: extern "C" fn() -> ! = unsafe { core::mem::transmute(entry_address) };
        entry();
    }

    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
