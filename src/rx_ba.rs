//! Receive BlockAck sessions: a peer's aggregates to us. A station has one
//! peer, its AP; an access point one per station sending aggregates.
//!
//! The firmware answers the peer's ADDBA requests and DELBAs itself, as the
//! vendor does. Upstream cw1200 refuses every `ampdu_action`, so mac80211
//! never sees these sessions. The MAC is programmed to answer the peer's
//! A-MPDUs and BlockAckReqs with BlockAcks (vendor `pipe_setup_entry`), and
//! session frames are put back in sequence order here before they reach the
//! host; the vendor delivers them in arrival order.

use crate::rx_reorder::{Released, Reorder};
use crate::tx::ba::Outgoing;

/// Sessions the MAC can answer for (vendor: four receive pipes).
pub const SESSIONS: usize = 4;
/// Our window: the reorder buffer holds frames in the RX FIFO, which has
/// room for about 16 full-size frames.
pub const WINDOW: u16 = 16;
/// ADDBA status "request declined".
const STATUS_DECLINED: u16 = 37;
/// A hole older than this is given up; its frame, if it still comes, goes
/// out late. The sender retransmits within a few milliseconds.
pub const HOLE_TIMEOUT_US: u32 = 10_000;
const READY: usize = SESSIONS * WINDOW as usize + 16;

struct Session<T> {
    peer: [u8; 6],
    tid: u8,
    reorder: Reorder<T, { WINDOW as usize }>,
    /// Head and arrival time of the hole frames wait behind.
    hole: Option<(u16, u32)>,
}

/// Outcome of the peer's ADDBA request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Addba {
    /// Program receive pipe `slot` for the session.
    Accepted {
        slot: usize,
        window: u16,
    },
    Declined,
    /// No reply storage. Leave any existing agreement alone; the peer can retry.
    Busy,
}

pub struct RxBlockAck<T> {
    sessions: [Option<Session<T>>; SESSIONS],
    /// Frames released in order, waiting for the host.
    ready: [Option<T>; READY],
    ready_head: usize,
    ready_len: usize,
    // Dense FIFO, at most one pending negotiation per peer/TID.
    replies: [Option<Outgoing>; SESSIONS],
    /// TIDs the host allows sessions for (MIB 0x100e).
    allowed_tids: u8,
}

impl<T> Default for RxBlockAck<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> RxBlockAck<T> {
    pub const fn new() -> Self {
        Self {
            sessions: [const { None }; SESSIONS],
            ready: [const { None }; READY],
            ready_head: 0,
            ready_len: 0,
            replies: [None; SESSIONS],
            allowed_tids: 0,
        }
    }

    pub const fn allowed_tids(&self) -> u8 {
        self.allowed_tids
    }

    pub fn set_allowed_tids(&mut self, tids: u8) {
        self.allowed_tids = tids;
    }

    fn slot_of(&self, peer: [u8; 6], tid: u8) -> Option<usize> {
        self.sessions.iter().position(|session| {
            session
                .as_ref()
                .is_some_and(|session| session.peer == peer && session.tid == tid)
        })
    }

    /// Whether `peer`'s frames of `tid` go through a session.
    pub fn has_session(&self, peer: [u8; 6], tid: u8) -> bool {
        self.slot_of(peer, tid).is_some()
    }

    /// The peer asks to send aggregates for `tid`, starting at `ssn`. A
    /// session for the TID is replaced. Retires its hardware pipe before
    /// allocating the replacement, even if declined. Queues the ADDBA response.
    #[allow(clippy::too_many_arguments)]
    pub fn on_addba_request(
        &mut self,
        peer: [u8; 6],
        token: u8,
        tid: u8,
        immediate: bool,
        buffer_size: u16,
        ssn: u16,
        allowed_tids: u8,
        discard: &mut impl FnMut(T),
        retire_pipe: &mut impl FnMut(usize),
    ) -> Addba {
        // A later negotiation supersedes our unsent response for this stream.
        self.cancel_replies(peer, Some(tid));
        let Some(reply) = self.replies.iter().position(Option::is_none) else {
            return Addba::Busy;
        };
        let window = if buffer_size == 0 {
            WINDOW
        } else {
            buffer_size.min(WINDOW)
        };
        if let Some(slot) = self.slot_of(peer, tid) {
            retire_pipe(slot);
            self.end_slot(slot, discard);
        }
        let free = self.sessions.iter().position(Option::is_none);
        let outcome = match free {
            Some(slot) if immediate && tid < 8 && allowed_tids & (1 << tid) != 0 => {
                self.sessions[slot] = Some(Session {
                    peer,
                    tid,
                    reorder: Reorder::new(ssn, window),
                    hole: None,
                });
                (Addba::Accepted { slot, window }, 0, window)
            }
            _ => (Addba::Declined, STATUS_DECLINED, 0),
        };
        let (outcome, status, window) = outcome;
        self.replies[reply] = Some(Outgoing::AddbaResponse {
            peer,
            tid,
            token,
            status,
            window,
        });
        outcome
    }

