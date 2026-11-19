---
title: Clock and reset
section: Hardware reference
---
# Clock and reset

## Clock / reset block (0x0aa8_0000)
{:#s5-1}

{% include registers/ClockReset.md %}

- **CONTROL_04**: Written `0x200` once, in `0x00000bb0`. After this write the `0x0900ff98` packet-RAM alias disappears on some images; that is a mapping effect, not a CPU stop.

- **WINDOW_SELECTOR**: Bits 2..4 select one of eight remap windows to be read through `+0x2c`. Bits other than 2..4 are preserved.

- **FEATURE_CONTROL**: Bits `0x0018_0200` are set during the memory probe and restored afterwards.

- **WINDOW_VALUE**: Reads the selected remap window. Eight 32-bit descriptors are sampled once at startup.

- **MASK_40**: OR `0x00f0_0000` in the DMA/clock path.

- **MASK_44**: AND `0x7fff_f777` at interrupt setup.

- **MASK_48**: AND `0x777f_ffff` at interrupt setup, then OR `0xf0` in the DMA path.

### Remap window relocation

The eight window descriptors are sampled by `FUN_00015944`. If any is nonzero, windows 3, 4 and 5 are rewritten with a relocated base: the page number (bits 12..27) is shifted, biased by `0x166` and written back, with window 4 also carrying the overflow into bits 28..31 and window 5 receiving the low nibble of that overflow. This is a fixed translation of the vendor algorithm and is reproduced exactly:

```
t3 = ((w3 & 0x0fff_ffff) >> 12) + 0x166
t4 = ((w4 & 0x0fff_ffff) >> 12) + 0x166
th = ((w4 >> 28) | (w5 << 4)) + 0x166
w3 = (w3 & 0xf000_0fff) | (t3 << 12)
w4 = (w4 & 0x0000_0fff) | (t4 << 12) | (th << 28)
w5 = (w5 & 0xffff_f000) | ((th & 0xffff) >> 4)
```

Window 0 is later read as the remap control word: if its bit 27 is clear, startup mode becomes 3 and the probe sequence runs.
