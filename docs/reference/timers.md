---
title: Timers
description: The XR819 timers at 0x0ac0_0000, the firmware's microsecond clock and its wake-up source.
---
Two timers at `0x0ac0_0000`, both clocked at 1 MHz from 80 MHz. Timer 0 runs free and is the firmware's only clock. Timer 1 is a one-shot that raises IRQ 6; the idle loop uses it to bound each sleep.

## Registers

{% include registers/Timers.md %}

### TIMER0_CONTROL, TIMER1_CONTROL

{% include registers/TimerControl.md %}

## Programming

Timer 0 is started once at bring-up: `TIMER0_LOAD = 0xffff_ffff`, `TIMER0_PRESCALE = 0x4f`, `TIMER0_CONTROL = 0x80`. From then on `NOW` counts microseconds.

Timer 1 needs `TIMER1_PRESCALE = 0x4f` and `TIMER1_MODE = 0`, also set at bring-up, and source 6 enabled in the [interrupt controller](interrupts.html) so that its expiry ends a wait for interrupt. To wait with a timeout of *n* µs:

1. Write `TIMER1_CONTROL = 0`. This step is not optional: `0xc1` written over a fired timer does not start it again.
2. Write `TIMER1_LOAD = n`.
3. Write `TIMER1_CONTROL = 0xc1`. When the count ends, IRQ 6 becomes pending; a test image read `TIMER1_CONTROL` as `0xc3` then.
4. Wait for interrupt, then write `TIMER1_CONTROL = 0` and acknowledge IRQ 6.

The timer keeps counting while the CPU waits, so it ends the wait even with IRQ masked in the CPSR. *n* is the programmed interval, not a bound on the elapsed time: [measured](measurements.html#interrupts-and-sleep) waits overrun it by about 1 µs, and it is no use as a delay below about 7 µs.
