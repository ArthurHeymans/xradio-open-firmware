ENTRY(_start)

/*
 * Run-time support comes from aarch32-rt: its vector table, reset routine
 * (_default_start: zero .bss and the stacks, copy .data, set the mode stacks,
 * call kmain in SYS mode) and exception trampolines. Its stock link.x cannot
 * be used as is: the packer needs explicit program headers so that nothing in
 * DTCM or packet RAM becomes a load segment. This script provides the symbols
 * aarch32-rt expects instead.
 */

/*
 * The CPU is an ARM966E-S (CP15 ID 0x41259661) with fixed TCMs: 128 KiB of
 * ITCM at address 0 and DTCM at 0x04000000. Its TCM size register (CP15 c0,
 * op2 2) reads 0x001c0200, 128 KiB ITCM and 64 KiB DTCM, but DTCM above
 * 0x0400c000 does not read back what was written: 48 KiB are usable. Writes
 * up to the end of the 128 KiB ITCM read back and move nothing below.
 *
 * ITCM holds code and read-only data only. Mutable state lives in DTCM:
 * .data, which the loader writes in place, the exception record and .bss
 * below the mode stacks.
 *
 * Packet-RAM sections below are independent address-only ownership objects.
 * They deliberately have no enclosing MEMORY region and no program header:
 * unknown holes and bootstrap overlays are not allocatable runtime storage.
 */
MEMORY
{
    ITCM (rx) : ORIGIN = 0x00000000, LENGTH = 0x20000
    /* .data, the exception record and .bss, below the stacks. */
    DTCM_STATE (rw) : ORIGIN = 0x04000000, LENGTH = 0x0a000
    DTCM_STACKS (rw) : ORIGIN = 0x0400a000, LENGTH = 0x02000

    /* Rust-owned runtime objects are packed in source-controlled section order. */
    PACKET_RUNTIME (rw) : ORIGIN = 0x09007000, LENGTH = 0xf630
    PACKET_RX_FIFO (rw) : ORIGIN = 0x09400000, LENGTH = 0x8000
}

PHDRS
{
    text PT_LOAD FLAGS(5);
    data PT_LOAD FLAGS(6);
}

