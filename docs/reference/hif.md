---
title: SDIO host interface
section: Hardware reference
---
# SDIO host interface

The two descriptor rings face opposite directions. A descriptor becoming available and the host finishing a transfer are different events.

The HIF is a descriptor-driven DMA engine between the SDIO function and packet RAM. It has two rings in opposite directions plus a dedicated emergency descriptor used by the fatal-exception path. Both rings are memory-mapped in the CPU's address space, so firmware behaviour is "publish a descriptor, then read the engine's status registers".

## Descriptor format (8 bytes)
{:#s6-1}

{% include registers/Descriptor.md %}

### CONTROL bitfield

{% include registers/DescriptorControl.md %}

The Rust `LENGTH` mask includes bit 0, which the driver sets to publish ownership. Bits 12:1 encode the even transfer length. The hardware descriptor uses the low two bits of the WSM sequence.

## Ring 1: host-to-firmware descriptors (0x0ab0_0000)
{:#s6-2}

32 descriptors, 8 bytes each, covering the 30 input buffers of `0x660` bytes. Slots 30 and 31 are published with a zero address and zero control.

{% include registers/RxShared.md %}

The engine writes the received message into the buffer; the firmware reads the 4-byte WSM header (*length* LE16, *id* LE16) at the buffer start and the payload at `+4`.

## Ring 2 and shared registers (0x0ab0_0100)
{:#s6-3}

{% include registers/HifShared.md %}

- **TX[0..3]**: Four firmware-to-host descriptors, same format as above. Packet buffers are `0x180` bytes in the linker-owned output pool.

- **CONTROL**: Bits 10:0 length mask. Vendor writes `0x7ff`; if bit 10 already reads set it writes `0x7f7` and the firmware's per-message length mask becomes `value & 0x07f8`.

- **STATUS**: Cause bits. Bit 1 = transmit completion pending; bit 2 = host request ready. Only these two schedule work.

- **INTERRUPT_ACK**: Write the value read from `+0x24` to clear the causes. Re-arming IRQ 13 before all causes are acknowledged loses events.

- **EMERGENCY_ADDRESS**: Bus address of the fatal-exception message buffer.

- **EMERGENCY_CONTROL**: Same bitfield as a normal descriptor control word, plus the emergency sequence.

- **STARTUP_STATUS**: Startup wait condition; the agent that clears bit 10 has not been identified. The vendor routine at `0xab4` reads +0x40 once, then polls bit 10 at `0x0ab00134` until clear. The two reads are not interchangeable.

- **ACTIVATION**: Named for the vendor routine that reads it. Reads `2` in the working vendor runtime; the routine does not identify who provided the value.

## Ring geometry and flow control
{:#s6-4}

| Parameter | Value | Meaning |
| --- | --- | --- |
| Input buffers | 30 × 0x660 | Announced to the host in the startup indication (`input_buffers`, `input_buffer_size`). |
| Output descriptors | 4 | Hardware ring depth. Only four messages may be in flight in this direction. |
| Output queue depth | 64 | Software staging queue, independent of the hardware ring. |
| Input ring descriptors | 32 | Two slots beyond the buffer count stay disabled. |
| Sequence width | 2 bits | Per-descriptor; wraps independently of the 64-entry queue. |

**Ownership rule.** A cleared descriptor control word alone is not a transfer-completion notification: the engine may clear it after fetching the descriptor but before the host has finished reading the buffer. Firmware must treat HIF status bit 1 as the completion event, and status bit 2 as the request-ready event, with the descriptor being reclaimed only afterwards. Vendor `hif_tx_confirm_drain` ordering is explicit: make the descriptor slot reusable and push its successor *before* releasing the completed message's backing storage.

## Shared control mask

{% include registers/HifControl.md %}

[Follow a host startup handshake →](hif-flow.html)
