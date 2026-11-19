---
title: Encrypting and decrypting a frame
section: Hardware reference
---
# Encrypting and decrypting a frame

```
classify frame
    |
    |-- wait for bit 12 clear before issuing a command; later stages also wait for bit 13 set
    |-- if key not loaded: 4 FIFO words (16 key bytes), then 0x1100
    |-- write 16-byte CCM context block, then 0x1240
    |-- write AAD stream: 2-byte BE length + AAD bytes, then 0x1403/0x1402
    |-- write remaining AAD words and the length-specific tail command (use the TX tail command minus one for RX)
    |-- source, destination, length
    |-- write barrier
    |-- start: 0x3008_1009 (TX) or 0x3008_1008 (RX)
    |
    |<-- IRQ 18 or 20
    |-- read status: bit 0 is the MIC check for decryption
    |-- next queued operation (the engine is globally serialized)
```

Firmware invariants: one transfer at a time device-wide; key, AAD and payload storage retained until completion; DMA-visible uncached memory; a write barrier before the start word; bounded FIFO and command waits, treated as fatal until a reset sequence is known.
