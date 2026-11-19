---
title: Receive DMA and FIFO
section: Hardware reference
---
# Receive DMA and FIFO

Packet DMA is the only writer of received frames. It appends complete 802.11 frames, each prefixed by a 0x20-byte header and followed by a 4-byte-aligned trailer, into the RX FIFO ring and advances a producer offset. The firmware reads the producer, claims slots, publishes a consumer offset, and releases buffers back to the HIF.

## Register map
{:#s7-1}

{% include registers/RxDma.md %}

- **CONTROL**: Bit 0 ENABLE, bit 23 ACTIVE. Startup value `0x0102_0418`. ACTIVE stays set until a frame in flight completes after ENABLE is dropped.

- **PRODUCER**: FIFO offset just past the newest frame written.

- **CONSUMER**: FIFO offset up to which firmware has released frames. Writing this is the release.

- **CONFIGURATION_0C**: `0x80`. Written *before* CONTROL during initialization.

- **CONFIGURATION_1C**: `7`.

- **CONFIGURATION_20**: `0x3000_0000`.

### CONTROL bitfield

| Bit | Name | Meaning | Conf. |
| --- | --- | --- | --- |
| 0 | ENABLE | Packet DMA writes received frames into the RX FIFO. | Observed |
| 23 | ACTIVE | Still moving a frame. Clears after ENABLE is dropped. A bounded poll of this bit is the documented stop sequence. | Observed |

## FIFO slot layout
{:#s7-2}

Every slot starts with a 0x20-byte header. The frame follows immediately and is padded up to the next word boundary; the trailer follows at that word boundary.

| Offset | Field | Description | Conf. |
| --- | --- | --- | --- |
| 0x00 | MAGIC | `0x00aa_55ff` in a valid slot. A slot past the producer holds arbitrary data and must be validated by magic and length. | Observed |
| 0x04 | NEXT | Offset of the next slot. A slot whose next pointer is not the producer still has data behind it. | Observed |
| 0x08 | STATE | Release state word; firmware writes `0xcccc_cc00` when it hands a slot back. | Observed |
| 0x14 | RX_TSF | Low word of the MAC TSF when the frame began. Measured on beacons: the value is the beacon's airtime before processing. | Observed |
| 0x18 | LENGTH | Frame length in octets *including* FCS. | Observed |
| 0x20 | FRAME | The 802.11 frame itself. | Observed |
| 0x20 + align4(len) | TRAILER | Receive status. `+0x06` = rate index, `+0x07` = RCPI. | Observed |

Offsets are logical: they wrap below `0x7000`, and the FIFO keeps `0x1000` bytes of linear spill so that a slot starting near the top of the ring remains addressable. CPU access is always `rx_fifo_base + (offset % 0x7000)`. The WSM message ceiling is `0x1ffe` bytes. Subtract 16 bytes of host headroom for the frame limit (8174 bytes); the RX slot length may include another four FCS bytes (8178). The available bytes and physical storage must also cover the slot.

## FIFO header layout

{% include registers/SlotHeader.md %}

## DMA control field masks

{% include registers/RxDmaControl.md %}
