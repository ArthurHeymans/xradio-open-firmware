---
title: System control
section: Hardware reference
---
# System control

Three registers. Used by memory probing and by DMA/clock bring-up.

{% include registers/SystemControl.md %}

- **CONTROL**: Probe path: clear bit 5, set bit 6. DMA path: clear bit 7, set bit 4.

- **STATUS**: Bit 5 is polled after the bit-6 transition; it reads 0 on this device and moves startup mode from 3 to 1.

- **SECONDARY_CONTROL**: Bit 6 set during the probe. DMA path: bit 4 cleared. Bit 6 is cleared again if `STATUS` bit 5 reads set.
