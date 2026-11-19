# Orange Pi Zero smoke test, 2026-11-19

Firmware source `f53c5d4`. A fresh checkout with locally extracted inputs produced identical packed images (SHA-256):

```
05ba3714e15477ed72b38ec31ad0792f731cd74728b032cb72ce5229c53f0b99  boot_xr819.bin
2c0226d2bca44e12fbcdc27fa9e57c70aaaba313ecd1c6d6924b2db1f578c21a  fw_xr819.bin
```

The board booted [Linux `8b3b8d68f31d`](https://github.com/ArthurHeymans/linux/commit/8b3b8d68f31d), based on Torvalds master `4405c18c95d2`. The kernel, both firmware images and `cw1200` modules were checked after copying. Probe reported `XR819 open Rust native`. The standalone firmware checks passed (267 Rust tests, 44 Python tests, ARM stack check and both image builds); the Linux build included both SDIO and SPI. Only SDIO ran on hardware.

With the XR819 as a station, WPA2-PSK/CCMP completed against the Intel peer. Each end received 9 of 10 pings. With the roles reversed, hostapd completed the four-way handshake. The board lost its test IPv4 address on the interface mode change; after assigning it to the running AP, both ends received 20 of 20 pings with 1,200-byte payloads. These are connectivity checks, not throughput measurements.

SDIO unbind subsequently faulted in `cw1200_debug_release`: unregistering the wiphy had already removed its debugfs subtree. [Linux `e4bcf24ebf56`](https://github.com/ArthurHeymans/linux/commit/e4bcf24ebf56) drops the second removal and builds for ARM. Its unbind/rebind result is pending; the station and AP results above are for its parent.
