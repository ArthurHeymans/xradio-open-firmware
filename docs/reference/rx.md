---
title: Receiving a frame
description: How a received frame travels from the XR819 RX FIFO to the host as a WSM receive indication.
---
Packet DMA writes each received frame into the [RX FIFO](rx-dma.html#fifo-slots). The firmware wraps it in a WSM receive indication, hands it to the host through the HIF output ring, and releases the FIFO slot once the host has read it.

```text
packet DMA                       firmware main loop                     HIF / host
    |                                   |                                    |
    |-- write slot at PRODUCER -------->|                                    |
    |   header, frame, trailer          |                                    |
    |-- advance PRODUCER -------------->|-- read PRODUCER                    |
    |                                   |-- check MAGIC and LENGTH           |
    |                                   |-- read frame and trailer           |
    |                                   |-- RECEIVE_IND in place ----------->|
    |                                   |                                    |-- host reads
    |                                   |<-- STATUS.TX_DONE -----------------|
    |                                   |-- STATE, MAGIC: released           |
    |<-- write CONSUMER ----------------|                                    |
```

## Claiming a slot

Each pass, the firmware compares its claim offset with `PRODUCER`. If they differ, it reads the slot header there:

1. **Normal claim.** If `MAGIC` is `0x00aa_55ff` and `LENGTH` is at least 4, at most [8178](rx-dma.html#size-limits), and within the bytes up to the producer, the slot is valid. The firmware computes the next slot's offset from the length, writes it to `NEXT`, clears all but the low byte of `STATE` to mark the slot claimed, and moves its claim offset there.
2. **Tail repair.** If `MAGIC` is wrong but the length fits and the computed next offset lands exactly on `PRODUCER`, where no magic is written yet, the header marker alone was damaged. The firmware writes `NEXT`, writes `MAGIC` at the producer as the new sentinel, and claims the slot as in step 1.
3. **Recovery scan.** Otherwise the slot counts as a receive error. The firmware steps forward one word at a time, up to the producer, looking for `MAGIC`. It accepts a candidate whose length is at least 4, fits the remaining bytes, and whose own next slot also starts with `MAGIC`. If the scan reaches the producer, the whole span is dropped.
4. **Skipping ahead.** After a recovery scan, the claim offset moves to the candidate, or to the producer, at once. `CONSUMER` follows immediately only if no frames are with the host and every claimed slot has already been released; otherwise the firmware writes it when the last outstanding frame comes back.

## Handing a frame to the host

The firmware builds the `RECEIVE_IND` message in place, in the 16 bytes before the frame, over the end of the slot header, so no frame data is copied. An output descriptor then points the host interface straight at the slot.

When the host has read it, the firmware releases the slot. Slots must go back to packet DMA in claim order:

- If the slot is the oldest claimed one, the release head, it is released at once: `STATE` becomes `0xcccc_cc00` with its low byte kept, `MAGIC` is cleared, and `CONSUMER` advances past it. Any following slots already marked pending are released with it.
- Otherwise it is only marked pending, `STATE` = `0xffff_ff00` with its low byte kept, and is released when the head reaches it.

## Rules

**Offsets wrap, slots do not;** see [Offsets and wrap-around](rx-dma.html#offsets-and-wrap-around).

**Release only what was claimed, in order.** Writing a `CONSUMER` value beyond the slots actually processed loses frames silently. Slots return to packet DMA in the order they were claimed; at most 24 slots are with the host at once.

## Wake-up

A received frame raises a MAC event with bit 7 set (see the [event word](mac.html#event-word)), which wakes a sleeping CPU through the [FIQ line](interrupts.html#the-fiq-line).
