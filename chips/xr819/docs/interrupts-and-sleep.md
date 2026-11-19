# Interrupts and sleep

The firmware takes IRQs but keeps FIQ masked. The IRQ handler
(`interrupts.rs`) masks each pending source, acknowledges it and records
it; the source's owner takes the record in the main loop, clears the cause
in its device and re-arms the source. It runs in IRQ mode from the
firmware's own entry: aarch32-rt's trampoline switches to SYS mode with
only IRQ masked, so the MAC's FIQ line, asserted whenever events wait, was
taken at the first IRQ.

An ARM966E-S in wait for interrupt (`mcr p15, 0, r0, c7, c0, 4`) wakes when
any source the controller lets through is pending, masked in the CPSR or
not. The idle sleep masks IRQ across its last check and the wait, as the
vendor's scheduler does, and the handler records what woke it afterwards.

## What the board showed

Found with throwaway images that wrote register values into the counters
table (MIB 0x100c) during startup or in the main loop.

- Interrupt controller at `0x0a88_0000`: `+0x0c` enable, `+0x04`
  acknowledge (write ones), `+0x20` pending, which shows only enabled
  sources. `+0x28` reads raw source states. Configuration `+0x18` is
  `0x1600_a037`; its top byte names source 0x16, the MAC, for the FIQ.
- Timers at `0x0ac0_0000`, both 1 MHz from 80 MHz (`prescale` 0x4f).
  Timer 0 runs free, `+0x04` is the firmware clock. Timer 1: write 0 to
  control `+0x1c`, the delay in microseconds to `+0x14`, then 0xc1 to
  control. It raises IRQ 6 once and control reads 0xc3. Writing 0xc1 over
  a fired timer does not start it again. It keeps counting while the CPU
  sleeps: requested 1,000, 20,000 and 5 us slept 1,001, 20,001 and 7 us.
- IRQ 0 and 2 are the vendor's debug console UART at `0x0a90_0000`
  (handlers in the vendor's high support section at `0xfff019d8` and
  `0xfff019e0`). IRQ 0, transmit register empty, is pending whenever
  nothing is sent; the vendor's print routine enables it only while it
  has output. Enabled, it ended every wait at once.
- Seen pending after a pass under traffic: IRQ 13 (HIF) and IRQ 20 (AES),
  both cleared by their owners. Never seen: IRQ 26, which the vendor
  takes for the TSF adjustment it makes when syncing to beacons; this
  firmware never adjusts the TSF.
- Received frames end the wait through the MAC's FIQ line: the MAC
  reports each frame as an event with bit 7 set, and the vendor's FIQ
  handler (`mac_irq_handler`) schedules `rx_handler` from it. Of 3,707
  sleeps during which the RX DMA producer (MAC `0x0604`) moved, 3,681
  ended early with no controller source pending, and only 17 sleeps ran
  their full length while a frame arrived. The FIQ line shows neither in
  the pending register nor in the raw one, and enabling source 0x16 there
  changes nothing: the vendor never enables it either.
- With every source enabled, 9, 24 and 28 stay pending; raw bit 28 is set
  while the MAC event FIFO holds events. Acknowledging 28 (or every
  source) stopped RX within seconds, so the firmware leaves them alone.
- The vendor writes nine registers at `0x0abb_0010..0030` just before it
  unmasks interrupts. Replaying them changed nothing measurable here.
- The stack driver's debugfs `ahb` reads every one of these registers as
  0 with the CPU halted, the free-running clock included, so the vendor
  firmware's settings cannot be read back that way.

## How the loop sleeps

Each owner exposes an activity fingerprint (`activity.rs`): HIF ring
cursors, RX FIFO offsets and the frame in decryption, AES transfer
states, host TX context states, MAC events serviced. A pass that changed
none is idle. After 5 ms of idle passes the loop waits for an interrupt
(`platform::wait_for_event`), with timer 1 set for the next timed work:
the pipe watchdog every 200 ms, beacon loss, an ADDBA request's
response timeout, an aggregate's missing BlockAck, the end of a scan
dwell or the delay before the next probe request. A scan that tunes,
settles or stops the radio keeps the loop awake. While work is in
flight (frames queued for the MAC, in decryption or held for
reordering, output the host has not read) a sleep lasts at most 1 ms:
not every step of that work raises an interrupt or names a deadline.

An associated, idle board wakes about 20 times a second, for beacons
and the watchdog; it woke 1,000 times a second with a fixed 1 ms
bound.

Without the linger, sleeping between frames lost 0.5-7% of a 30 Mbit/s
downlink, also with a 200 us bound and with the write buffer drained
before each wait. Waiting awake on the same conditions instead (a pending
source, a MAC event, timer expiry) lost 0-0.1%. So the loss comes from
the wait for interrupt itself, not from a missing wake. It shows only with
the minimal host driver: with the stack driver, sleeping between frames
lost 0.03-0.04% of the same downlink, like the vendor firmware (0.01-0.04%)
and the firmware with the linger (0.03-0.06%). Why is not known. With the
linger, frames of a flow keep the loop awake. An idle board sleeps about
99% of the time.
