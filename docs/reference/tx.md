---
title: Transmitting a frame
section: Hardware reference
---
# Transmitting a frame

```
HIF request      firmware                                   MAC pipe                  air
    |                |                                            |                        |
    |-- 0x0004 ----->| retain context (frame borrowed)            |                        |
    |                | classify header, select key                |                        |
    |                |-- AES encrypt in place (optional) ------->|                        |
    |                | build command descriptor in packet RAM     |                        |
    |                |   words: 0x51, 0x50, 0x52, 0x32, 0x29,     |                        |
    |                |          0x07.., 0x40, 0x07.., 0xf0        |                        |
    |                | write descriptor flags                     |                        |
    |                |-- ring +0x0c base, +0x10 len, +0x14 GO=1 ->| fetch command          |
    |                |-- push airtime to ring +0x00 ------------->|                        |
    |                |                                            |-- transmit ----------->|
    |                |<-- event: start (type 3) ------------------|                        |
    |                |<-- event: success / status ---------------|<-- ACK/BA ------------|
    |                | resolve status, retry or retire            |                        |
    |                |-- build 0x0404 confirmation ------------->|                        |
    |<-- host reads --|                                            |                        |
```

### Publication checklist

1. **Reserve the slot** in the pipe registry (state 0), keeping the previous image so an abort can restore it.
2. **Build the descriptor** completely before any MAC-visible write. The descriptor lives in a packet-RAM command slot owned exclusively by that pipe and slot index.
3. **Set the descriptor flags word** and push the airtime word into the ring's duration FIFO.
4. **Write GO = 1** as the last write, after a write-buffer drain. The MAC fetching a partially written descriptor is not recoverable.
5. **Record the publication** (pipe, slot, context, frame address, command base) so that start, status and completion events can be matched. A completion for an unowned slot must be rejected before any dereference.
6. **Advance on the event FIFO**, not on the register readback: the pipe's cursor register can move before the event that describes the frame.

**Why the descriptor is not a plain packet buffer.** The MAC executes the command word stream. It can insert values (frame control, timestamps), source bytes (interface metadata), and split header from payload with different encodings, including a DMA bus address for the payload. That is what allows one descriptor template to serve beacons, ACK/CTS responses, BlockAcks and ordinary data.
