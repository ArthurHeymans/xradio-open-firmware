---
title: WSM messages
description: The WSM message IDs exchanged between the cw1200 driver and the XR819 firmware.
---
WSM is the message protocol between the `cw1200` driver and the firmware. The [host interface](hif.html) carries each message in one HIF buffer. Every message begins with a 4-byte header: a little-endian 16-bit length followed by a little-endian 16-bit ID.

## Message IDs

The tables list base IDs, without the sequence and link bits. A confirmation carries its request's ID with bit 10 set; indications have bit 11 set.

{% include registers/Messages.md %}

A confirmation and a later indication are separate messages: a successful scan confirmation does not mean the scan has finished.

In a request's ID, bits 9:6 carry the host's link ID and bits 15:13 its sequence number. A request with an ID the firmware does not support is answered with a generic confirmation carrying status 1, failure. Two kinds of ID are treated as transport faults instead: an ID with bit 10 or 11 set, which only firmware messages carry, is dropped without a reply, as is a message shorter than its header; an ID whose bits 12:0 are 0 stops the firmware with a fault record.

## MIB IDs

MIB IDs are a separate namespace, carried inside `READ_MIB_REQ` and `WRITE_MIB_REQ`. The firmware names these:

{% include registers/Mibs.md %}

## Payloads

The host driver defines the payloads, in [`wsm.h`](https://github.com/ArthurHeymans/linux/blob/bb557dc24a46/drivers/net/wireless/st/cw1200/wsm.h) and [`wsm.c`](https://github.com/ArthurHeymans/linux/blob/bb557dc24a46/drivers/net/wireless/st/cw1200/wsm.c) of the driver series. This page documents the data-path messages, the configuration request and the startup indication. Offsets count from the start of the message, header included.

| Offset | `TX_REQ` (`0x0004`) field |
|---|---|
| `0x00` | Header: length, ID |
| `0x04` | Packet ID, returned in the confirmation |
| `0x08` | Maximum transmit rate |
| `0x09` | Queue: 0 best effort, 1 background, 2 video, 3 voice. Selects the [TX pipe](rings.html#pipes-and-queues). |
| `0x0a` | More frames pending on the host |
| `0x0b` | Flags: expiry start, priority, retry policy; bit 7 moves the frame 2 bytes later |
| `0x0c` | Reserved |
| `0x10` | Expiry time, TU |
| `0x14` | HT parameters |
| `0x18` | The 802.11 frame, or at `0x1a` when flags bit 7 is set |
{: .compact}

Which message confirms a frame depends on the path it took. Data frames whose subtype carries data (subtype bit 2 clear, so not null-data) take the data path, unless they are unicast EAPOL frames, and are confirmed in `MULTI_TX_CONFIRM`, as many per message as fit, even when there is only one. Everything else, such as management frames, unicast EAPOL, null-data frames and BlockAck requests, takes the single-frame path and is confirmed with `TX_CONFIRM` (`0x0404`), as is a request the firmware rejects. Both confirmations come in two formats. The firmware uses the CW1200 format when the host's accepted `CONFIGURATION_REQ`, below, is padded with at least four bytes after the DPD block, as the driver at `bb557dc24a46` sends it; otherwise it uses the XR819 format, which adds three per-rate words. Before any `CONFIGURATION_REQ` it uses the XR819 format.

| XR819 offset | CW1200 offset | `TX_CONFIRM` field |
|---|---|---|
| `0x00` | `0x00` | Header: length, ID |
| `0x04` | `0x04` | Packet ID of the request |
| `0x08` | `0x08` | Status: 0 on success |
| `0x0c` | `0x0c` | Rate used |
| `0x0d` | `0x0d` | Transmissions without an acknowledgement |
| `0x0e` | `0x0e` | Flags |
| `0x10` | — | Attempts per hardware rate, three words of 4-bit counts; 0 in `TX_CONFIRM` |
| `0x1c` | `0x10` | Media delay, µs; the firmware writes 0 |
| `0x20` | `0x14` | Queue delay, µs; the firmware writes 0 |
{: .compact}

`MULTI_TX_CONFIRM` (`0x041e`) batches confirmations: the header, a 32-bit count at `0x04`, then that many entries from `0x08`, each laid out like a `TX_CONFIRM` from its `0x04` onwards, in the same format: 20 bytes for CW1200, 32 for XR819.

| Offset | `CONFIGURATION_REQ` (`0x0009`) field |
|---|---|
| `0x00` | Header: length, ID |
| `0x04` | Maximum transmit MSDU lifetime |
| `0x08` | Maximum receive lifetime |
| `0x0c` | RTS threshold; 0 disables RTS |
| `0x10` | DPD block length, in bytes from `0x10`; at least 12 |
| `0x12` | DPD version |
| `0x14` | Station MAC address |
| `0x1a` | DPD flags |
| `0x1c` | SDD calibration data, to the end of the DPD block |
{: .compact}

| Offset | `RECEIVE_IND` (`0x0804`) field |
|---|---|
| `0x00` | Header: length, ID; the ID carries the interface in bits 7:6 |
| `0x04` | Status: 0 on success |
| `0x08` | Channel number |
| `0x0a` | Receive rate index |
| `0x0b` | RCPI |
| `0x0c` | Flags: bits 2:0 are 3 when the firmware decrypted the frame with CCMP; bit 7 marks a beacon |
| `0x10` | The 802.11 frame, including CCMP header and MIC if it was protected |
{: .compact}

The firmware builds this message [in place](rx.html#handing-a-frame-to-the-host) in the RX FIFO.

## Startup indication

The first message the firmware sends, once the hardware is up, is `STARTUP_IND` (`0x0801`). Its field order is fixed by the host driver.

| Offset | Field | Value sent |
|---|---|---|
| `0x00` | Header | Length, ID |
| `0x04` | input_buffers | 30 |
| `0x06` | input_buffer_size | 1632 (`0x660`) |
| `0x08` | hardware_id | 7 |
| `0x0a` | hardware_sub_id | 9 |
| `0x0c` | status | 0; the driver rejects any other value |
| `0x0e` | firmware_capabilities | `0x8001`: bit 0, 2.4 GHz, and bit 15, peer HT. Bit 1, 5 GHz, stays clear: the XR819 has no 5 GHz radio. |
| `0x10` | firmware_type | 2; the driver rejects types above 4 |
| `0x12` | firmware_api | 1 |
| `0x14` | firmware_build | 1 |
| `0x16` | firmware_version | 1 |
| `0x18` | label | `XR819 open Rust native`, NUL-padded to 128 bytes |
| `0x98` | config | Four zero words |
{: .compact}
