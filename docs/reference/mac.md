---
title: MAC
description: The XR819 MAC register window at 0x09c0_0000, grouped by function, with the mode, event and scheduler word layouts.
---
The MAC window at `0x09c0_0000` spans `0x2000` bytes but is sparse: the firmware uses more than a hundred of its offsets, grouped here by function. Offsets are written `+0x0e34`, relative to the window. Receive packet DMA, at `+0x0600`, has [its own page](rx-dma.html). Words that only the start-up image writes are listed under [Receive register image](#receive-register-image). Every register is also in the [register index](registers.html).

The Access column says how the firmware uses a register: W written, R read, RW both.

## Address match

Three address entries, each a low word, a high word and a mode word, decide which frames the MAC accepts and acknowledges. Four further entries at `+0x0060` are initialized disabled and not used.

{% include register-rows.html data=site.data.mac prefix="mac" section="match" %}

### Match mode words {#match-mode}

{% include registers/MatchMode.md %}

## Mode and filter

{% include register-rows.html data=site.data.mac prefix="mac" section="mode" %}

<div class="warning" markdown="1">
**The scan filter drops BlockAcks.** With the scan value in `FILTER`, packet DMA never delivers received compressed BlockAck frames, even though the AP sends them and TX status completes. A joined station must switch to the joined value.
</div>

### MODE

{% include registers/MacMode.md %}

## Packet DMA and rate RAM

{% include register-rows.html data=site.data.mac prefix="mac" section="dma" %}

## Response engine

The response engine sends descriptors that the firmware builds in packet RAM: beacons, ACK, CTS and BlockAck. It finds them through a table of 32 pointers.

{% include register-rows.html data=site.data.mac prefix="mac" section="response" %}

### Response commands

The descriptors the pointer slots point at are response commands in packet RAM. Like a transmit slot they start with three header words, then a command stream ([opcodes](rings.html#opcodes)). Times are in microseconds; *base* is the slot-time base and *const* the slot-time constant.

| Command | `+0x00` | `+0x04` | `+0x08` |
|---|---|---|---|
| Beacon (3) | <code>gap &#124; gap &lt;&lt; 16 &#124; 0xe000_e000</code>, gap = (16 × *base* + *const*) & `0x1fff` | `0xbc01_0000`: a fixed backoff | `0x5800_0000` |
| ACK, CTS (4–7) | <code>const &lt;&lt; 16 &#124; 0x2000_0000</code> | 0 | `0x5800_0000` |
| BlockAck (11, 12) | <code>const &lt;&lt; 16 &#124; 0x2000_0000</code> | 0 | `0x5800_0000` |
| Idle and TBTT (0–8, 11, 12 at start-up) | <code>n &#124; n &lt;&lt; 16 &#124; 0x2000_0000</code>, n = (*const* + 8 × *base*) & `0x1fff` | `0xdc01` in bits 31:16; the low half is not written | `0x8000_0000` |
{: .compact .nowrap-last}

The beacon's stream:

```text
0x51 rate word
0x50 PHY control
0x52 rate attribute, length + 4
0x32 frame control
0x32 0                         duration
0x29 header address
0x21 sequence word
0x0700_8400                    insert the timestamp
0x40 address of byte 32, length word
0x0700_4600
0xf000_0000
```

The ACK and CTS templates, from `+0x0c`:

```text
0x5900_0000
0x5800_0003
0x5a00_000e
0x3100_00d4 (ACK)  or  0x3100_00c4 (CTS)
0x4700_0000
0x20 interface metadata byte
0x0700_2008 (ACK)  or  0x0700_2004 (CTS)
0x0700_6828
0x0700_4600
0xf000_0000
```

The BlockAck template, from `+0x0c`:

```text
0x5900_0000
0x5800_0003
0x5a00_0020
0x3100_0094
0x4700_0000
0x20 interface metadata byte
0x3200_0000
0x6900_0000
0x0700_6820
0x6800_0000
0x6000_0000
0x0700_4600
0xf000_0000
```

### Response slot bitmaps

{% include registers/ResponseSlots.md %}

### BEACON_CONTROL

{% include registers/BeaconControl.md %}

### Pointer slots

Every slot starts pointing at response command 8, the idle descriptor; slots not listed keep it. The firmware installs:

| Slot | Response command | Descriptor |
|---|---|---|
| 0 | 3 | Beacon, while armed |
| 2, 3 | 11, 12 | BlockAck, interface 0 and 1 |
| 9, 10 | 6, 7 | CTS, interface 0 and 1 |
| 11, 12 | 11, 12 | BlockAck, interface 0 and 1 |
| 16 | 8 | Idle descriptor, installed when beaconing starts so that its bitmap bits are set |
| 21, 22 | 4, 5 | ACK, interface 0 and 1 |
{: .compact}

Pointers hold MAC packet offsets. Four things set the slots up:

- **Installing a pointer** in slot *n*, for slots 2 to 26, sets bit *n* − 2 in `RESPONSE_BITMAP_SET` and `RESPONSE_BITMAP_CLEAR` and clears it in `RESPONSE_STATE`. Slot 0 is written directly when the beacon is armed or disarmed; slots 27 and 28 are never installed.
- **Configuring a BlockAck slot** sets its bit in `RESPONSE_BITMAP_SET`, clears it in `RESPONSE_BITMAP_CLEAR` and `RESPONSE_STATE`, and writes 0 to `SLOT_FIELD_LOW` (slots up to 17) or `SLOT_FIELD_HIGH`. The order differs: slots 2 and 3 are configured, then installed, which sets their `RESPONSE_BITMAP_CLEAR` bit again; slots 11 and 12 are installed, then configured, which leaves it clear. Each is then ORed into `RESPONSE_ENABLE`.
- **The mode writes** set `RESPONSE_ENABLE` whole: the joined value enables the BlockAck, CTS and ACK slots, the scan value the CTS and ACK slots.
- **The beacon**, slot 0, is armed through `BEACON_CONTROL` and `BEACON_TIMER`, not through these bitmaps.

If a slot's `RESPONSE_BITMAP_SET` bit is clear, the MAC sends its descriptor from another pipe, which the peer does not expect.

## Event FIFO

The MAC reports transmit progress, received frames and pipe service requests as 32-bit words in an event FIFO. Its event line is the CPU's [FIQ](interrupts.html#the-fiq-line); the main loop drains the FIFO. The firmware drains the FIFO only while `EVENT_READINESS` bit 31 is clear: it pops one event, then keeps popping until the bit sets.

{% include register-rows.html data=site.data.mac prefix="mac" section="events" %}

### Event word

{% include registers/MacEventWord.md %}

### EVENT_READINESS

{% include registers/EventReadiness.md %}

## TSF and beacon timing

{% include register-rows.html data=site.data.mac prefix="mac" section="tsf" %}

### BEACON_TIMER

{% include registers/BeaconTimer.md %}

## Slot timing and EDCA

The firmware writes these together, with `SLOT_TIMING_614`, `SLOT_TIMING_618` and `SLOT_TIMING_810` listed under [packet DMA](#packet-dma-and-rate-ram). `SLOT_BASE`, `SLOT_X16` and `SLOT_X24` derive from the interface's slot-time base, 9 µs for a joined station; the others are constants.

{% include register-rows.html data=site.data.mac prefix="mac" section="slot" %}

## TX pipe scheduler

{% include register-rows.html data=site.data.mac prefix="mac" section="scheduler" %}

### PIPE_SCHEDULER

{% include registers/PipeScheduler.md %}

### PIPE_TRIGGER

{% include registers/PipeTrigger.md %}

## Control and debug

{% include register-rows.html data=site.data.mac prefix="mac" section="control" %}

## Receive register image

At start-up the firmware writes this fixed image, in this order, to configure reception; the functions of these words are unknown. It includes the 23 `TIMING` words from `+0x0210`. Two of them, `0x0400_0004` at `+0x0230` and `0x0400_0000` at `+0x0260`, look like DTCM addresses but are probably timing values <span class="tag inferred">inferred</span>: [other values slow association](measurements.html#image-loading-and-timing).

<details markdown="1">
<summary>Show the 75 writes</summary>

{% include registers/MacReceiveImage.md %}

</details>
