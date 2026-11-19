---
title: A-MPDU aggregation
description: How the XR819 firmware sends several frames as one A-MPDU and resolves the BlockAck.
---
An aggregate sends 2 to 16 frames of one BlockAck session in a single transmission of at most 4 ms. It occupies one command slot of a [transmit pipe](rings.html), like a single frame, and the recipient answers with one compressed BlockAck whose bitmap says which members arrived.

## Building an aggregate

The firmware takes members from the access category's queue while they fit in the session's window of 16, the airtime budget at the chosen HT rate, and the peer's minimum MPDU start spacing (MIB `0x1010`, 0–7). Each member keeps its own descriptor; the aggregate adds two command streams around them.

The **slot command**, in the pipe's command slot, holds what all members share:

| Offset | Word |
|---|---|
| `+0x00`–`+0x08` | [Slot header](rings.html#slot-header), written when the slot is staged; the first member expects the BlockAck |
| `+0x0c`–`+0x14` | PHY words `0x51…`, `0x50…` and `0x52…`; the last carries the aggregate's total length |
| `+0x18` | Transfer to the software record |
{: .compact}

The **software record**, one per pipe in packet RAM, transfers the members in order:

```text
0x6500_0000 | ((member 1 + 8) & 0x001f_fffc)     transfer member 1, from its length word
0x6600_0000                                      delimiter
[transfer into the spacing padding, when the peer asks for spacing]
0x6500_0000 | ((member 2 + 8) & 0x001f_fffc)     transfer member 2
...
0x6500_0000 | ((last + 8) & 0x001f_fffc)         transfer the last member
0xe400_0000                                      end of the aggregate
```

Every member but the last is followed by a delimiter. Members are marked for aggregation, which selects the aggregate PHY control word and keeps a retry at an HT rate. The first member stands for the slot: it expects the BlockAck, and its timing describes the whole aggregate.

## Completion

1. The MAC reports the slot's status once the BlockAck has arrived, or a failure. The firmware frees the slot.
2. The BlockAck itself is a received frame. The firmware finds it in the RX FIFO, possibly before or after the status, and matches it to the aggregate by peer, TID and sequence numbers. One BlockAck may answer more than one aggregate of the same session.
3. Members the bitmap marks as received complete normally. Missing members go back to the head of the queue, oldest first, for another aggregate.
4. If no BlockAck arrives in time, every member counts as missing.
5. A member gets two aggregate transmissions. After that it goes out alone, under the host's retry policy, which steps the rate down and finally gives up.

## The first member

A delimiter controls the boundary before the member that follows it, and a transfer word has no spare flag bit, so the first member cannot be invalidated by itself without breaking the aggregate; see the [perturbation experiments](measurements.html#a-mpdu-command-stream). Losing the first member is therefore not controllable from software <span class="tag inferred">inferred</span>.
