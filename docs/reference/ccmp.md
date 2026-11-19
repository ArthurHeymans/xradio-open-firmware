---
title: CCMP encryption
description: One CCMP transfer through the XR819 AES engine, step by step.
---
The [AES engine](aes.html) encrypts transmitted frames and decrypts received frames, both in place: only the payload between the CCMP header and the MIC changes, and a received frame reaches the host with its CCMP header and MIC still in it. One transfer at a time runs device-wide.

## Sequence

```text
classify the frame
    |-- wait for BUSY clear before every command; later stages also wait for FIFO_READY
    |-- if the key is not loaded: 4 FIFO words (16 key bytes), then 0x1100
    |-- 16-byte CCM context block, then 0x1240
    |-- AAD: 2-byte big-endian length and the first AAD bytes, then 0x1403 (TX) / 0x1402 (RX)
    |-- remaining AAD words, then the tail command for the header shape
    |-- SOURCE, DESTINATION, LENGTH
    |-- write barrier
    |-- start: 0x3008_1009 (TX) or 0x3008_1008 (RX)
    |
    |<-- IRQ 18 or IRQ 20
    |-- read COMMAND_STATUS: for decryption, AUTH_OK is the MIC check
    |-- start the next queued operation
```

## Invariants

- One transfer at a time, device-wide.
- Key, AAD and payload stay in place until the transfer completes.
- The payload lies in DMA-visible packet RAM; [the bus address](addresses.html) is checked before the engine starts.
- A write barrier precedes the start command.
- Every FIFO and command wait is bounded, at 50 ms. A timeout is fatal until an engine reset sequence is known.
