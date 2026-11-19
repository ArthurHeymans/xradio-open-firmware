---
title: Platform bring-up
description: The XR819 platform start-up order, from the configuration word to clocks and DMA.
---
Platform bring-up prepares memory remapping, timers, the interrupt controller and the clocks. The order matters: several blocks must be programmed before the remapping they depend on, and one write changes the packet-RAM mapping for everything after it. CPU IRQ and FIQ stay masked throughout.

## Configuration word

Clear bit 0 and set bit 23 of `PlatformControl.CONFIGURATION` (`0x0ac8_00bc`). This comes before everything below.

## Remap, memory and interrupts

1. Sample the eight [remap windows](clock.html#remap-windows) and relocate windows 3, 4 and 5.
2. If remap window 0 has bit 27 clear, run the [memory probe](system.html#memory-probe), which sets the startup mode.
3. Start timer 0 free-running and prescale timer 1, as in [Timers](timers.html#programming).
4. [Reset the interrupt controller](interrupts.html#firmware-reset-sequence).
5. Mask clock sources: `MASK_44 &= 0x7fff_f777`, `MASK_48 &= 0x777f_ffff`.

## Debug console

Two writes reproduce the hardware effects of the vendor's high-SRAM support code; the UART's interrupts stay disabled.

{% include register-rows.html data=site.data.platform prefix="platform" section="console" %}

## DMA and clocks

1. `SystemControl.CONTROL`: clear bit 7, set bit 4. `SECONDARY_CONTROL`: clear bit 4.
2. `MASK_48 |= 0xf0`, `MASK_40 |= 0x00f0_0000`.
3. `SHARED_POINTER = 0x0004_0090`, `POWER_CONTROL = 0x10`. If no remap window is present, `CONFIGURATION |= 0x0012_0000`.
4. `ClockReset.CONTROL_04 = 0x200`. The stack must be in DTCM; see [the `CONTROL_04` transition](clock.html#the-control_04-transition).
5. `PARAMETER_A0 = 31`, `PARAMETER_9C = 6`.
6. Poll `READY_STATUS` bit 0, at most `0x200` times.
7. `CLOCK_FLAGS = 0x0c02` if `CLOCK_STATUS` bit 3 is clear, else `0x0042`.

[Host handshake](hif-flow.html) comes next.