    /// `peer` ended its session for `tid`; returns the receive pipe freed.
    pub fn end_session(
        &mut self,
        peer: [u8; 6],
        tid: u8,
        discard: &mut impl FnMut(T),
    ) -> Option<usize> {
        self.cancel_replies(peer, Some(tid));
        let slot = self.slot_of(peer, tid)?;
        self.end_slot(slot, discard);
        Some(slot)
    }

    /// End every session of `peer` (its link was reset); returns the
    /// receive pipes freed, one bit each.
    pub fn end_peer(&mut self, peer: [u8; 6], discard: &mut impl FnMut(T)) -> u8 {
        self.cancel_replies(peer, None);
        (0..SESSIONS).fold(0, |freed, slot| {
            if self.sessions[slot]
                .as_ref()
                .is_some_and(|session| session.peer == peer)
            {
                self.end_slot(slot, discard);
                freed | 1 << slot
            } else {
                freed
            }
        })
    }

    fn end_slot(&mut self, slot: usize, discard: &mut impl FnMut(T)) {
        let Self {
            sessions,
            ready,
            ready_head,
            ready_len,
            ..
        } = self;
        if let Some(mut session) = sessions[slot].take() {
            session
                .reorder
                .flush(&mut |released| route(ready, ready_head, ready_len, discard, released));
        }
    }

    /// `peer`'s BlockAckReq: frames before `ssn` are not coming.
    pub fn on_block_ack_request(
        &mut self,
        peer: [u8; 6],
        tid: u8,
        ssn: u16,
        now: u32,
        discard: &mut impl FnMut(T),
    ) {
        let Some(slot) = self.slot_of(peer, tid) else {
            return;
        };
        let Self {
            sessions,
            ready,
            ready_head,
            ready_len,
            ..
        } = self;
        if let Some(session) = sessions[slot].as_mut() {
            session.reorder.advance_to(ssn, &mut |released| {
                route(ready, ready_head, ready_len, discard, released)
            });
            note_hole(session, now);
        }
    }

    /// `peer`'s frame of `tid` with sequence number `sequence` arrived at
    /// `now`. Returns it back when there is no session; otherwise it is held
    /// or queued for the host in order.
    pub fn admit(
        &mut self,
        peer: [u8; 6],
        tid: u8,
        sequence: u16,
        item: T,
        now: u32,
        discard: &mut impl FnMut(T),
    ) -> Option<T> {
        let Some(slot) = self.slot_of(peer, tid) else {
            return Some(item);
        };
        let Self {
            sessions,
            ready,
            ready_head,
            ready_len,
            ..
        } = self;
        if let Some(session) = sessions[slot].as_mut() {
            session.reorder.receive(sequence, item, &mut |released| {
                route(ready, ready_head, ready_len, discard, released)
            });
            note_hole(session, now);
        }
        None
    }

    /// Give up holes older than [`HOLE_TIMEOUT_US`], or every hole when the
    /// RX FIFO runs short (`pressure`): frames the MAC acknowledges but
    /// cannot store are lost. A frame that fills a hole later goes out late.
    pub fn service(&mut self, now: u32, pressure: bool, discard: &mut impl FnMut(T)) {
        let Self {
            sessions,
            ready,
            ready_head,
            ready_len,
            ..
        } = self;
        for session in sessions.iter_mut().flatten() {
            let expired = session
                .hole
                .is_some_and(|(_, since)| now.wrapping_sub(since) >= HOLE_TIMEOUT_US);
            if session.reorder.held() != 0 && (pressure || expired) {
                session.reorder.skip_hole(&mut |released| {
                    route(ready, ready_head, ready_len, discard, released)
                });
                note_hole(session, now);
            }
        }
    }

