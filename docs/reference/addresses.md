---
title: Packet RAM addressing
description: The encodings the XR819 engines use for the same packet SRAM, and the rules for converting between them.
---
The same packet SRAM byte has several representations, one per hardware user. They are not interchangeable, and mixing them corrupts memory silently instead of faulting.

## Address classes

| Class | Form | Used for |
|---|---|---|
| CPU address, runtime pool | `0x0900_7000`–`0x0901_662f` | CPU loads and stores, software ownership records |
| CPU address, RX FIFO | `0x0940_0000`–`0x0940_7fff` | CPU access to FIFO slots |
| MAC packet offset | `cpu & 0x007f_ffff` | MAC registers and command-word operands |
| DMA bus address | `cpu & 0xf6ff_ffff` | HIF descriptors, AES source and destination, packet DMA |
| A-MPDU transfer word | <code>0x6500_0000 &#124; (cpu &amp; 0x001f_fffc)</code> | MAC command streams only |
| RX FIFO offset | below `0x7000` | Producer and consumer registers |

For example, the HIF input buffers at CPU address `0x0900_800c` appear as MAC offset `0x0000_800c` and bus address `0x0000_800c`, and an aggregate member there is transferred with the word `0x6500_800c`. A FIFO slot at offset `0x0120` is read by the CPU at `0x0940_0120`.

## Conversion rules

- **Encode only from a CPU address.** Every hardware form is derived from a CPU address that belongs to a known object.
- **Never decode.** A hardware word is not a CPU pointer, and OR-ing a base back into it does not make one. The firmware recovers the CPU address from its own records instead: the HIF queue entry, the TX pipe registry or the A-MPDU software record.
- **Check the whole range.** An address is valid only if the complete access, not just its first byte, lies in one object.
- **Wrap FIFO offsets, not slots;** see [Receive DMA](rx-dma.html#offsets-and-wrap-around).