SECTIONS
{
    .text ORIGIN(ITCM) : ALIGN(4)
    {
        KEEP(*(.vector_table))
        *(.text .text.*)
        *(.rodata .rodata.*)
        /*
         * The image packer requires every PT_LOAD file size to be word
         * aligned. Without this the read-only segment ends wherever the last
         * rodata item happens to land and packing succeeds only by luck.
         */
        . = ALIGN(4);
    } > ITCM :text

    /*
     * Loaded in place by the loader, mostly as fill records: the large state
     * objects are nearly all zeros. aarch32-rt's copy of .data runs onto
     * itself.
     */
    .data : ALIGN(4)
    {
        __sdata = .;
        *(.data .data.*)
        . = ALIGN(4);
        __edata = .;
    } > DTCM_STATE :data
    __sidata = __sdata;

    /* Not zeroed at startup, so a restart can still read the last fault. */
    .noinit.exception (NOLOAD) : ALIGN(8)
    {
        KEEP(*(.noinit.exception))
    } > DTCM_STATE :NONE

    .packet_ram.runtime (NOLOAD) : ALIGN(4)
    {
        __packet_ram_runtime_start = .;
        KEEP(*(SORT_BY_NAME(.packet_ram.runtime.*)))
        __packet_ram_runtime_end = .;
    } > PACKET_RUNTIME :NONE

    __packet_ram_lmc_anchor_0 = 0x09016ab4;
    __packet_ram_lmc_anchor_1 = 0x09017500;

    .packet_ram.rx_fifo_backing 0x09400000 (NOLOAD) : ALIGN(4)
    {
        KEEP(*(.packet_ram.rx_fifo_backing))
    } > PACKET_RX_FIFO :NONE

    /* Startup zeroes .bss; nothing loads it. */
    .bss (NOLOAD) : ALIGN(4)
    {
        __sbss = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(4);
        __ebss = .;
    } > DTCM_STATE :NONE

    /*
     * aarch32-rt's mode stacks, lowest first. The SYS stack, which runs
     * everything but the exception entries, takes what the others leave.
     */
    .stacks (NOLOAD) : ALIGN(8)
    {
        _stacks_low_end = .;
        _und_stack_low_end = .;
        . += _und_stack_size;
        _und_stack_high_end = .;
        _svc_stack_low_end = .;
        . += _svc_stack_size;
        _svc_stack_high_end = .;
        _abt_stack_low_end = .;
        . += _abt_stack_size;
        _abt_stack_high_end = .;
        _hyp_stack_low_end = .;
        _hyp_stack_high_end = .;
        _irq_stack_low_end = .;
        . += _irq_stack_size;
        _irq_stack_high_end = .;
        _fiq_stack_low_end = .;
        . += _fiq_stack_size;
        _fiq_stack_high_end = .;
        _sys_stack_low_end = .;
        . = ORIGIN(DTCM_STACKS) + LENGTH(DTCM_STACKS);
        _sys_stack_high_end = .;
        _stacks_high_end = .;
    } > DTCM_STACKS :NONE
    _sys_stack_size = _sys_stack_high_end - _sys_stack_low_end;

    ASSERT(_sys_stack_low_end == 0x0400a500,
           "the SYS stack no longer starts where check-stack.py expects")

    ASSERT(ADDR(.text) + SIZEOF(.text) <= ORIGIN(ITCM) + LENGTH(ITCM),
           "XR819 image exceeds the ITCM")
    ASSERT(ORIGIN(DTCM_STACKS) == ORIGIN(DTCM_STATE) + LENGTH(DTCM_STATE),
           "XR819 DTCM state and stacks are not contiguous")

    ASSERT(__packet_ram_runtime_start == ORIGIN(PACKET_RUNTIME),
           "Rust packet-RAM region moved")
    ASSERT(__packet_ram_runtime_end == ORIGIN(PACKET_RUNTIME) + LENGTH(PACKET_RUNTIME),
           "Rust packet-RAM region size changed")
    ASSERT(SIZEOF(.packet_ram.runtime) == 0xf630,
           "Rust packet-RAM objects no longer fill their region")
    ASSERT(__packet_ram_runtime_end <= __packet_ram_lmc_anchor_0,
           "Rust packet-RAM objects overlap retained LMC state")
    ASSERT(__packet_ram_lmc_anchor_0 == 0x09016ab4,
           "first LMC anchor moved")
    ASSERT(__packet_ram_lmc_anchor_1 == 0x09017500,
           "second LMC anchor moved")
    ASSERT(ADDR(.packet_ram.rx_fifo_backing) == 0x09400000,
           "RX FIFO backing moved")
    ASSERT(SIZEOF(.packet_ram.rx_fifo_backing) == 0x8000,
           "RX FIFO backing size changed")

    /DISCARD/ :
    {
        *(.ARM.exidx*)
        *(.ARM.extab*)
        *(.comment*)
    }
}

_und_stack_size = 0x100;
_svc_stack_size = 0x100;
_abt_stack_size = 0x100;
_hyp_stack_size = 0;
_irq_stack_size = 0x100;
_fiq_stack_size = 0x100;
_num_cores = 1;

/* aarch32-rt's defaults, as its link.x provides them. */
PROVIDE(_start                      = _default_start);
PROVIDE(_asm_undefined_handler      = _asm_default_undefined_handler);
PROVIDE(_asm_svc_handler            = _asm_default_svc_handler);
PROVIDE(_asm_hvc_handler            = _asm_default_hvc_handler);
PROVIDE(_asm_prefetch_abort_handler = _asm_default_prefetch_abort_handler);
PROVIDE(_asm_data_abort_handler     = _asm_default_data_abort_handler);
PROVIDE(_asm_irq_handler            = _asm_default_irq_handler);
PROVIDE(_asm_fiq_handler            = _asm_default_fiq_handler);
PROVIDE(_undefined_handler          = _default_handler);
PROVIDE(_svc_handler                = _default_handler);
PROVIDE(_hvc_handler                = _default_handler);
PROVIDE(_prefetch_abort_handler     = _default_handler);
PROVIDE(_data_abort_handler         = _default_handler);
PROVIDE(_irq_handler                = _default_handler);
PROVIDE(kmain_secondary             = _default_kmain_secondary);