    /// End every session and drop what they hold (join, unjoin).
    pub fn reset(&mut self, discard: &mut impl FnMut(T)) {
        for slot in 0..SESSIONS {
            self.end_slot(slot, discard);
        }
        while let Some(item) = self.pop_ready() {
            discard(item);
        }
        self.replies.fill(None);
    }

    /// The next frame for the host, oldest first.
    pub fn pop_ready(&mut self) -> Option<T> {
        if self.ready_len == 0 {
            return None;
        }
        let item = self.ready[self.ready_head].take();
        self.ready_head = (self.ready_head + 1) % READY;
        self.ready_len -= 1;
        item
    }

    pub const fn ready_len(&self) -> usize {
        self.ready_len
    }

    /// The ADDBA response owed, if any; [`Self::take_response`] once it
    /// is handed to the MAC.
    pub const fn response(&self) -> Option<Outgoing> {
        self.replies[0]
    }

    fn cancel_replies(&mut self, peer: [u8; 6], tid: Option<u8>) {
        let mut keep = 0;
        for i in 0..SESSIONS {
            let Some(r) = self.replies[i] else { break };
            if !matches!(r, Outgoing::AddbaResponse { peer: p, tid: t, .. } if p == peer && tid.is_none_or(|tid| tid == t))
            {
                self.replies[keep] = Some(r);
                keep += 1;
            }
        }
        self.replies[keep..].fill(None);
    }

    pub fn take_response(&mut self) -> Option<Outgoing> {
        let first = self.replies[0].take()?;
        self.replies.rotate_left(1);
        Some(first)
    }
}

/// Track how long the head has been missing.
fn note_hole<T>(session: &mut Session<T>, now: u32) {
    let head = session.reorder.head();
    session.hole = match session.hole {
        _ if session.reorder.held() == 0 => None,
        Some((waiting, since)) if waiting == head => Some((waiting, since)),
        _ => Some((head, now)),
    };
}

fn route<T>(
    ready: &mut [Option<T>; READY],
    head: &mut usize,
    len: &mut usize,
    discard: &mut impl FnMut(T),
    released: Released<T>,
) {
    match released {
        Released::Deliver(item) if *len < READY => {
            ready[(*head + *len) % READY] = Some(item);
            *len += 1;
        }
        // Cannot happen: at most the held frames plus one wait here.
        Released::Deliver(item) | Released::Drop(item) => discard(item),
    }
}

/// Program receive pipe `slot` to answer `peer`'s aggregates for `tid` with
/// BlockAcks, window starting at `ssn` (vendor `pipe_setup_entry`, 0x2652).
#[cfg(target_arch = "arm")]
pub fn program_pipe(slot: usize, interface: u8, peer: [u8; 6], tid: u8, ssn: u16) {
    use crate::platform::mac_register;
    let write = |offset: usize, value: u32| unsafe {
        (mac_register(offset) as *mut u32).write_volatile(value)
    };
    let read = |offset: usize| unsafe { (mac_register(offset) as *const u32).read_volatile() };
    write(
        0x60 + slot * 0xc,
        u32::from_le_bytes([peer[0], peer[1], peer[2], peer[3]]),
    );
    write(
        0x64 + slot * 0xc,
        u32::from(u16::from_le_bytes([peer[4], peer[5]])),
    );
    write(0x68 + slot * 0xc, u32::from(tid));
    // Two enable bits per pipe; the second serves the second interface.
    let field = 16 + 2 * slot;
    let enable = (read(0x11c) & !(3 << field)) | 1 << (field + usize::from(interface != 0));
    write(0x11c, enable);
    let pipe = 0x1200 + slot * 0x20;
    [0x10, 0x14, 0x18, 0x1c]
        .into_iter()
        .for_each(|offset| write(pipe + offset, 0));
    write(
        pipe + 4,
        (u32::from(ssn & 0x0fff) << 4).wrapping_add(0x8040_0000),
    );
    write(pipe, u32::from(tid) << 28 | 4 << 16 | 3);
}

