---
title: WSM message identifiers
section: Hardware reference
---
# WSM message identifiers

The HIF carries CW1200-compatible WSM messages: a 4-byte header (*length*, *id*, both little-endian) followed by a payload. Identifiers relevant to the flows above:

| ID | Direction | Message | ID | Direction | Message |
| --- | --- | --- | --- | --- | --- |
| 0x0004 | host → fw | TX request | 0x0404 | fw → host | TX confirm |
| 0x0005 | host → fw | Read MIB | 0x0405 | fw → host | Read MIB response |
| 0x0006 | host → fw | Write MIB | 0x0406 | fw → host | Write MIB response |
| 0x0007 | host → fw | Start scan | 0x0407 | fw → host | Start scan response |
| 0x0008 | host → fw | Stop scan | 0x080f | fw → host | JOIN complete |
| 0x0009 | host → fw | Configuration | 0x0409 | fw → host | Configuration response |
| 0x000a | host → fw | Reset | 0x040a | fw → host | Reset response |
| 0x000b | host → fw | JOIN | 0x040b | fw → host | JOIN response |
| 0x000c | host → fw | Add key | 0x040c | fw → host | Add key response |
| 0x000d | host → fw | Remove key | 0x040d | fw → host | Remove key response |
| 0x0010 | host → fw | Set power mode | 0x0809 | fw → host | Set-PM complete |
| 0x0011 | host → fw | Set BSS params | 0x0411 | fw → host | Set BSS params response |
| 0x0012 | host → fw | TX queue params | 0x0412 | fw → host | TX queue params response |
| 0x0013 | host → fw | EDCA params | 0x0413 | fw → host | EDCA params response |
| 0x0016 | host → fw | Switch channel | 0x080a | fw → host | Switch channel indication |
| 0x0017 | host → fw | Start (AP) | 0x0804 | fw → host | Receive indication |
| 0x0018 | host → fw | Beacon transmit | 0x0801 | fw → host | Startup indication |
| 0x001b | host → fw | Update IE | 0x041e | fw → host | Multi-TX confirm |
| 0x001c | host → fw | Map link | 0x080c | fw → host | Suspend/resume indication |

Upstream `wsm_cmd_send()` pads every command by four bytes ("buggy SPI on CW1200"), so request parsers must take the length they need and ignore any tail bytes.
