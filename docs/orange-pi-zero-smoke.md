# Orange Pi Zero smoke test, 2026-11-19

Firmware source `f53c5d4`. A fresh checkout with locally extracted inputs produced identical packed images (SHA-256):

```
05ba3714e15477ed72b38ec31ad0792f731cd74728b032cb72ce5229c53f0b99  boot_xr819.bin
2c0226d2bca44e12fbcdc27fa9e57c70aaaba313ecd1c6d6924b2db1f578c21a  fw_xr819.bin
```

The board booted [Linux `8b3b8d68f31d`](https://github.com/ArthurHeymans/linux/commit/8b3b8d68f31d), based on Torvalds master `4405c18c95d2`. The kernel, both firmware images and `cw1200` modules were checked after copying. Probe reported `XR819 open Rust native`. The standalone firmware checks passed (267 Rust tests, 44 Python tests, ARM stack check and both image builds); the Linux build included both SDIO and SPI. Only SDIO ran on hardware.

With the XR819 as a station, WPA2-PSK/CCMP completed against the Intel peer. Each end received 9 of 10 pings. With the roles reversed, hostapd completed the four-way handshake. The board lost its test IPv4 address on the interface mode change; after assigning it to the running AP, both ends received 20 of 20 pings with 1,200-byte payloads. These are connectivity checks, not throughput measurements.

SDIO unbind subsequently faulted in `cw1200_debug_release`: unregistering the wiphy had already removed its debugfs subtree. [Linux `e4bcf24ebf56`](https://github.com/ArthurHeymans/linux/commit/e4bcf24ebf56) drops the second removal and builds for ARM. The station and AP results above are for its parent.

## Retest with firmware `f9e52aa`

Torvalds master was still `4405c18c95d2`. With the six-patch Linux tip `e4bcf24ebf56` the board booted, exchanged WPA2-CCMP traffic with the Intel `wlp4s0` in both roles, and unbound without an oops. Bind still returned `-EINVAL`: the XR819 was already in queue mode and the driver could not start another firmware download.

Linux `476daf13af49` resets only the XR819 SDIO card on probe. With that change, three consecutive unbind/bind cycles passed. The board then booted the revised module in the one-boot kernel and passed a fourth cycle. Both firmware images were read back: runtime SHA-256 `8069466211146778955ecf17545dcf45259678b1726779b5eaa40ac75790c3ed`, bootloader unchanged from above.

After rebind, the first station association attempt did not complete within 35 seconds. On retry WPA2-CCMP completed: 19/20 pings from board to Intel and 20/20 back (1,200-byte payload). As an AP, the XR819 exchanged 10/10 pings each way. The kernel still logged a failed WSM confirmation for request `0x0017`. This was a connectivity and rebind check, not a lossless, throughput, or endurance qualification.

The board rebooted into `7.3.0-xr819-test+` after the test. Both firmware files and both driver modules compared equal to `/root/xr819-rest`; the one-boot kernel slot was restored too.
