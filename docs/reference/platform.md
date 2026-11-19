---
title: Platform control
section: Hardware reference
---
# Platform control

{% include registers/PlatformControl.md %}

- **SHARED_POINTER**: `0x0004_0090` in the DMA path. Points into DTCM.

- **POWER_CONTROL**: `0x10`.

- **READY_STATUS**: Bit 0 set when the clock/PLL block has settled; polled with a bound of `0x200` iterations.

- **CLOCK_FLAGS**: `0x0c02` when `+0x94` bit 3 is clear, else `0x0042`.

- **CLOCK_STATUS**: Bit 3 selects between the two clock-flag words above.

- **PARAMETER_9C**: `6`.

- **PARAMETER_A0**: `31`.

- **CONFIGURATION**: Before platform init: clear bit 0, set bit 23. Later, if no remap windows are present, OR `0x0012_0000`.

**Ordering constraint at `0x0ac8_00bc`.** The `+0xbc` update (clear bit 0, set bit 23) must happen *before* `FUN_000009e0` runs, exactly as the vendor does it from `0x000164c8`. The same applies to `clock_reset+0x04 = 0x200`: it is a single transition whose position in the sequence matters, and it is the point after which a former packet-RAM alias is no longer readable. A lost alias is not evidence that the CPU stopped.
