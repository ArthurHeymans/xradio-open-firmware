---
title: Idle and sleep
description: When the XR819 firmware's CPU waits for an interrupt, and how a station's receiver dozes in 802.11 power save.
---
Two different things save power. The **CPU** waits for an interrupt whenever the main loop has nothing to do; this is invisible to the network. In **802.11 power save**, a station tells its access point that it sleeps and turns its receiver off between beacons.

## CPU sleep

The main loop sleeps with the ARM966E-S's wait for interrupt (`mcr p15, 0, r0, c7, c0, 4`). The core wakes when any source the [interrupt controller](interrupts.html) lets through is pending, whether or not IRQ is masked in the CPSR, and also on the MAC's [FIQ line](interrupts.html#the-fiq-line).

### When

Each owner exposes an activity fingerprint: HIF ring cursors, RX FIFO offsets and the frame being decrypted, AES transfer states, host TX context states and MAC events serviced. A pass of the loop that changes none of them is idle.

Once passes have been idle for 5 ms, every idle pass ends with a sleep. The sleep's programmed timeout is the time to the next timed work:

- the pipe watchdog, every 200 ms;
- beacon-loss detection;
- an ADDBA response, or a BlockAck an aggregate still waits for;
- the end of a scan dwell, or the next probe request;
- an access point's next beacon;
- a power-save deadline: the end of a doze, a null frame to retry, or the idle time before dozing.

While any work is still in flight, such as frames queued for the MAC, a frame being decrypted or held for reordering, or output the host has not read, the timeout is capped at 1 ms instead: not every step of that work raises an interrupt or names a deadline. A scan step that tunes, settles or stops the radio keeps the loop awake.

### How

With IRQ masked in the CPSR, the loop checks once more whether the IRQ handler has recorded a source. If not, it arms [timer 1](timers.html#programming) with the timeout and waits for interrupt. Afterwards it stops timer 1, acknowledges IRQ 6 and unmasks IRQ. A source that becomes pending after the check still ends the wait, and the handler records it once IRQ is unmasked.

| Wake source | Seen as |
|---|---|
| Timer 1, the timeout | IRQ 6 |
| A host message or a finished transfer to the host | IRQ 13 |
| An AES transfer completing | IRQ 18 or 20 |
| A received frame, or any other MAC event | The [FIQ line](interrupts.html#the-fiq-line) |
{: .compact}

An associated, idle board wakes about 20 times a second, for beacons and the watchdog. The 5 ms linger keeps the loop awake between the frames of a flow: sleeping between them [lost frames](measurements.html#sleeping-between-frames) with one host driver.

## Station power save

The host enables power save with `SET_PM_REQ`: mode 1 (legacy) or `0x81` (fast), with an idle time that defaults to 100 ms. The firmware records the mode, confirms the request and queues `SET_PM_COMPLETE_IND`; it does not wait for the access point. The indication reports `0x81` for either power-save mode, a legacy request included. Both modes then run as dynamic power save:

1. **Fall asleep.** After the idle time without traffic, send the AP a null-data frame with the power-management bit set. A null frame that gets no ACK is sent again after 20 ms.
2. **Doze.** While the AP considers the station asleep, nothing is in flight and no scan runs, turn the receiver off until 5 ms before a selected beacon: at most two beacon intervals ahead, never past a DTIM, and never past the beacon-loss deadline. Dozes shorter than 5 ms are skipped.
3. **Listen.** Bring the receiver back for the beacon; this takes about 3.6 ms. If its TIM says the AP holds frames for the station, send a null frame with the power-management bit clear. After a DTIM beacon that announces group frames, stay on until the last of them, for at most 10 ms. After a missed beacon, stay on until the next one.
4. **Wake.** The station's own data frames carry the power-management bit clear and wake it as well.

### Receiver off and on

| Step | Hardware |
|---|---|
| Off | Clear `RxDma.CONTROL.ENABLE` and wait for `ACTIVE` to clear; stop the PHY's current operation; `PlatformControl.POWER_CONTROL` = `0x10` |
| On | [Retune](sequences.html#retuning-to-a-channel) the operating channel through full RF initialization, and [restore the joined state](sequences.html#restoring-the-joined-state) as at the end of a scan |
{: .compact}
