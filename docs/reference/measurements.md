---
title: Hardware measurements
description: Experiments on XR819 hardware that settled decisions in the firmware.
---
These board experiments settled decisions in the firmware; the reference pages state the resulting rules and link here for the evidence. Most ran on an Orange Pi Zero with throwaway images that wrote register values into the counters table (MIB `0x100c`) during start-up or from the main loop. Run lengths and exact revisions were not always recorded.

## Interrupts and sleep

| Experiment | Result | Decision |
|---|---|---|
| Timer 1 waits of 1,000, 20,000 and 5 µs | Lasted 1,001, 20,001 and 7 µs | Timer 1 bounds sleeps; it is no delay primitive below about 7 µs |
| Sleeps while the RX producer moved | Of 3,707, 3,681 ended early with no controller source pending, 9 ended early with one pending, and 17 ran their full length while a frame arrived | The MAC's FIQ wakes the CPU for received frames, invisibly to the controller |
| Every interrupt source enabled | Sources 9, 24 and 28 stay pending; acknowledging 28, or every source, stopped reception within seconds | Leave 9, 24 and 28 disabled; drain the MAC event FIFO from the main loop |
| UART transmit interrupt (source 0) enabled | Ended every wait at once | Leave it disabled |
| Pending sources after a pass under traffic | 13 (HIF) and 20 (AES), both cleared by their owners; 26 never | IRQ 26 needs no handling while the TSF is not adjusted |
| Replaying the nine interrupt routing words | No measurable change | The firmware does not write them |
| Reading the interrupt controller and timer registers through a host driver's debugfs `ahb` file, with the CPU halted | All read 0, the free-running clock included | Vendor settings cannot be read back this way |
| Fixed 1 ms sleep bound, associated and idle | 1,000 wakes a second | Sleep until the next deadline: about 20 wakes a second |
{: .compact}

## Sleeping between frames

Downlink loss at 30 Mbit/s with two host-driver builds used during development, recorded only as the "minimal" driver and the "stack" driver; which revisions they were cannot be recovered, so these results cannot be reproduced exactly.

| Policy | Host driver | Loss |
|---|---|---:|
| Sleep between frames, 1 ms bound | Minimal | 0.5–7 % |
| Same, with a 200 µs bound, or with the write buffer drained before each wait | Minimal | 0.5–7 % |
| Wait awake on the same conditions | Minimal | 0–0.1 % |
| Sleep between frames | Stack | 0.03–0.04 % |
| Vendor firmware | Stack | 0.01–0.04 % |
| Stay awake while a flow is active (current) | Stack | 0.03–0.06 % |
{: .compact}

Waiting awake on the same wake conditions cut the loss to 0–0.1 %, which points at the wait for interrupt itself rather than a missed wake <span class="tag inferred">inferred</span>. Why only the minimal driver is affected is an [open question](unknowns.html).

## A-MPDU command stream

Each change was made to an otherwise working aggregate.

| Change | Result | Decision |
|---|---|---|
| Delimiter `0x6600_0001` | Normal operation | The delimiter's low bit is ignored |
| Delimiter `0x6700_0000` | Only the second member was lost: 52 selective retries of it and 8 terminal failures in 5 s | A delimiter controls the boundary before the member that follows it |
| First transfer word `0x6400_0000` | Aggregation never became operational; 8 host buffers were not returned | The first member cannot be invalidated alone |
{: .compact}

## Image loading and timing

| Experiment | Result | Decision |
|---|---|---|
| The runtime repacked as three records: a `0x7500`-byte copy at 0, a `0x2218`-byte zero fill, an entry record | Healthy WSM idle state, no leaked buffers, normal scan traffic | The loader rebuilds the runtime from records; the host-visible file need not match the memory image |
| Runtime started without the five PHY load groups | Booted, then failed in ways that looked like MAC or calibration bugs | The bootloader [applies them](boot.html#phy-initialization) |
| MAC `+0x0230` and `+0x0260` written with anything but `0x0400_0004` and `0x0400_0000` | Association took 13–41 s instead of about 2 s | Keep these values. They look like DTCM addresses but act like timing values <span class="tag inferred">inferred</span> |
| Stack in high SRAM across the `CONTROL_04` write | Failed at that write, looking like a packet-memory fault | [Keep the stack in DTCM](clock.html#the-control_04-transition) |
{: .compact}
