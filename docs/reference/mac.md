---
title: MAC registers
section: Hardware reference
---
# MAC registers

The tables below describe register behavior, including vendor observations. For the literal offsets selected by the current Rust MAC routines, see the [source inventory](mac-access.html). Computed slot addresses are not enumerated there.

The MAC aperture is sparse. Offsets below are the ones the vendor image and the open firmware actually touch; unused holes exist between them. Absolute address is `0x09c0_0000 + offset`.

## Address match and interface identity
{:#s8-1}

| Offset | Name | Observed programming | Conf. |
| --- | --- | --- | --- |
| 0x0030 | OWN_MAC_LOW | Station address octets 0..3, little-endian. Written twice (two address entries) together with the registers below. | Observed |
| 0x0034 | OWN_MAC_HIGH | Station address octets 4..5. | Observed |
| 0x0038 | OWN_MAC_MODE | `0x101` for the own-address entry. | Observed |
| 0x003c | BSSID_LOW | BSSID octets 0..3 (station role). | Observed |
| 0x0040 | BSSID_HIGH | BSSID octets 4..5. | Observed |
| 0x0044 | BSSID_MODE | `0x301` for a station (mode 3), `0x101` for AP/IBSS (mode 1). The vendor passes mode 3 for a station; with mode 1 the MAC answers the AP's A-MPDUs from an unused receive BlockAck pipe. AP role clears this register to 0 (own address only). | Observed |
| 0x0048 | SECOND_ADDR_LOW | Second copy of the own address, for the second interface slot. | Observed |
| 0x004c | SECOND_ADDR_HIGH | As above. | Observed |
| 0x0050 | SECOND_ADDR_MODE | `0x101`. | Observed |
| 0x0060 + n*0x0c | MATCH_ENTRY[n] | Four entries initialised to `len = 0xffff_ffff`, `+4 = 0xff`, `+8 = 0xff` (disabled). | Observed |

## Mode, filter and beacon control
{:#s8-2}

| Offset | Name | Observed programming | Conf. |
| --- | --- | --- | --- |
| 0x0200 | MODE | Main MAC mode word. Scan path: `0x07f3_bd5e`. Joined station: built from `0x07e3_b85c` OR per-active-record path masks (`0x0008_0281` or `0x0010_0502`) plus `0x4000` per record in mode 2. | Inferred |
| 0x0204 | FILTER | Scan path `0x0019_8000`; joined station `0x0279_fe00`. The scan value excludes received compressed BlockAck frames from packet DMA even though the AP transmits them and TX status completes, which is why the joined path must use the `mac_program_mode_regs` value. | Observed |
| 0x0210 + n*4 | CONTROL_WORDS | 23 words cleared during startup. | Observed |
| 0x0270 | SECOND_PATH | `0x0100_0000` or `1 << 26` depending on the second active record's path selector. | Observed |
| 0x0308 | RECEIVE_SETUP_308 | `0x70`. | Observed |
| 0x0310 | TIMING_310 | `0x7800_0000` in both scan and joined modes. | Observed |
| 0x0314 | SECOND_PATH_ENABLE | `0x0100_0000` when a second active record shares the channel, else 0. | Observed |
| 0x02ec | RECEIVE_SETUP_2EC | `4`. | Observed |
| 0x0800 | RATE_RAM_BASE | MAC packet offset of the rate RAM object. Reprogrammed after every channel change. | Observed |
| 0x080c | PACKET_DMA_CFG | Cleared during packet-DMA preparation; semantics not recovered. | Hypothesis |

## Response engine: pointers, slots and beacons
{:#s8-3}

The response engine sends descriptors that firmware places in packet RAM, addressed through a 32-entry pointer table. Slot numbering is fixed by the vendor (table below) and each slot is enabled by a bit in the pipe-enable bitmaps.

| Offset | Name | Description | Conf. |
| --- | --- | --- | --- |
| 0x0a00 | BEACON_CONTROL | `0x1030_0000` idle; `0x1030_0001` arms the beacon descriptor in pointer slot 0. | Observed |
| 0x0a04 | BEACON_SELECTOR | Also the response-slot enable bitmap. Scan `0x0018_0180`, joined station `0x0018_0783`. Bit `n` enables response slot `n+2`. | Observed |
| 0x0a08 | RESPONSE_BITMAP_SET | OR in bit `n` to mark slot `n+2` as having a programmed response. Keeping the programmed slots set is *required* for the MAC to recognise their response path. | Observed |
| 0x0a0c | RESPONSE_BITMAP_CLEAR | AND-mask; bits cleared here disable the slot's alternate path. | Observed |
| 0x0a10 | RESPONSE_COUNT | 24-bit count per slot, AND-masked to `0x00ff_ffff`. | Observed |
| 0x0a14 | SLOT_FIELD_LOW | Cleared for slots 2..0x11 after programming. | Observed |
| 0x0a18 | SLOT_FIELD_HIGH | Cleared for slots above 0x11. | Observed |
| 0x0a1c | RESPONSE_MASK | `0x827b_ffdf`. | Observed |
| 0x0a20 | MAC_EVENT_FIFO | Pop one 32-bit event word. The first pop after an event line is unconditional. | Observed |
| 0x0a24 | EVENT_READINESS | Reads negative (bit 31) while more events are queued. This bit gates the remaining pops. | Observed |
| 0x0a28 | RADIO_STOP | Cleared in the RX-stop path. | Inferred |
| 0x0c00 | RESPONSE_POINTER_BASE | Packet offset of the 32-entry response pointer table. | Observed |
| 0x0e08 | TBTT | Low 24 bits of the target beacon transmission time, with bit 30 select. `0` disarms. | Observed |
| 0x0e30 | SLOT_BASE_REG | `base * 8 - 1`. | Observed |
| 0x0e34 | TSF_CONTROL | Written `0` then `3` (a `0 → 3` edge; writing 3 directly is not equivalent if the register has sticky semantics). IRQ 26 handler clears `0x1f00_0003`. | Observed |
| 0x0e38 | TSF_LOW | Low 32 bits of the 64-bit microsecond TSF. | Observed |
| 0x0e3c | TSF_HIGH | High 32 bits. Read high, low, high again and retry on change. | Observed |
| 0x0e58 | SLOT_X16 | `base * 16 + 2`. | Observed |
| 0x0e5c | SLOT_X24 | `base * 24 + 2`. | Observed |
| 0x0e60 | PIPE_COUNT | `2`. | Observed |
| 0x0e64 | EDCA_AIFS | Packed AIFS values for the active record. Only written while the MAC is quiescent. | Observed |
| 0x0e70 + pipe*4 | PIPE_QUANTUM[pipe] | TX quantum, written as `(quantum + 0x1f) >> 5`. | Observed |
| 0x0e84 | PIPE_IRQ_PENDING | Scheduler word: low bits select the pipe to serve, nibbles 4..7 and 12..15 select service phases. Written with per-pipe ones-complements to acknowledge. | Observed |
| 0x0e80 | PIPE_CFG_80 | `0x100`. | Observed |
| 0x0e88 | PIPE_CFG_88 | `0xff`. | Observed |
| 0x0e8c | COMMAND_FETCH | `0xbf` enables the command-fetch engine. Must only be written while the MAC is quiescent: cycling it under a running fetch engine disturbs fetch state even though the final value is identical. | Inferred |
| 0x0e94 | BEACON_AIRTIME | Airtime of a 24-octet frame at the beacon rate, with a small per-configuration adjustment. | Observed |
| 0x0e98 | PIPE_IRQ_TRIGGER | Writing `(1 << pipe) << 25` signals the MAC that firmware serviced the pipe. | Observed |
| 0x0e00 | TIMING_BLOCK | `0x4f00_004f` then `0x0909_0000`, then offsets `0x08..0x2c` cleared. | Observed |
| 0x1400..0x1410 | DEBUG_CONTEXT | Saved/restored register context: `+0x04`, `+0x08`, `+0x0c = 0x7ff`, `+0x10`, and the base at `+0x00` written *last*. | Observed |

### Response pointer slot assignment

| Slot | Descriptor | Slot | Descriptor |
| --- | --- | --- | --- |
| 0 | Beacon (pointer to response command 3) | 2, 3 | Immediate ACK, interface 0 / 1 |
| 4, 5 | ACK response descriptors, interface 0 / 1 | 6, 7 | CTS response descriptors, interface 0 / 1 |
| 8 | Idle descriptor used as the default pointer | 0x0b, 0x0c | BlockAck response slot, interface 0 / 1 |
| 9 + n | CTS-to-self, interface *n* | 0x15 + n | ACK, interface *n* |
| 0x10 | Idle descriptor installed when beaconing starts | 0x1b, 0x1c | Alias slots mapped to logical 30 / 31 |

Pointers are published as MAC packet offsets (23-bit form). The slot's enable bit and its `+0x0a08` bitmap entry must be programmed together; clearing the bitmap entry makes the MAC send the descriptor from a different, unused pipe.

## EDCA and slot timing block
{:#s8-4}

`program_slot_timings()` writes a fixed group derived from the interface's slot-time base. Values are exact from the vendor path:

| Offset | Value | Offset | Value |
| --- | --- | --- | --- |
| 0x0618 | 0x0000_004d | 0x0614 | 0x0000_01cd |
| 0x0428 | 0x0000_049d | 0x042c | 0x0000_049d |
| 0x0444 | 0x0000_0258 | 0x0434 | 0x0000_003d |
| 0x0438 | 0x0000_003d | 0x0448 | 0x0000_0258 |
| 0x043c | 0x0000_0298 | 0x0440 | 0x0000_0298 |
| 0x044c | 0x0000_0134 | 0x0810 | 0 |

A joined station also restores a 9 µs slot-time base after a synthetic scan transition; the scan record's timing word is zero in the open runtime.
