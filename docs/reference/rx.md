---
title: Receiving a frame
section: Hardware reference
---
# Receiving a frame

```
packet DMA                    firmware main loop                 HIF
    |                                 |                             |
    |-- write slot at producer -----> |                             |
    |   header: magic, next, state    |                             |
    |   frame, trailer                |                             |
    |-- producer advances ----------> |-- read 0x0604 (producer)    |
    |                                 |-- validate magic + length   |
    |                                 |-- read frame + trailer      |
    |                                 |-- build WSM 0x0804 message  |
    |                                 |    header + frame           |
    |                                 |-- stage into TX descriptor ->|
    |                                 |   (4-deep ring, seq bits)    |
    |<-- write 0x0608 (consumer) -----|                             |
    |   release slot (state=0xcccccc00)                             |
    |                                 |<---- host reads buffer -----|
    |                                 |-- reclaim descriptor         |
    |                                 |-- return RX buffer to ring --|
```

### What the firmware must get right

1. **Producer read is a snapshot.** Treat it as an upper bound, not a promise that everything below it is complete; validate magic and length before trusting a slot.
2. **Offsets are modulo `0x7000`** on the CPU side, and a slot that starts near the end of the ring spills linearly into the next `0x1000` bytes of storage. Do not wrap a slot's interior.
3. **Release only what was claimed.** Writing a larger consumer value than the slots actually processed loses frames silently.
4. **Return RX buffers in the order they were read.** The radio publishes at most 24 FIFO slots before it takes them back, and the return queue is 64 entries of tokens.
5. **Resynchronization is real.** When a slot's internal next pointer disagrees with the producer, the vendor re-derives the consumer by scanning for the next magic slot. Doing that too aggressively costs frames; not doing it stalls the ring.
