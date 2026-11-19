---
title: AES and CCMP
section: Hardware reference
---
# AES and CCMP

The CCMP engine takes keys and authenticated header data through a FIFO. It moves the frame payload through packet RAM by DMA. The initialization port is a different register.

For each transfer the firmware stages words in the FIFO, issues commands through the command/status register, and gives the engine a DMA address and payload length. It need not reload an unchanged key.

## Register map
{:#s10-1}

{% include registers/AesRegisters.md %}

- **COMMAND_STATUS**: Write a command word; read status. Bit 13 FIFO ready, bit 12 busy/command not accepted, bit 0 authenticated result (sampled after the completion IRQ for decryption).

- **FIFO**: 32-bit writes supply little-endian key, CCM context and AAD words. The initialization stream goes to +0x08, not here.

- **MODE1_INIT**: The engine initializer writes 430 bytes here, one per 32-bit write, with bit 31 set on the first. It then writes 0x9000 to +0x00. The vendor transfer dispatcher can select this routine. The bytes’ encoding and whether the engine needs them are unknown.

- **SOURCE**: DMA source, encoded `cpu & 0xf6ff_ffff`.

- **DESTINATION**: DMA destination, same encoding.

- **LENGTH**: Payload length in octets, excluding CCMP header and MIC.

## Command words
{:#s10-2}

| Command | Stage | Notes | Conf. |
| --- | --- | --- | --- |
| 0x1100 | Key load | After four FIFO words holding the 16 key bytes little-endian. Bytes are not reversed relative to their WSM representation. | Observed |
| 0x1240 | CCM context | After one 16-byte FIFO block: `[0]=1`, `[1]=`TID, `[2..8]=`A2, `[8..14]=`PN5..PN0, `[14..16]=`payload length big-endian. | Observed |
| 0x1403 / 0x1402 | AAD first block | Transmit / receive respectively. | Observed |
| 0x148b / 0x148a | AAD tail, 22 bytes | 3-address non-QoS, TX / RX. | Observed |
| 0x14ab / 0x14aa | AAD tail, 24 bytes | 3-address QoS, TX / RX. | Observed |
| 0x14eb / 0x14ea | AAD tail, 28 bytes | 4-address non-QoS, TX / RX. | Observed |
| 0x140b / 0x140a | AAD tail, 30 bytes | 4-address QoS, TX / RX. | Observed |
| 0x3008_1009 | Start, encrypt | Payload length already in `+0x18`; encrypts in place when source = destination. | Observed |
| 0x3008_1008 | Start, decrypt | Supports the vendor overlap: `src = frame + hdr + 8`, `dst = frame + hdr`, which decrypts while removing the CCMP header. | Observed |

### AAD stream shape

```
FIFO word 0 : big-endian AAD length, then AAD bytes
AAD         : masked frame control (below), A1, A2, A3,
              masked sequence control,
              [A4], [QoS control]
then        : one of the tail commands above
then        : DMA start command
```

The Rust encoder clears Retry, Power Management and More Data; it also masks subtype bits 4–6 for non-management frames and bit 15 for QoS data. It sets the Protected bit in the AAD for both management and data frames.

## Transfer class table
{:#s10-3}

The vendor keeps a transfer-class table in DTCM at `0x0400_0804`. The classes relevant to ordinary data CCMP:

| Class | Flags | Operation |
| --- | --- | --- |
| 6 | 0x84 | CCMP transmit encryption |
| 7 | 0x80 | CCMP receive decryption |
| 8, 9, 10 | - | Protected-management / BIP stages, not ordinary data CCMP |

Completion is signalled on IRQ 18 and IRQ 20. The vendor handler clears the global owner, checks status bit 0 for classes 6 through 9, moves the descriptor to a deferred-completion list, starts the next queued operation, and runs callbacks outside interrupt context. The engine is globally serialized: one transfer at a time, key and AAD storage retained until completion, and a bounded FIFO/command wait (50 ms in the open firmware) that is fatal until an engine reset sequence is known.

[Follow a CCMP transfer →](ccmp.html) · [Origin of the initialization payload](../init-data.html)
