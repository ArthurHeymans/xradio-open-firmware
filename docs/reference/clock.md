---
title: Clock and reset
description: The XR819 clock and reset block at 0x0aa8_0000, its remap windows and clock masks.
---
The block at `0x0aa8_0000` holds the eight memory remap windows, three clock mask words and a one-time control write that changes the packet-RAM mapping.

## Registers

{% include registers/ClockReset.md %}

### WINDOW_SELECTOR

{% include registers/WindowSelector.md %}

### WINDOW_VALUE

{% include registers/RemapWindow.md %}

## Remap windows

At start-up the firmware selects each window in turn through `WINDOW_SELECTOR` and samples it from `WINDOW_VALUE`. If any window is nonzero, windows 3, 4 and 5 are stored relocated. Three page numbers move up by `0x166`: window 3's bits 27:12, window 4's bits 27:12, and a third formed from window 5's bits 11:0 above window 4's bits 31:28. The firmware reproduces the vendor arithmetic exactly:

```text
t3 = ((w3 & 0x0fff_ffff) >> 12) + 0x166
t4 = ((w4 & 0x0fff_ffff) >> 12) + 0x166
th = ((w4 >> 28) | (w5 << 4)) + 0x166
w3 = (w3 & 0xf000_0fff) | (t3 << 12)
w4 = (w4 & 0x0000_0fff) | (t4 << 12) | (th << 28)
w5 = (w5 & 0xffff_f000) | ((th & 0xffff) >> 4)
```

Window 0 then serves as the remap control word: if its bit 27 is clear, startup mode becomes 3 and the [memory probe](system.html#memory-probe) runs. Whether any window is nonzero also decides a later `PlatformControl.CONFIGURATION` update.

## The CONTROL_04 transition

`CONTROL_04` is written `0x200` exactly once, during [DMA and clock set-up](bringup.html#dma-and-clocks). After this write the packet-RAM word at `0x0900_ff98`, in the download control block, may no longer reflect what was written there. That is a mapping change, not a CPU stop.

<div class="warning" markdown="1">
**Keep the stack in DTCM across this write.** With the stack in high SRAM the firmware [fails at this write](measurements.html#image-loading-and-timing), in a way that looks like a packet-memory fault.
</div>
