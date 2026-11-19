---
title: Programming sequences
section: Hardware reference
---
# Programming sequences

These are the sequences as checklists. Each step is a register write or a bounded poll; anything that must be quiescent is marked.

## Cold bring-up
{:#s13-1}

1. Apply the five loader groups (4 copies + 41 MMIO pairs).
2. Clear firmware `.bss`; initialize boot state (startup mode 1, no remap windows).
3. `0x0ac8_00bc`: clear bit 0, set bit 23.
4. Sample and relocate the eight remap windows; run the memory probe and select startup mode.
5. Start timer 0 (free-running), configure timer 1; reset the interrupt controller; mask clock sources.
6. Drain; program `system_control`; set clock masks; `shared_pointer`, `power_control`, `configuration |= 0x0012_0000` when applicable.
7. `clock_reset.control_04 = 0x200` (stack must already be in low SRAM).
8. `parameter_a0 = 31`, `parameter_9c = 6`, poll `ready_status` bit 0, write `clock_flags`.
9. HIF: publish the descriptor ring, enable IRQ 13, ack and set the length mask; the vendor routine waits for startup bit 10 to clear.
10. Register IRQs 6, 18, 20; later 26.
11. Packet DMA initialization ([Starting packet DMA](bringup-dma.html)), including the response list and MAC receive image.
12. Enable the packet controller, build and send the startup indication.
13. Enable IRQ/FIQ, apply the routing words, enter the scheduler.

## Channel change
{:#s13-2}

1. Stop the radio: `0x0a28 = 0`, `0x0a00 = 0x1030_0000`, restore the beacon selector, point every response pointer slot at the idle descriptor.
2. Drain RX in software; PHY command 7 for the new channel; program `phy_set_band_reg`.
3. Publish the rate-RAM base at `0x09c0_0800`; reprogram the rate tables of every active record; rebuild and reinstall the immediate response descriptors.
4. Write the mode word `0x0200`, then the filter/mask registers of [MAC registers](mac.html#s8-2).
5. Restore slot timings (a joined station uses a 9 µs base; the scan record's timing word is zero).
6. Save the debug context at `0x1400..0x1410`, base last.

## Beacon start and stop
{:#s13-3}

1. `build_beacon_descriptor()`: response command 3 gets the gap word, fixed backoff, PHY words, frame control, payload address, timestamp macro, terminator.
2. `start_beaconing()`: put an idle descriptor in pointer slot `0x10`; write `0x0e94` with the airtime of 24 octets at the beacon rate.
3. `arm_beacon(tbtt)`: pointer slot 0 = response command 3, `0x0a00 = 0x1030_0001`, `0x0e08 = 0x4000_0000 | (tbtt & 0x00ff_ffff)`.
4. `disarm_beacon()`: `0x0e08 = 0`, `0x0a00 = 0x1030_0000`, pointer slot 0 = idle descriptor.

## Publishing one HIF output message
{:#s13-4}

1. Check status bit 1 was seen since the last drain; confirm ring capacity (`producer - consumer < 4`).
2. Write the message length and id into the buffer; rewrite the id with the two sequence bits.
3. Write the descriptor: bus address, control = `((len+1) & 0x1ffe) | 1` plus sequence.
4. Advance the producer.
5. On status bit 1: for each descriptor whose control bit 0 is clear, in order, re-stage the next message *before* releasing the completed buffer.

## Selective A-MPDU retry
{:#s13-5}

1. Validate the retained aggregate identity (two exact frame-node addresses, command base, pipe, slot).
2. Decode the compressed BlockAck bitmap; members it rejects become retry candidates.
3. For each candidate: validate the same identity again, build the transfer word, and re-arm the command mask bit.
4. Re-publish with a fresh duration word and GO; count the retry against that pipe-slot key, not a global counter.
5. On exhaustion, retire the member to a give-up pair and release its command mask.
