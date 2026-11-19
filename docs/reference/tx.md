---
title: Transmitting a frame
description: How a host frame becomes a command descriptor on an XR819 TX pipe, and how its completion returns to the host.
---
A transmit request from the host becomes a command descriptor in one of the four [transmit rings](rings.html). The MAC executes it, reports progress through the event FIFO, and the firmware answers the host with a TX confirmation.

```text
host               firmware                                   MAC pipe              air
 |                     |                                          |                    |
 |-- TX_REQ 0x0004 --->| keep the context; the frame stays in     |                    |
 |                     | the HIF input buffer                     |                    |
 |                     | classify the header, select the key      |                    |
 |                     | CCMP-encrypt in place on the AES engine, |                    |
 |                     | if protected                             |                    |
 |                     | build the command descriptor             |                    |
 |                     |-- trigger, duration FIFO, GO = 1 ------->|-- fetch command    |
 |                     |                                          |-- transmit ------->|
 |                     |<-- event: start -------------------------|                    |
 |                     |<-- event: completion and status ---------|<-- ACK or BA ------|
 |                     | resolve status: retry or retire          |                    |
 |<-- TX confirmation -| 0x041e or 0x0404                         |                    |
```

## Publishing a descriptor

A publication hands one command slot, or a batch of consecutive slots, of one pipe to the MAC.

1. **Reserve the slot** in the pipe registry and keep its previous state, so an abort can restore it.
2. **Build the descriptor completely** in the slot's packet-RAM command area before any write the MAC can act on.
3. **Record the publication**: pipe, slot, context, frame address and command base. Start, status and completion events are matched against this record, and a completion for a slot nobody owns is rejected before anything is dereferenced.
4. **Stage it:** set the descriptor flags, write the ring's `GO` = 0, and write `EDCA_AIFS` if the interface's value changed and the pipe's `PIPE_QUANTUM`. A batch repeats steps 1–4 for each slot.
5. **Publish:** write `PIPE_TRIGGER` for the pipe, then one airtime word per staged slot into the ring's `DURATION_FIFO`, then `GO` = 1, once.
6. **Advance on events, not registers.** The ring's cursor can move before the event that describes the frame arrives.

A retry does not repeat this: it sets the descriptor flags, writes `PIPE_TRIGGER`, and clears the slot's bit in the ring's [`PENDING` mask](rings.html#cursor_and_pending_mask).

The descriptor's [command words](rings.html#a-data-frame) set the rate, insert frame control and duration, point the MAC at the header and payload, and close the descriptor.

## Completion

A completion event carries a TX status in its low six bits (`COMPLETION` set). The firmware either retires the frame or keeps it for a retry.

Retired frames are confirmed with `MULTI_TX_CONFIRM` or `TX_CONFIRM`, depending on the path they took; [WSM messages](wsm.html#payloads) gives the rule and both layouts.

A watchdog checks each pipe every 200 ms. A pipe stuck on a batch completes each of its frames with give-up status `0x0b`, so the host learns about the failure instead of losing the frames.
