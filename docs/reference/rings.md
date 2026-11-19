---
title: Transmit rings
description: The four XR819 MAC transmit rings, the pipe-to-access-category mapping and the command descriptor format.
---
The MAC has four transmit pipes, each with a small register ring at `0x09c6_0000 + 0x80 × pipe`. The table gives pipe 0's addresses. To transmit, the firmware builds a command descriptor in packet RAM, pushes one airtime word into the ring's duration FIFO and writes `GO`. The MAC fetches and executes the descriptor, then reports start, completion and threshold events through the [event FIFO](mac.html#event-fifo).

## Ring registers

{% include registers/TxRing.md %}

Start-up programs the rings twice, both times before any frame is sent. [Packet-DMA start-up](bringup-dma.html) writes `COMMAND_BASE`, `COMMAND_LENGTH` and `GO` = 1 for every pipe and then writes `COMMAND_FETCH` = `0xbf`. After the MAC core starts, the [MAC start-up](bringup-dma.html#mac-start-up) repeats it: `COMMAND_FETCH` = 0, the scheduler words, the four rings again, and `COMMAND_FETCH` = `0xbf`.

During operation the firmware writes `GO` = 0 as it stages each descriptor and `GO` = 1 once per publication, after the duration words; [Transmitting a frame](tx.html#publishing-a-descriptor) gives the full order. Retries and recovery write `INACTIVE_SENTINEL` and the cursor word instead.

<div class="warning" markdown="1">
**Do not reprogram the rings while frames are in flight.** Untested: rewriting a ring's base and length, a `GO` = 1 outside a publication, or writing [`COMMAND_FETCH`](mac.html#mac-command-fetch) from 0 to `0xbf` may disturb a running fetch engine without changing any register value <span class="tag hypothesis">hypothesis</span>. See [Open questions](unknowns.html).
</div>

### CURSOR_AND_PENDING_MASK

{% include registers/RingCursor.md %}

## Pipes and queues

Pipe *n* serves the host's WSM queue *n*, selected by the queue field of each [`TX_REQ`](wsm.html#payloads). The firmware also keeps an access-category index per frame, in increasing priority order, which selects the EDCA parameters.

| Pipe and WSM queue | Access category | Firmware AC index |
|---:|---|---:|
| 0 | Best effort | 1 |
| 1 | Background | 0 |
| 2 | Video | 2 |
| 3 | Voice | 3 |
{: .compact}

Each pipe owns four command slots of `0x54` bytes in packet RAM. The slot states, reserved, published, started, transmitted, retry-owned and status seen, are the firmware's own bookkeeping.

## Command descriptors

A command slot holds a three-word header followed by a command stream: 32-bit words that the MAC executes in order, ending with `0xf000_0000`. The top byte of each command selects the operation and the low bits carry its operand.

### Slot header

The firmware writes the header when it stages the slot:

| Offset | Word |
|---|---|
| `+0x00` | 0 |
| `+0x04` | Random backoff, masked with the contention window, in bits 21:10; plus `0x80` + the expected response class when the frame expects a response |
| `+0x08` | `0xd800_0000` with the response timeout in bits 12:3 and bit 13 set, when a response is expected; otherwise `0xdc00_0000` |
{: .compact}

The response timeout is the slot-time sum the firmware computes for the frame's rate, in microseconds. The response engine's descriptors use [their own header words](mac.html#response-commands).

### Opcodes

The table lists the opcodes in the firmware's descriptor builders, with the operands seen; it is not a complete decoding.

| Opcode | Operand | Operation | Used in |
|---|---|---|---|
| `0x07` | Flag pattern | Fill or insert macro: `0x0700_8400` inserts the 8-octet timestamp; `0x0700_4600` closes a descriptor; `0x0700_2008` and `0x0700_2004` select the ACK and CTS response paths. `0x0700_6828` (ACK and CTS templates) and `0x0700_6820` (BlockAck template) have unknown effect. | All |
| `0x20` | `0x80_0000` plus a MAC offset | Source one interface metadata byte, which the MAC ORs into the high frame-control octet | Data and host control frames, RTS, response templates |
| `0x21` | MAC offset | Take the sequence control from a packet-RAM word instead of the frame, when the firmware numbers the frame <span class="tag inferred">inferred</span> | Data frames, beacons |
| `0x26` | MAC offset | Address fields of the protected data frame, its address + 4 | RTS |
| `0x29` | MAC offset | Frame header address, frame + 4 | Data frames, beacons |
| `0x31` | 16-bit frame control; `0xd4` ACK, `0xc4` CTS, `0xb4` RTS, `0x94` BlockAck | Insert the frame control | Data and host control frames, templates, RTS |
| `0x32` | 16-bit value | Insert a value: a duration or NAV, the frame's own sequence control, or a beacon's frame control | All |
| `0x33` | Three address octets | Insert half of the own MAC address | CTS-to-self |
| `0x40` | <code>cpu &amp; 0x007f_fffc</code>, the word-aligned MAC offset | Payload DMA address; followed by a word holding the length in bits 23:12 and the byte offset in bits 1:0 | Data and host control frames, beacons |
| `0x47` | Frame-control flags octet | Insert the flags octet | Data and host control frames, RTS, CTS-to-self, templates (0) |
| `0x50` | PHY control bits | Finish the PHY rate word for the frame length | Data frames, beacons, RTS |
| `0x51` | PHY rate word | Transmit rate and preamble | Data frames, beacons, RTS |
| `0x52` | <code>(rate attribute &lt;&lt; 16) &#124; (length + 4)</code> | Hardware rate class and frame length | Data frames, beacons, RTS |
| `0x58`, `0x59`, `0x5a` | Constant | Fixed segments of the response templates: `0x5800_0003`, `0x5900_0000`, and `0x5a00_000e` (ACK, CTS) or `0x5a00_0020` (BlockAck) <span class="tag inferred">inferred</span>. `0x5800_0000` is also the third header word of response commands. | Templates |
| `0x60`, `0x68`, `0x69` | — | BlockAck template segments <span class="tag inferred">inferred</span> | BlockAck template |
| `0x65` | `cpu & 0x001f_fffc` | Transfer: an A-MPDU member, or the aggregate's software record | Aggregates |
| `0x66` | — | Delimiter after an A-MPDU member | Aggregates |
| `0xd8` | Response timeout | Slot header word for a frame that expects a response | All slots |
| `0xdc` | — | Slot header word for a frame that expects no response | All slots |
| `0xe4` | — | End of the A-MPDU | Aggregates |
| `0xf0` | — | End of the descriptor | All |

### A data frame

```text
+0x00 .. +0x08                 slot header
0x51 rate word
0x50 PHY control
0x52 rate attribute, length + 4
0x31 frame control             (Retry, bit 11, set when the frame's TX context is a retry)
0x47 frame-control flags
0x20 interface metadata byte
0x32 ACK duration
0x29 header address
0x21 sequence word  or  0x32 sequence control
[0x0700_8400]                  probe responses of 32 bytes or more: insert the timestamp
[0x40 address, length word]    the bytes after the first 24, or 32 for such a probe response
0x0700_4600
0xf000_0000
```

The `0x29` header segment always covers the fixed 24-byte header, so a fourth address, QoS and HT control go in the `0x40` segment with the body. That segment is present whenever the frame is longer than the header.

A control frame from the host stops after the `0x20` word with `0x40` and a length word for everything after its frame control, then `0x0700_4600` and `0xf000_0000`. Beacons and the response templates have [their own streams](mac.html#response-commands). Because the MAC executes commands rather than copying a buffer, it can insert values such as timestamps and fetch the header and payload separately, so one mechanism serves beacons, responses, BlockAcks and data.

## Aggregates

An aggregate uses opcode `0x65` twice: the slot command transfers to a software record, and the record transfers each member, with `0x66` delimiters between them and `0xe4` at the end. [A-MPDU aggregation](ampdu.html#building-an-aggregate) gives both streams.
