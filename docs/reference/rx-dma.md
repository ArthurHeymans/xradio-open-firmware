---
title: Receive DMA
description: The XR819 receive packet DMA, its registers and the RX FIFO slot format.
---
Receive packet DMA is the only writer of received frames. It appends each frame to the RX FIFO in packet RAM, with a `0x20`-byte header in front and a receive-status trailer after it, and advances a producer offset. The firmware reads frames up to the producer and advances the consumer as it frees their slots. A slot whose frame is still on its way to the host cannot be reused.

## Registers

The block sits inside the MAC window, at `+0x0600`. Two slot-timing words, `SLOT_TIMING_614` and `SLOT_TIMING_618`, fall inside its range but are not part of this layout; see [MAC](mac.html#packet-dma-and-rate-ram).

{% include registers/RxDma.md %}

### CONTROL

{% include registers/RxDmaControl.md %}

## FIFO slots

Every slot starts with a `0x20`-byte header. The frame follows it directly and is padded to a word boundary; the receive trailer follows the padding.

{% include registers/SlotHeader.md %}

| Offset | Contents |
|---|---|
| `0x00` | Slot header, above |
| `0x20` | The 802.11 frame, `LENGTH` octets including the FCS |
| `0x20 + align4(LENGTH)` | Receive trailer: rate index at `+0x06`, RCPI at `+0x07` |
{: .compact}

`RX_TSF` was measured on beacons: it holds the TSF at the start of the frame, so a beacon's own timestamp is later by its airtime.

## Offsets and wrap-around

FIFO offsets are logical and wrap below `0x7000`. The storage is `0x8000` bytes: the extra `0x1000` let a slot that starts near the top of the ring continue linearly instead of wrapping. The CPU address of a slot is `0x0940_0000 + (offset % 0x7000)`.

Data beyond the producer is arbitrary. [Receiving a frame](rx.html#claiming-a-slot) says how the firmware decides that a slot is valid and recovers from one that is not.

## Size limits

| Limit | Bytes |
|---|---:|
| WSM message | `0x1ffe` (8190) |
| Frame, after 16 bytes of host headroom | 8174 |
| Slot length field, including the FCS | 8178 |

A slot must also fit in the bytes available and in the physical storage.

[Receiving a frame](rx.html) follows a frame from the FIFO to the host.
