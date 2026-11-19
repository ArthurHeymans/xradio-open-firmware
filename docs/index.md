---
title: XR819 firmware manual
---
# XR819 firmware manual

The XR819 runs the radio on an Orange Pi Zero. Linux talks to it over SDIO; the firmware on the chip manages packet memory, queues transmissions, and handles the MAC and PHY.

## [Use the firmware](linux-driver.html)

Build the bootloader and runtime, find the Linux driver, and keep the calibration file that belongs to your board.

## [Hardware reference](reference/index.html)

Start from a CPU address, or follow a packet through the [host interface](reference/hif.html), [transmit rings](reference/rings.html), and [radio](reference/phy.html). Register layouts come from Rust; the [reading guide](reference/guide.html) distinguishes those layouts from measurements and inferred behavior.

## [Lab notes](lab.html)

Look up the driver and image used in a board run, compare an observation with the reference, or find a question that still needs a measurement.
