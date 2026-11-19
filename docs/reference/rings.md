---
title: Transmit rings
section: Hardware reference
---
# Transmit rings

Four identical rings, stride `0x80`. Ring *p* is at `0x09c6_0000 + p*0x80`. A pipe is started by pushing one airtime word into the duration FIFO and writing GO; the MAC then fetches the command descriptor from packet RAM, transmits, and reports start/completion/threshold events through the MAC event FIFO.

## Ring layout
{:#s9-1}

{% include registers/TxRing.md %}

The transmit path treats `opaque_04` as bytes. Startup also programs two words inside it: the command base and descriptor length. That range is not reserved.

| Offset | Name | Description | Evidence |
| --- | --- | --- | --- |
| 0x00 | DURATION_FIFO | Write one airtime word per publication, immediately before GO. | Observed |
| 0x0c | COMMAND_BASE | MAC packet offset of the pipe's command slot array (`tx_command(pipe, 0)`). | Observed |
| 0x10 | COMMAND_LENGTH | Command descriptor size, `0x54`. | Observed |
| 0x14 | GO | Write `1` to hand the prepared command to the MAC. Written `0` first when re-arming after a retry. | Observed |
| 0x18 | INACTIVE_SENTINEL | `0xff00_ffff` marks a pipe as retry-inactive. | Observed |
| 0x1c | COMPLETION_WORD | Per-pipe completion state. | Inferred |
| 0x20 | CURSOR_AND_PENDING_MASK | Bits 23:26 plus a 24-bit pending mask: the current command slot index is `(value & 0x07ff_ffff) >> 24`. | Observed |

**Initialization is once, before MAC core enable.** COMMAND_BASE, COMMAND_LENGTH and GO are programmed by `prepare_packet_dma()` in the vendor's `mac_hw_init_pipes` position, and the vendor never programs them again. Re-writing GO while the MAC is live disturbs a running command-fetch engine. Final register values are identical either way, so a bug caused by the extra write shows up only as behavioural difference, not as a value difference.

## Pipeline mapping
{:#s9-2}

Pipe index and 802.11 access category are not the same numbering. Both directions of the mapping are fixed:

| Pipe | Access category | Use |
| --- | --- | --- |
| 0 | 1 | AC_BE |
| 1 | 0 | AC_VO |
| 2 | 2 | AC_VI |
| 3 | 3 | AC_BK |

Each pipe owns four command slots of `0x54` bytes in packet RAM. Slot state is the firmware's own: reserved, published, started, transmitted, retry-owned, status seen.

## Command descriptor format
{:#s9-3}

A command descriptor is a stream of 32-bit words consumed by the MAC in order. Word `+0x00..+0x08` hold the airtime/sequence header; the command words start at `+0x0c` and end with the terminator `0xf000_0000`. Every word's top byte selects an operation class; the operand is carried in the lower bits.

| Opcode | Operand | Meaning | Conf. |
| --- | --- | --- | --- |
| 0x21 | 23-bit packet offset | Point at a duration word (used when the header duration is zero). | Observed |
| 0x20 | 23-bit packet offset, `0x2080_0000` form | Source one metadata byte that the MAC ORs into the high frame-control octet. | Observed |
| 0x29 | 23-bit packet offset | Frame header address (frame + 4). | Observed |
| 0x32 | 16-bit value | Insert a segment value: frame control, or duration when no operand. | Observed |
| 0x31 | control subtype | Emit a control frame (ACK `0xd4`, CTS `0xc4`). | Observed |
| 0x40 | bus address, `0x007f_fffc` mask | Payload DMA address. | Observed |
| 0x50 | PHY control bits | Finalise the PHY rate word for the frame length. | Observed |
| 0x51 | PHY rate word | Transmit rate and preamble selection. | Observed |
| 0x52 | rate attribute << 16 \| length+4 | Hardware rate class and frame length. | Observed |
| 0x58 | constant | Response/fixed-value segment (`0x5800_0003`). | Inferred |
| 0x59, 0x5a | constant | Control-frame PHY segments (`0x5900_0000`, `0x5a00_000e`). | Inferred |
| 0x47 | constant | Fixed macro command in every control-frame template. | Inferred |
| 0x60, 0x68, 0x69 | constant | BlockAck template segments. | Inferred |
| 0x07 | flag pattern | MAC fill/insert macro. `0x0700_8400` has the MAC insert the 8-octet timestamp (beacon, probe response); `0x0700_4600` closes a descriptor; `0x0700_2008`/`0x0700_2004` select an ACK/CTS response path; `0x0700_6828`/`0x0700_6820` arm BlockAck handling. | Observed |
| 0x65 | `cpu & 0x001f_fffc` | A-MPDU member transfer (see 9.4). | Observed |
| 0x66 | none | A-MPDU inter-member delimiter. | Observed |
| 0xe4 | none | A-MPDU terminal command. | Observed |
| 0xf0 | none | Descriptor terminator. | Observed |

The operation set is not fully decoded: the table lists classes whose use is confirmed in working descriptors. Do not assume an unlisted opcode is invalid, and do not assume a listed class has no other operands.

## A-MPDU command stream
{:#s9-4}

```
word 0        header: airtime / sequence words
word 1        ...
0x6500_0000 | (member_1_addr & 0x001f_fffc)   transfer member 1
0x6600_0000                                    delimiter
0x6500_0000 | (member_2_addr & 0x001f_fffc)   transfer member 2
0x6600_0000                                    delimiter
...
0xe400_0000                                    terminal
0xf000_0000                                    end of descriptor
```

The first member's transfer word has no spare per-transfer flag bits, so firmware cannot invalidate member one while preserving both operands, the member order and the aggregate shape. Changing the delimiter opcode instead invalidates the member *after* it: changing `0x66` to `0x67` produced only second-member loss, and changing the first transfer opcode to `0x64` prevented aggregation from becoming operational at all.
