---
title: XR819 open firmware
nav_title: Overview
description: Rust firmware for the XRadio XR819 Wi-Fi chip, and a reference manual for its hardware.
---
Open firmware for the XRadio XR819 Wi-Fi chip, written in Rust, and a reference manual for the hardware it drives.

The XR819 is the 2.4 GHz 802.11b/g/n radio on boards such as the Orange Pi Zero. A patched version of Linux's `cw1200` driver talks to it over SDIO. This firmware runs on the chip's ARM966E-S core in place of the vendor image: it owns packet memory, queues frames for the MAC, runs CCMP through the AES engine and drives the PHY.

<ul class="cards">
<li><a href="linux-driver.html"><strong>Build and install</strong><span>Build the three firmware files and install them for the patched <code>cw1200</code> driver.</span></a></li>
<li><a href="reference/"><strong>Hardware reference</strong><span>Memory map, register blocks with bit layouts, descriptor formats and host messages.</span></a></li>
<li><a href="reference/flows.html"><strong>Operation</strong><span>Boot, bring-up, receive, transmit, encryption and sleep, step by step.</span></a></li>
<li><a href="orange-pi-zero-smoke.html"><strong>Lab notes</strong><span>Board test reports, then measurements and open questions.</span></a></li>
</ul>

## Status

On an Orange Pi Zero with the patched driver, the firmware associates as a WPA2-PSK station, runs as a WPA2 access point, and survives repeated driver unbind and bind. In the November 2026 retest the first association after a rebind needed a retry, and the kernel logged one failed AP start confirmation. These are connectivity tests, not throughput or endurance qualification; the [board test reports](orange-pi-zero-smoke.html) list the exact kernel and firmware revisions. Only SDIO has run on hardware.

## About this manual

Tables and bit diagrams of the typed register blocks are generated from the firmware's Rust declarations, so they always match the code that runs; the sparse MAC and PHY windows are maintained by hand from the code. Everything else states how it was established; [Reading this reference](reference/guide.html) explains the conventions, the [register index](reference/registers.html) finds any named register by name or address, and the search box, or <kbd>/</kbd>, searches every page.
