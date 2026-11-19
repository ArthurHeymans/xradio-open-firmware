---
title: System control
description: The XR819 system control block at 0x0a98_0000, used by the memory probe and the DMA and clock set-up.
---
Three registers at `0x0a98_0000`. Two start-up steps use them: the memory probe during platform preparation, and the DMA and clock set-up. Each flips a pair of bits whose function is unknown; [Platform bring-up](bringup.html) gives the order.

## Registers

{% include registers/SystemControl.md %}

### CONTROL

{% include registers/SystemControlBits.md %}

### STATUS

{% include registers/SystemStatusBits.md %}

### SECONDARY_CONTROL

{% include registers/SecondaryControlBits.md %}

## Memory probe

The probe runs only when remap window 0 has bit 27 clear (see [Clock and reset](clock.html#remap-windows)):

1. Set startup mode 3 and OR `0x0018_0200` into the clock block's `FEATURE_CONTROL`.
2. In `CONTROL`, clear bit 5 and set bit 6. Set `SECONDARY_CONTROL` bit 6.
3. Read `STATUS` bit 5. If it is set, clear `SECONDARY_CONTROL` bit 6 and read `STATUS` again; if bit 5 has now cleared, drop to startup mode 1.
4. Restore `FEATURE_CONTROL`.

The startup mode is recorded for later use: the PHY set-up checks its bit 1.
