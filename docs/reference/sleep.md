---
title: Waiting for interrupts
section: Hardware reference
---
# Waiting for interrupts

The firmware services interrupts but keeps FIQ masked. The MAC's event line is FIQ: it ends a wait-for-interrupt without ever appearing as a pending source.

```
_irq_handler (IRQ mode, IRQ+FIQ masked)
    pending = controller.pending
    controller.enable &= ~pending       mask every pending source
    controller.acknowledge = pending    clear them
    RAISED |= pending                   record for the owner

main loop
    owner takes its bit from RAISED, clears the cause in its device,
    then re-arms its source

idle path
    if nothing changed for 5 ms: arm timer 1 for the next deadline,
    mask IRQ, check once more, then WFI
```

Why the masking matters: a level source (HIF) cannot re-enter the handler before its owner ran, and a latched source (AES, timer) is not lost while its owner is busy. Main-loop state is never touched from the handler.

### Linger and wake policy

While work is in flight (frames queued for the MAC, mid-decryption, held for reordering, or output the host has not read) a sleep lasts at most 1 ms: not every step raises an interrupt or names a deadline. After 5 ms of idle passes the loop sleeps until the next timed work: the pipe watchdog every 200 ms, beacon loss, an ADDBA response timeout, a missing BlockAck, the end of a scan dwell, or the next probe delay. A scan that tunes, settles or stops the radio keeps the loop awake.

| Condition | Downlink loss (30 Mbit/s) | Note |
| --- | --- | --- |
| Sleep between frames, 1 ms bound, minimal host driver | 0.5 - 7 % | Worse with the write buffer drained before each wait |
| Same, 200 µs bound | 0.5 - 7 % | The bound does not matter, the wait itself does |
| Busy-wait on the same conditions | 0 - 0.1 % | No wait-for-interrupt at all |
| Sleep between frames, stack driver | 0.03 - 0.04 % | Vendor firmware: 0.01 - 0.04 % |
| Linger policy (current), stack driver | 0.03 - 0.06 % | Frames of a flow keep the loop awake |

An associated idle board wakes about 20 times per second for beacons and the watchdog, and sleeps about 99 % of the time.
