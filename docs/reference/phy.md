---
title: PHY and RF
description: The XR819 PHY and RF window at 0x0ab8_0000, the loader-applied tables and the calibration engine.
---
The PHY and RF blocks occupy a `0x6_0000`-byte window at `0x0ab8_0000`. The vendor programs most of it with absolute address/value sequences whose meaning cannot be recovered from the image, so this page covers what is known: the tables the loader applies before the runtime starts, the calibration engine, and a few words shared with packet DMA.

## Loader tables

The bootloader, not the runtime, initializes the PHY. Before jumping to the runtime it applies 41 ordered register writes and copies four tables into PHY memory. A downloader that skips them starts the runtime with an uninitialized PHY, and the result looks like MAC or calibration bugs.

| Destination | Length | Contents |
|---|---:|---|
| Register writes | 41 pairs | [Below](#register-writes), in order |
| `0x0ab8_1000` | `0x400` | PHY table `PHY_INIT_AB81000` |
| `0x0ab8_8400` | `0x330` | PHY table `PHY_INIT_AB88400` |
| `0x0ab8_8800` | `0x330` | PHY table `PHY_INIT_AB88800` |
| `0x0ab8_8c00` | `0x330` | PHY table `PHY_INIT_AB88C00` |

The tables are copied as little-endian words after the register writes. They are not the SDD file. They live in [`src/init_data.rs`](https://github.com/ArthurHeymans/xradio-open-firmware/blob/main/src/init_data.rs).

### Register writes

In write order. The last two land in the [interrupt routing](vendor.html#interrupt-routing-words) window.

<details markdown="1">
<summary>Show the 41 writes</summary>

{% include registers/LoaderWrites.md %}

</details>

## Calibration engine

A command/result unit inside the PHY window at `0x0abb_8000`. The firmware selects a mode, writes the I and Q settings, triggers a measurement, waits for completion and reads two accumulators.

{% include register-rows.html data=site.data.phy prefix="phy" section="calibration" %}

### CAL_COMMAND

{% include registers/CalibrationCommand.md %}

### CAL_SAMPLE

{% include registers/CalibrationSample.md %}

### CAL_GAIN

{% include registers/CalibrationGain.md %}

## Channel PLL

The radio is tuned by a fractional-N PLL. For channel *c* the carrier is 2407 + 5*c* MHz, or 2484 MHz for channel 14, and the divider is

```text
ratio      = f_kHz × 1250 / (f_ref_kHz × 1000)
integer    = floor(ratio)
fraction   = floor((ratio − integer) × 2^21)
PLL_VALUE  = integer << 21 | fraction
```

The reference is 26 MHz unless the SDD file's element `0xc5` gives another. The factor 1.25 comes from the vendor arithmetic; what it corresponds to in the synthesizer is not established. For channel 6, 2437 MHz: integer 117, fraction `0x5_3b13`, `PLL_VALUE` = `0x0ea5_3b13`.

To retune, follow these steps. Delays are in spin-delay units: one unit is 34 iterations of a busy loop, of unmeasured duration.

1. Write `PLL_VALUE`.
2. Clear `PLL_CONTROL.RESTART`, delay 1, set it again, delay 120.
3. In the 2.4 GHz mode only: clear bit 6 of `RF_MODE_0084` and bit 9 of `RF_BAND_0050`, delay 10, set both again, delay 10.

The 120 µs settle in [Retuning to a channel](sequences.html#retuning-to-a-channel) is separate: it follows calibration and gain programming.

{% include register-rows.html data=site.data.phy prefix="phy" section="pll" %}

### PLL_VALUE

{% include registers/PllValue.md %}

### PLL_CONTROL

{% include registers/PllControl.md %}

## Words shared with packet DMA

{% include register-rows.html data=site.data.phy prefix="phy" section="shared" %}
