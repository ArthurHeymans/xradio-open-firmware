---
title: AES/CCMP engine
description: The XR819 AES engine at 0x09c5_0000, its command words, AAD format and transfer classes.
---
The AES engine at `0x09c5_0000` performs CCMP. Keys, the CCM context and the additional authenticated data (AAD) go through a FIFO; the frame payload moves through packet RAM by DMA. The engine handles one transfer at a time.

## Registers

{% include registers/AesRegisters.md %}

### COMMAND_STATUS

{% include registers/AesStatus.md %}

## Command words

Each stage writes its input words to `FIFO`, then a command to `COMMAND_STATUS`. An unchanged key need not be reloaded.

| Command | Stage | FIFO input |
|---|---|---|
| `0x1100` | Load key | Four words: the 16 key bytes, little-endian, in their WSM order |
| `0x1240` | CCM context | One 16-byte block: `[0] = 1`, `[1]` = TID, `[2..8]` = A2, `[8..14]` = PN5…PN0, `[14..16]` = payload length, big-endian |
| `0x1403` / `0x1402` | First AAD block | The first 16 bytes of the AAD stream; transmit / receive |
| `0x148b` / `0x148a` | AAD tail, 22-byte AAD | Three addresses, no QoS; TX / RX |
| `0x14ab` / `0x14aa` | AAD tail, 24-byte AAD | Three addresses, QoS; TX / RX |
| `0x14eb` / `0x14ea` | AAD tail, 28-byte AAD | Four addresses, no QoS; TX / RX |
| `0x140b` / `0x140a` | AAD tail, 30-byte AAD | Four addresses, QoS; TX / RX |
| `0x3008_1009` | Start encryption | Length already in `LENGTH`; encrypts in place when source and destination are equal |
| `0x3008_1008` | Start decryption | The firmware decrypts in place, with source equal to destination |
| `0x9000` | Initialize | After the 430-byte stream through `INIT_PORT` |

Every receive command is the transmit command minus one.

## AAD

The AAD is sent as a 32-byte stream: a 2-byte big-endian length, then masked frame control, A1, A2, A3, masked sequence control, [A4] and [QoS control], padded with zeros.

```text
FIFO  bytes 0..15 of the stream
      command 0x1403 (TX) / 0x1402 (RX)
FIFO  bytes 16..31 of the stream
      the AAD tail command for the header shape
      ... source, destination, length, then the start command
```

The tail commands' 22, 24, 28 and 30 bytes are the total AAD length for each header shape.

In the frame control the firmware clears Retry, Power Management and More Data; for non-management frames it also clears subtype bits 4–6, and for QoS data the Order bit, bit 15. It sets the Protected bit for management and data frames. In the sequence control it keeps the fragment number and clears the sequence number.

## Completion

Completion arrives on IRQ 18 or IRQ 20. For a decryption, `AUTH_OK` then reports the MIC check. The engine runs one transfer at a time, and the firmware queues the rest.

## Start-up self-test

Right after the host handshake the firmware initializes the engine and checks it:

1. Write `COMMAND_STATUS = 0` and wait for `BUSY` to clear.
2. Stream 430 vendor bytes to `INIT_PORT`, one byte per 32-bit write, with bit 31 set on the first.
3. Write `COMMAND_STATUS = 0x9000` and wait for `BUSY` to clear.
4. Run a fixed frame through CCMP in HIF output buffer 0 and compare the result with one computed at build time.

The result word is 1 on success, or `0xe0xx` naming the failed step. The format of the 430 bytes, and whether the engine needs them, are unknown; [Vendor initialization data](../init-data.html) records their origin.

[CCMP encryption](ccmp.html) gives the transfer sequence step by step.
