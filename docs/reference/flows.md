---
title: Operation overview
description: The order in which the XR819 firmware brings up the hardware, and the tasks of its main loop.
---
This part of the manual follows the firmware through one task at a time, in the order the hardware requires. Several steps keep the vendor's order even where the final register values would not show a difference; the pages mark those steps.

## Start-up

1. [**Download the image.**](boot.html) The bootloader unpacks the runtime into the chip's memories and initializes the PHY.
2. [**Prepare the platform.**](bringup.html) Configuration word, remap windows and memory probe, timers, interrupt controller, then DMA and clocks.
3. [**Wait for the host.**](hif-flow.html#wait-for-the-host) The host interface reports that the host is ready.
4. **Enable timer and AES interrupts, and [test the AES engine](aes.html#start-up-self-test).**
5. [**Publish the host interface rings.**](hif-flow.html#publish-the-rings)
6. [**Start packet DMA.**](bringup-dma.html) Receive DMA, the TSF, the TX rings and the MAC's automatic responses.
7. [**Start the MAC.**](bringup-dma.html#mac-start-up) Receive register image, MAC start-up state, packet controller.
8. [**Announce the firmware.**](hif-flow.html#announce-the-firmware) The startup indication tells the driver the firmware is ready.

## Main loop

After the startup indication the firmware runs a single loop with no tasks or threads:

- [Receiving a frame](rx.html): from the RX FIFO to a WSM indication.
- [Transmitting a frame](tx.html): from a WSM request to a TX pipe and back to a confirmation.
- [A-MPDU aggregation](ampdu.html): several frames in one transmission, and selective retry.
- [CCMP encryption](ccmp.html): one AES engine transfer.
- [Idle and sleep](sleep.html): when the CPU waits for an interrupt, and how a station dozes in power save.

[Programming checklists](sequences.html) condenses the register-level steps of the common operations.
