---
title: Host interface
description: The XR819 host interface (HIF), its descriptor rings, status bits and flow control.
---
The host interface (HIF) is a descriptor-driven DMA engine between the SDIO function and packet RAM. One ring carries messages from the host, a second carries messages to it, and a separate emergency descriptor serves the fatal-exception path. Every message is a [WSM message](wsm.html).

## Descriptor format {#descriptor}

Both rings use the same 8-byte descriptor.

{% include registers/Descriptor.md %}

### Descriptor CONTROL {#descriptor-control-word}

{% include registers/DescriptorControl.md %}

## Input ring

Thirty-two host-to-firmware descriptors at `0x0ab0_0000`, refilled in turn as the firmware returns buffers. The host writes a message into an input buffer; the firmware reads the 4-byte WSM header (length, then ID, both little-endian 16-bit) at the buffer start and the payload at `+4`.

{% include registers/RxShared.md %}

## Output ring and control

Four firmware-to-host descriptors and the shared control registers at `0x0ab0_0100`. A descriptor points either at an output buffer or, for a received frame, directly at the frame's [RX FIFO slot](rx.html#handing-a-frame-to-the-host).

{% include registers/HifShared.md %}

### CONTROL

{% include registers/HifControl.md %}

### STATUS, INTERRUPT_ACK

{% include registers/HifStatus.md %}

### STARTUP_STATUS

{% include registers/HifStartup.md %}

## Ring geometry

| Parameter | Value | Notes |
|---|---|---|
| Input buffers | 30 × `0x660` | Announced to the host in the startup indication |
| Input descriptors | 32 | Two start empty; refills rotate through all 32 |
| Output descriptors | 4 | At most four messages in flight to the host |
| Output queue | 64 entries | Software staging, independent of the hardware ring |
| Sequence | 2 bits per descriptor | Wraps independently of the queue |

## Ownership

<div class="warning" markdown="1">
**A cleared descriptor is not a completion.** The engine may clear a descriptor's control word after fetching it but before the host has finished reading the buffer. Reuse an output buffer only after `STATUS.TX_DONE`.
</div>

When `TX_DONE` is seen, the firmware walks the output descriptors in order and treats each one whose `OWN` bit has already cleared as complete. The reasoning: `TX_DONE` is raised after the host has read the buffers, so a descriptor released before it was raised has been read too <span class="tag inferred">inferred</span>. For each, it first makes the slot reusable and stages the next queued message, and only then releases the completed message's buffer. On the input side, `RX_READY` schedules the firmware to take one request. It takes it only when a reply could be sent at once: an output buffer is free and the 64-entry output queue has room. Otherwise the request waits. After taking one, the firmware checks the next input descriptor: if its `OWN` bit has cleared, it schedules that request as well, without waiting for another `RX_READY`.

## Sequence numbers

Two sequence numbers travel with each output message:

- The message's WSM ID carries a 3-bit sequence number in bits 15:13. The firmware writes it into the buffer when it stages the message; it counts staged messages, skipping the numbers used by emergency messages, because the host counts both.
- The descriptor's `SEQUENCE` field carries bits 14:13 of the ID as the message was queued, before that rewrite.

[Host handshake](hif-flow.html) shows how the HIF is brought up, and [Programming checklists](sequences.html#publishing-one-hif-output-message) how one output message is published.
