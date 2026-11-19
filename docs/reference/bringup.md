---
title: Platform bring-up
section: Hardware reference
---
# Platform bring-up

The vendor fixes the order in `0x000164bc`. It is not a suggestion: several blocks must be programmed before the memory and clock remapping they depend on, and two of them (the `+0xac800bc` update and the `0x0aa80004 = 0x200` transition) mark points after which earlier aliases stop working.

```
wait until DTCM word 0x04001428 reads 0
0x00015926                      no-op in this image
0x0ac800bc                     clear bit 0, set bit 23
0x000009e0                     remap/platform preparation
0x00000ba8 -> 0x00000a74        high-bootstrap platform/IRQ setup
0x00015b60                     diagnostic console setup
0x00000c58 -> 0x00000bb0        DMA/clock setup, IRQ 4 registration
0x00000ab4                     HIF activation and wait
0x00000c38                     IRQ 6 registration
0x00000a88                     IRQ 18 and IRQ 20 registration
0x00000b88                     IRQ 21 registration
0x000009ac                     complete subsystem initialization
0x0abb0010..0x0abb0030         interrupt routing words
0x00016550                     write-buffer drain, IRQ/FIQ enable
0x0000f140                     scheduler entry
```

### What 0x000009e0 does

1. Sample all eight remap windows through `0x0aa80008`/`0x0aa8002c`, and relocate windows 3, 4 and 5.
2. If window 0 bit 27 is clear: set startup mode 3, set `feature_control |= 0x0018_0200`, clear `system_control.control` bit 5 and set bit 6, set `secondary_control` bit 6, then poll `status` bit 5. If it reads 0 (it does on this device), clear `secondary_control` bit 6 and drop startup mode to 1. Restore `feature_control`.
3. Start timer 0 free-running: load `0xffff_ffff`, prescale `0x4f`, control `0x80`. Prescale timer 1 to `0x4f`, mode 0.
4. Reset the interrupt controller: configuration `0x1600_a037`, clear and acknowledge all sources, vector base, enable 0, control 1.
5. Mask clock sources: `mask_44 &= 0x7fff_f777`, `mask_48 &= 0x777f_ffff`.

### What 0x00000bb0 does

1. Drain the write buffer, then reprogram `system_control`: clear bit 7, set bit 4; clear `secondary_control` bit 4.
2. `mask_48 |= 0xf0`, `mask_40 |= 0x00f0_0000`.
3. `platform_control.shared_pointer = 0x0004_0090`, `power_control = 0x10`, and (if no remap windows) `configuration |= 0x0012_0000`.
4. `clock_reset.control_04 = 0x200`. After this write, the `0x0900ff98` alias may stop reading; that is a mapping change.
5. `parameter_a0 = 31`, `parameter_9c = 6`, then poll `ready_status` bit 0 (bounded).
6. `clock_flags = 0x0c02` if `clock_status` bit 3 is clear, else `0x0042`.

**Trap: stack placement.** The Rust stack must be in low SRAM across the `0x0aa80004` transition. An image that kept its stack in the `0xfffxxxxx` window failed at exactly this step, and the failure looked like a packet-memory access fault. With the stack below the transition, the same code passes.
