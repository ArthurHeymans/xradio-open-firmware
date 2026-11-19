---
title: A-MPDU aggregation
section: Hardware reference
---
# A-MPDU aggregation

An aggregate is one descriptor whose command stream transfers each member, delimits it, and ends with a terminal command. Members live in a per-pipe software record of `0x2a0` bytes, and each member has a retained identity so that a later selective retry cannot consume another member's state.

```
negotiated BA session (MIB 0x100e, ADDBA)
    |
    |-- choose members from the per-AC queue, bounded by the peer's
    |   minimum MPDU start spacing (MIB 0x1010, 0..=7)
    |
    |-- for each member: build transfer word 0x65000000 | (addr & 0x001ffffc)
    |-- between members: 0x66000000
    |-- close: 0xe4000000
    |
    |-- publish once, push one airtime word, GO
    |
    |<-- compressed BlockAck observed, bitmap retained against the two
    |    exact frame-node identities
    |-- selective retry: re-arm only the members the bitmap rejects
```

Observed behaviour when the stream is perturbed (each attempt was a hardware run):

| Change | Result |
| --- | --- |
| Delimiter `0x6600_0001` (ignored low bit) | Ordinary operation preserved |
| Delimiter `0x6700_0000` | Only second-member loss: 52 selective second retries and 8 second terminals in 5 s, confirming the delimiter controls the boundary before member two |
| First transfer `0x6400_0000` | Aggregation never became operational, 8 buffers left behind |
