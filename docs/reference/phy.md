---
title: PHY and RF
section: Hardware reference
---
# PHY and RF

The loader writes four tables before the runtime takes over. We know their destinations and the order of the register writes, not every RF field’s purpose.

The PHY and RF blocks occupy a 0x60000 window. Most of their register semantics are unknown: the vendor programs them with absolute-address/value sequences whose meaning is not recoverable from the image alone. Three categories are documented here: the loader-applied tables (which the Rust bootloader currently writes), the calibration engine, and the packet-DMA-adjacent words that firmware does read.

## Loader-applied PHY tables
{:#s11-1}

Section type 2 of the vendor container is not consumed by firmware. The download bootloader applies 41 address/value pairs directly before jumping to the low image, and four type-0 copies land in MAC/PHY memory. A custom downloader that skips them starts from an uninitialised PHY.

| Section | Destination | Length | Contents |
| --- | --- | --- | --- |
| type 2 (MMIO) | address/value pairs | 0x148 | 41 pairs, listed in 11.2 |
| type 0 copy | 0x0ab8_1000 | 0x400 | MAC/PHY table |
| type 0 copy | 0x0ab8_8400 | 0x330 | MAC/PHY table |
| type 0 copy | 0x0ab8_8800 | 0x330 | MAC/PHY table |
| type 0 copy | 0x0ab8_8c00 | 0x330 | MAC/PHY table |

## Loader MMIO table (41 pairs, in order)
{:#s11-2}

{% include registers/LoaderWrites.md %}

## Calibration engine (0x0abb_80xx)
{:#s11-3}

The calibration block is a command/result unit embedded in the PHY window. Firmware triggers a measurement, waits for a busy bit to clear, and reads two accumulators.

| Address | Role | Conf. |
| --- | --- | --- |
| 0x0abb_8004 | Calibration enable. Bit `0x0004_8000` set enables the engine, cleared disables it. | Observed |
| 0x0abb_805c..0x0abb_8064 | Three context words restored around band selection. | Inferred |
| 0x0abb_80c0 | Register-base value the vendor carries as the fourth argument through band select and IQ DAC setup. | Observed |
| 0x0abb_80f0 | Command register. Bit 4 reads set when the measurement completed. Mode command `0x0100_4008 + mode`; sample command `0x0800_000d`. | Observed |
| 0x0abb_80f4 | Timing: `0x0020_0078` for modes 2/3, `0x0020_0190` otherwise. | Observed |
| 0x0abb_80f8 | `0x012c_00c8`. | Observed |
| 0x0abb_810c / 0x0abb_8110 | I and Q accumulators, decoded as the measured coefficient pair. | Observed |
| 0x0abb_8114 | Sample settings word: I in bits 8:0, Q in bits 24:16, plus `0x0100_0100`. | Observed |
| 0x0abb_81a4 | Calibration gain: `0x40 \| gain` for non-negative gain, `0` otherwise. | Observed |

## Words shared with the packet-DMA path
{:#s11-4}

| Address | Observed use | Conf. |
| --- | --- | --- |
| 0x0ab9_8040 | Cleared during `prepare_packet_dma()`, before packet DMA is enabled. | Observed |
| 0x0ab8_0c50 | Read once per MAC event that carries the sideband marker; the read may be what acknowledges the capture. Value is not consumed otherwise. | Inferred |
| 0x0abb_0010..0x0abb_0030 | Interrupt routing words, [Interrupt controller](interrupts.html#s3-3). | Observed |
