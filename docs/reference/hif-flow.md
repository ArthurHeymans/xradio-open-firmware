---
title: Host handshake
description: How the XR819 firmware waits for the host, publishes the HIF rings and announces itself.
---
The host interface comes up in three steps spread over start-up: the firmware waits for the host, publishes its descriptor rings, and, once the rest of the hardware is ready, sends its first message.

## Wait for the host

Right after the clocks are configured:

1. Read `HifShared.ACTIVATION` (`0x0ab0_0140`) once and discard the value.
2. Poll `STARTUP_STATUS.WAIT` (bit 10 of `0x0ab0_0134`) until it clears, at most 1,000,000 times.

Which agent clears bit 10 has not been identified. If it never clears, the firmware stops there.

## Publish the rings

After the AES self-test the firmware sets up the host interface. The output ring needs no set-up: its descriptors are written as messages are staged.

1. For input slots 0–29: `address` = the bus address of input buffer *n*, `control` = `(0x660 + 1) & 0x1fff`, which publishes the 1,632-byte buffer. Slots 30 and 31 get zero address and control.
2. Enable IRQ 13.
3. Write `INTERRUPT_ACK = 0x7ff` to clear any stale cause.
4. Write `CONTROL = 0x7ff`, or `0x7f7` if bit 10 already reads set. The firmware records the written value ANDed with `0x7f8` but does not apply it.

```text
firmware                          HIF                              host
   |                               |                                 |
   |-- ACTIVATION read ----------->|                                 |
   |-- poll STARTUP_STATUS.WAIT -->|                                 |
   |                               |   (bit 10 clears)               |
   |-- input descriptors 0..29 --->|                                 |
   |-- enable IRQ 13               |                                 |
   |-- INTERRUPT_ACK, CONTROL ---->|                                 |
   |        ... packet DMA and MAC start-up ...                      |
   |-- STARTUP_IND on output ring->|-- DMA read -------------------->|
   |<-- IRQ 13, STATUS.TX_DONE ----|                                 |
```

## Announce the firmware

When packet DMA and the MAC are running, the firmware queues the [startup indication](wsm.html#startup-indication) (`0x0801`) as its first output message. It tells the driver the number and size of the input buffers, the hardware IDs and the firmware's capabilities. From then on the host sends requests through the input ring and the firmware answers through the output ring.
