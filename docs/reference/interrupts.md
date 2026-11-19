---
title: Interrupt controller
description: The XR819 interrupt controller at 0x0a88_0000, its sources, and how the firmware takes interrupts.
---
A 32-source controller at `0x0a88_0000` with one IRQ and one FIQ output. Each source has one bit in the enable, pending and acknowledge registers; the [source table](#interrupt-sources-bits) below says who owns each one.

## Registers

{% include registers/InterruptController.md %}

### ACKNOWLEDGE, ENABLE, PENDING, RAW_STATE

Source 22 is drawn for completeness: it is routed to FIQ and never appears in these registers.

{% include registers/InterruptSources.md %}

### CONFIGURATION

{% include registers/InterruptConfiguration.md %}

## The FIQ line

`CONFIGURATION` routes source 22, the MAC's event line, to the CPU's FIQ; see `MAC_FIQ` and `MAC_EVENTS` in the [source table](#interrupt-sources-bits). The firmware keeps FIQ masked and drains the MAC event FIFO from its main loop.

## How the firmware takes interrupts

The IRQ handler only takes note:

```text
pending = PENDING
ENABLE &= ~pending        mask every pending source
ACKNOWLEDGE = pending     clear it
raised |= pending         record it for its owner
```

In the main loop each owner takes its bit from `raised`, clears the cause in its own device, and only then re-arms its source. A level source such as the HIF therefore cannot re-enter the handler before its owner has run, and a latched source such as the AES engine or timer 1 is not lost while its owner is busy. The handler never touches main-loop state.

The handler runs in IRQ mode, entered from the firmware's own vector with IRQ and FIQ masked. A handler that unmasked FIQ would take it at once, because the MAC asserts the line whenever events wait.

## Firmware reset sequence

The firmware resets the controller during platform preparation, in this order: `CONFIGURATION = 0x1600_a037`, `CLEAR_14 = 0xffff_ffff`, `ACKNOWLEDGE = 0xffff_ffff`, `VECTOR_BASE = 0x1000_0000` (the controller base shifted left by 9, truncated), `ENABLE = 0`, `CONTROL = 1`.
