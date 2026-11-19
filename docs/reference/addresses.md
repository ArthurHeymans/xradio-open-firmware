---
title: Packet-RAM address encodings
section: Hardware reference
---
# Packet-RAM address encodings

The same storage has several hardware representations. They are not interchangeable, and mixing them produces silent corruption rather than a fault.

| Class | Shape | Valid use |
| --- | --- | --- |
| CPU runtime address | 0x0900_7000..0x0901_6630 | CPU loads/stores and software ownership records |
| CPU RX FIFO address | 0x0940_0000..0x0940_8000 | CPU access to FIFO slots after bounded offset normalisation |
| MAC 23-bit offset | cpu & 0x007f_ffff | MAC registers and command-word operands |
| DMA/HIF bus address | cpu & 0xf6ff_ffff | HIF, AES/MIC and packet-DMA descriptors |
| A-MPDU transfer | 0x6500_0000 \| (cpu & 0x001f_fffc) | MAC command streams only |
| RX FIFO logical offset | offset < 0x7000 | Producer/consumer accounting only |

A bus encoding is never a CPU pointer. Decoding a hardware word by OR-ing a CPU base, or using the masked value directly as a pointer, is invalid unless an independently owned CPU-form identity proves the target object. In the open firmware, production `0x007f_ffff` and `0xf6ff_ffff` masks exist only inside the packet-RAM module; every caller supplies a linker-owned root or an object base.