/// Stop answering for receive pipe `slot` (vendor `pipe_clear_entry`, 0x2624).
/// Bit 7 of the TID written marks the entry unused (it reads back as bit 31).
#[cfg(target_arch = "arm")]
pub fn clear_pipe(slot: usize) {
    use crate::platform::mac_register;
    let write = |offset: usize, value: u32| unsafe {
        (mac_register(offset) as *mut u32).write_volatile(value)
    };
    write(0x1200 + slot * 0x20, 0);
    write(0x60 + slot * 0xc, 0xffff_ffff);
    write(0x64 + slot * 0xc, 0xff);
    write(0x68 + slot * 0xc, 0xff);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    const P: [u8; 6] = [0; 6];

    fn drain(rx: &mut RxBlockAck<u16>) -> Vec<u16> {
        core::iter::from_fn(|| rx.pop_ready()).collect()
    }

    #[test]
    fn a_session_reorders_and_other_tids_pass() {
        let mut rx = RxBlockAck::new();
        let mut dropped = Vec::new();
        let outcome = rx.on_addba_request(
            [0; 6],
            7,
            0,
            true,
            64,
            100,
            0xff,
            &mut |item| dropped.push(item),
            &mut |_| (),
        );
        assert_eq!(
            outcome,
            Addba::Accepted {
                slot: 0,
                window: 16
            }
        );
        assert_eq!(
            rx.take_response(),
            Some(Outgoing::AddbaResponse {
                peer: [0; 6],
                tid: 0,
                token: 7,
                status: 0,
                window: 16
            })
        );
        for sequence in [101, 102, 100, 100] {
            assert_eq!(
                rx.admit(P, 0, sequence, sequence, 0, &mut |item| dropped.push(item)),
                None
            );
        }
        assert_eq!(drain(&mut rx), [100, 101, 102]);
        assert_eq!(dropped, [100]);
        assert_eq!(rx.admit(P, 5, 1, 1, 0, &mut |_| ()), Some(1));
    }

    #[test]
    fn disallowed_tids_are_declined() {
        let mut rx = RxBlockAck::<u16>::new();
        assert_eq!(
            rx.on_addba_request([0; 6], 1, 3, true, 16, 0, 0x01, &mut |_| (), &mut |_| ()),
            Addba::Declined
        );
        assert_eq!(
            rx.take_response(),
            Some(Outgoing::AddbaResponse {
                peer: [0; 6],
                tid: 3,
                token: 1,
                status: 37,
                window: 0
            })
        );
        assert!(!rx.has_session(P, 3));
    }

    #[test]
    fn holes_are_given_up_on_timeout_and_block_ack_requests() {
        let mut rx = RxBlockAck::new();
        rx.on_addba_request([0; 6], 1, 0, true, 16, 0, 0xff, &mut |_| (), &mut |_| ());
        rx.admit(P, 0, 2, 2, 1_000, &mut |_| ());
        rx.service(1_000 + HOLE_TIMEOUT_US - 1, false, &mut |_| ());
        assert!(drain(&mut rx).is_empty());
        rx.service(1_000 + HOLE_TIMEOUT_US, false, &mut |_| ());
        assert_eq!(drain(&mut rx), [2]);
        rx.admit(P, 0, 5, 5, 0, &mut |_| ());
        rx.on_block_ack_request(P, 0, 6, 0, &mut |_| ());
        assert_eq!(drain(&mut rx), [5]);
        rx.admit(P, 0, 8, 8, 0, &mut |_| ());
        rx.service(0, true, &mut |_| ());
        assert_eq!(drain(&mut rx), [8]);
    }

    #[test]
    fn bar_times_the_new_hole_but_does_not_extend_an_unchanged_one() {
        let mut rx = RxBlockAck::new();
        rx.on_addba_request(P, 1, 0, true, 16, 0, 1, &mut |_| (), &mut |_| ());
        rx.admit(P, 0, 1, 1, 0, &mut |_| ());
        rx.admit(P, 0, 3, 3, 0, &mut |_| ());
        let bar = HOLE_TIMEOUT_US - 1;
        rx.on_block_ack_request(P, 0, 1, bar, &mut |_| ());
        assert_eq!(drain(&mut rx), [1]);
        rx.service(HOLE_TIMEOUT_US, false, &mut |_| ());
        assert!(drain(&mut rx).is_empty());
        rx.on_block_ack_request(P, 0, 2, bar + 5, &mut |_| ());
        rx.service(bar + HOLE_TIMEOUT_US, false, &mut |_| ());
        assert_eq!(drain(&mut rx), [3]);
    }

    #[test]
    fn a_delba_delivers_what_the_session_holds_and_a_reset_drops_it() {
        let mut rx = RxBlockAck::new();
        rx.on_addba_request([0; 6], 1, 4, true, 16, 0, 0xff, &mut |_| (), &mut |_| ());
        rx.admit(P, 4, 3, 3, 0, &mut |_| ());
        assert_eq!(rx.end_session(P, 4, &mut |_| ()), Some(0));
        assert_eq!(drain(&mut rx), [3]);
        rx.on_addba_request([0; 6], 2, 4, true, 16, 0, 0xff, &mut |_| (), &mut |_| ());
        rx.admit(P, 4, 3, 3, 0, &mut |_| ());
        let mut dropped = Vec::new();
        rx.reset(&mut |item| dropped.push(item));
        assert_eq!((drain(&mut rx), dropped), (Vec::new(), std::vec![3]));
        assert!(!rx.has_session(P, 4));
    }

    #[test]
    fn replacing_a_session_retires_its_pipe_even_when_declined() {
        let mut rx = RxBlockAck::new();
        let mut retired = Vec::new();
        let mut retire = |slot| retired.push(slot);
        rx.on_addba_request(P, 1, 0, true, 16, 0, 0xff, &mut |_| (), &mut retire);
        rx.on_addba_request(P, 2, 6, true, 16, 0, 0xff, &mut |_| (), &mut retire);
        rx.admit(P, 6, 3, 3, 0, &mut |_| ());
        assert_eq!(rx.end_session(P, 0, &mut |_| ()), Some(0));
        // Replacing slot 1 can allocate slot 0, but must retire slot 1 first.
        assert_eq!(
            rx.on_addba_request(P, 3, 6, true, 16, 20, 0xff, &mut |_| (), &mut retire),
            Addba::Accepted {
                slot: 0,
                window: 16
            },
        );
        assert_eq!(drain(&mut rx), [3]);
        // A declined replacement must retire the old pipe too.
        assert_eq!(
            rx.on_addba_request(P, 4, 6, false, 16, 20, 0xff, &mut |_| (), &mut retire),
            Addba::Declined,
        );
        assert_eq!(retired, [1, 0]);
        assert!(!rx.has_session(P, 6));
        assert_eq!(rx.end_session(P, 6, &mut |_| ()), None);
    }

    #[test]
    fn replies_survive_a_busy_transmitter_but_not_link_teardown() {
        let mut rx = RxBlockAck::<u16>::new();
        let add = |rx: &mut RxBlockAck<u16>, p, token| {
            rx.on_addba_request(
                [p; 6],
                token,
                3,
                true,
                16,
                0,
                0xff,
                &mut |_| (),
                &mut |_| (),
            )
        };
        let reply = |p, token| {
            Some(Outgoing::AddbaResponse {
                peer: [p; 6],
                tid: 3,
                token,
                status: 0,
                window: 16,
            })
        };
        for p in 1..=4 {
            add(&mut rx, p, p + 10);
        }
        assert_eq!(add(&mut rx, 5, 15), Addba::Busy);
        assert!(!rx.has_session([5; 6], 3));
        rx.end_peer([2; 6], &mut |_| ());
        add(&mut rx, 3, 23); // replace an unpublished agreement
        assert_eq!(rx.take_response(), reply(1, 11));
        assert_eq!(rx.take_response(), reply(4, 14));
        assert_eq!(rx.take_response(), reply(3, 23));
        assert_eq!(rx.take_response(), None);
        add(&mut rx, 1, 31);
        rx.end_session([1; 6], 3, &mut |_| ());
        assert_eq!(rx.response(), None);
    }

    #[test]
    fn sessions_of_different_peers_are_kept_apart() {
        let (a, b) = ([1; 6], [2; 6]);
        let mut rx = RxBlockAck::new();
        rx.on_addba_request(a, 1, 0, true, 16, 10, 0xff, &mut |_| (), &mut |_| ());
        rx.on_addba_request(b, 2, 0, true, 16, 50, 0xff, &mut |_| (), &mut |_| ());
        assert!(rx.has_session(a, 0) && rx.has_session(b, 0));
        assert_eq!(rx.admit(a, 0, 11, 11, 0, &mut |_| ()), None);
        assert_eq!(rx.admit(b, 0, 50, 50, 0, &mut |_| ()), None);
        assert_eq!(drain(&mut rx), [50]);
        assert_eq!(rx.end_session(b, 0, &mut |_| ()), Some(1));
        assert!(rx.has_session(a, 0) && !rx.has_session(b, 0));
        assert_eq!(rx.admit([3; 6], 0, 1, 1, 0, &mut |_| ()), Some(1));
    }
}
