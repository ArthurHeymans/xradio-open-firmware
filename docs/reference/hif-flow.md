---
title: Host activation and startup
section: Hardware reference
---
# Host activation and startup

Activation is a two-sided handshake. Firmware publishes the input descriptor ring, enables the HIF interrupt source, sets the length mask, and then waits for the host to clear the startup bit. Nothing else in the HIF works before that.

```
firmware                                    hardware                     host
   |                                            |                           |
   |-- publish 30 RX descriptors --------------->|                           |
   |   address = cpu & 0xf6ff_ffff              |                           |
   |   control = len|1, seq                     |                           |
   |-- enable IRQ 13 -------------------------->|                           |
   |-- +0x28 = 0x7ff (ack all causes) ---------->|                           |
   |-- +0x20 = 0x7ff (or 0x7f7) --------------->|                           |
   |                                            |                           |
   |   (vendor 0x00000ab4)                      |                           |
   |-- read 0x0ab00140 ------------------------->|  activation = 2           |
   |-- poll 0x0ab00134 bit 10 until clear ------>|  startup_status           |
   |                                            |<----- host writes --------|
   |                                            |       activation          |
   |-- build startup indication (0x0801) -------|                           |
   |-- enqueue to TX descriptor ring ---------->|-- DMA read -------------->|
```

### Startup indication payload

ID `0x0801`, sent as the first firmware-to-host message. Field order is fixed by the host driver:

| Offset | Field | Note |
| --- | --- | --- |
| 0x00 | input_buffers | 30 in the reference container |
| 0x02 | input_buffer_size | `0x660` |
| 0x04 | hardware_id | Identifies the XR819 family |
| 0x06 | hardware_sub_id | Board/stepping variant |
| 0x08 | status | Success/failure of initialization |
| 0x0a | firmware_capabilities | Feature bits the host may use |
| 0x0c | firmware_type | WSM firmware class, not vendor identity |
| 0x0e | firmware_api | WSM API revision |
| 0x10 | firmware_build | Build number |
| 0x12 | firmware_version | Version word |
| 0x14 | label[128] | Free-form build label, NUL padded |
| 0x94 | config[4] | Four configuration words |
