---
title: Platform control
description: The XR819 platform control block at 0x0ac8_0000, its clock and power settings and their ordering.
---
The first `0xc0` bytes of the `0x0ac8_0000` window hold clock, power and configuration words. The [DMA and clock set-up](bringup.html#dma-and-clocks) writes most of them once; their encodings are largely unknown, so the firmware writes the vendor's values in the vendor's order.

## Registers

{% include registers/PlatformControl.md %}

### READY_STATUS

{% include registers/PlatformReady.md %}

### CLOCK_STATUS

{% include registers/PlatformClockStatus.md %}

### CONFIGURATION

{% include registers/PlatformConfiguration.md %}

## Other platform words

Two words the typed layout does not declare, written during RF start-up: one inside its range and one beyond it.

{% include register-rows.html data=site.data.platform prefix="platform" section="extra" %}

## Ordering

<div class="warning" markdown="1">
**Update `CONFIGURATION` first.** Clearing bit 0 and setting bit 23 must happen before platform preparation samples the remap windows.
</div>

The other registers are written during [DMA and clock set-up](bringup.html#dma-and-clocks), in the order given there.
