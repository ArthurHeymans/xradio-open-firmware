---
title: Programming checklists
description: "Register-level checklists for common XR819 operations: stopping and retuning the radio, beaconing, HIF output and A-MPDU completion."
---
Steps name the registers they write, with MAC registers given their offset in the MAC window; steps that run a larger routine link to it. For start-up, see the [operation overview](flows.html).

## Stopping the radio

When a scan ends without a joined interface. Wait until no transmission is in flight and no received frame is with the host, then:

1. `RADIO_STOP` (`+0x0a28`) = 0, `BEACON_CONTROL` (`+0x0a00`) = `0x1030_0000`, write the last mode value back to `RESPONSE_ENABLE` (`+0x0a04`), and point all 32 response pointer slots at the idle descriptor.
2. Clear `RxDma.CONTROL.ENABLE` and wait for `ACTIVE` to clear. A timeout of about 4 s is fatal.
3. Stop the PHY's current operation and write `PlatformControl.POWER_CONTROL` = `0x10`.
4. Discard the frames still in the RX FIFO, one per pass, until it is empty; then mark every interface record idle.

## Retuning to a channel

The scheduler returns at once, doing nothing, when the MAC is already on the requested channel with its receiver running. Otherwise:

1. Prepare the scan context and the MAC for the channel: `RATE_RAM_BASE` (`+0x0800`), `SECOND_PATH` (`+0x0270`) and `SECOND_PATH_ENABLE` (`+0x0314`).
2. Clear `RxDma.CONTROL.ENABLE` and wait for `ACTIVE` to clear.
3. Unless the PHY is already on this channel in the same mode with no calibration pending, run the full RF initialization: the PHY register lists; `POWER_CONTROL` = `0x11`, about 40 µs, then 1, if its bit 0 reads clear; the gain tables and the receive band.
4. Program the [channel PLL](phy.html#channel-pll).
5. Program the measurement timing and measure the temperature. Run a calibration if step 3 ran and the calibration cache asks for one. Enable AGC and program the channel's transmit power and gain.
6. Wait 120 µs.
7. Set `ENABLE` again and write the scan values to `RESPONSE_ENABLE`, `RESPONSE_MASK`, `FILTER`, `MODE` and `TIMING_310`.

When a station transmits again after a retune and the PHY's retained state says the MAC needs it, the firmware first rewrites `RATE_RAM_BASE`, the rate tables of every active record, the response descriptors and `MODE`, then writes 0 to `DEBUG_CONTEXT_4` to `DEBUG_CONTEXT_10` and finally to `DEBUG_CONTEXT` (`+0x1400`).

## Restoring the joined state

After retuning to the operating channel, a station returns to its joined configuration:

1. Program the interface's slot timings.
2. Reprogram the interface's rate tables.
3. Rebuild the ACK, CTS and BlockAck response descriptors and install their [pointer slots](mac.html#pointer-slots).
4. Write the BSSID: `BSSID_LOW`, `BSSID_HIGH`, then `BSSID_MODE` = `0x301`.
5. Write the joined values: `RESPONSE_ENABLE` = `0x0018_0783`, `RESPONSE_MASK` = `0x827b_ffdf`, `FILTER` = `0x0279_fe00`, `MODE` as computed in [`MODE`](mac.html#mode), and `TIMING_310` = `0x7800_0000`.

## Beacon start and stop

1. **Build** response command 3: its [header words and beacon stream](mac.html#response-commands).
2. **Start:** put the idle descriptor in pointer slot 16, and write `BEACON_AIRTIME` (`+0x0e94`) with the airtime of 24 octets at the beacon rate.
3. **Arm:** point slot 0 at response command 3; `BEACON_CONTROL` = `0x1030_0001`; `BEACON_TIMER` (`+0x0e08`) = `0x4000_0000 | (tbtt & 0x00ff_ffff)`.
4. **Disarm:** `BEACON_TIMER` = 0; `BEACON_CONTROL` = `0x1030_0000`; point slot 0 at the idle descriptor.

## Publishing one HIF output message

1. **Enqueue.** Write the message, header included, into a free output buffer, or for a received frame [into its RX FIFO slot](rx.html#handing-a-frame-to-the-host), and add it to the 64-entry software queue.
2. **Stage**, while fewer than four messages are on the ring: rewrite the ID with its 3-bit [sequence number](hif.html#sequence-numbers); write the descriptor's bus address and `CONTROL = ((length + 1) & 0x1ffe) | 1` with `SEQUENCE` from the ID as queued; advance the producer; drain the write buffer.
3. **Reclaim** on `STATUS.TX_DONE`, as described under [Ownership](hif.html#ownership), which stages the next queued message before releasing the buffer.

## Resolving an A-MPDU

1. On the slot's completion status, free the slot.
2. Find the compressed BlockAck in the RX FIFO and match it to the aggregate by peer, TID and starting sequence number.
3. Complete the members whose bitmap bit is set.
4. Put the others back at the head of the queue, oldest first. If the BlockAck does not come in time, treat every member as missing.
5. After two aggregate transmissions, send a still-missing member alone; the host's retry policy takes over.
