---
title: Timers
section: Hardware reference
---
# Timers

{% include registers/Timers.md %}

- **TIMER0_LOAD**: `0xffff_ffff`; timer 0 free-runs.

- **NOW**: Microsecond counter, the firmware's only clock. Wraps at 232 µs (71.6 min).

- **TIMER0_CONTROL**: `0x80` to start free-running.

- **TIMER0_PRESCALE**: `0x4f`: 80 MHz divided to 1 MHz.

- **TIMER1_LOAD**: Delay in microseconds for the one-shot.

- **TIMER1_CONTROL**: `0xc1` starts a one-shot; it reads `0xc3` after firing. Writing `0xc1` over a fired timer does *not* restart it: write `0` first, always.

- **TIMER1_MODE**: `0`.

- **TIMER1_PRESCALE**: `0x4f`.

### Measured timer behaviour

| Requested delay | Actual sleep | Note |
| --- | --- | --- |
| 1,000 µs | 1,001 µs | +1 µs of overhead |
| 20,000 µs | 20,001 µs | +1 µs of overhead |
| 5 µs | 7 µs | below the resolution floor of the wake path |

The timer keeps counting while the CPU is in wait-for-interrupt, so it is a reliable wake source even with IRQ masked at the CPU.
