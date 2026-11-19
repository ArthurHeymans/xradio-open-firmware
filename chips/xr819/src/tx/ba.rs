//! TX BlockAck sessions, originator side (IEEE 802.11 BACK action frames).
//!
//! Upstream cw1200 leaves TX BlockAck to the firmware (`TX_AMPDU_SETUP_IN_HW`),
//! so the firmware negotiates its own sessions:
//!
//! - A session is requested when the first QoS data frame of a TID the host
//!   allows (MIB 0x100e) is admitted. Like the vendor, we offer a window of 16.
//! - The recipient's ADDBA response and any DELBA it sends for our session are
//!   consumed here and never reach the host. ADDBA requests and DELBAs for the
//!   peer's own sessions (our RX side) pass through.
//! - When a frame of an established session gives up, a BlockAckReq moves the
//!   recipient's window past it. A lost frame then costs one frame instead of
//!   holding the recipient's reorder buffer until it aliases.

use core::cell::Cell;

/// Largest window we offer; the vendor offers the same.
pub const WINDOW: u16 = 16;
const TIDS: usize = 8;
/// An ADDBA request without a response is resent after this long.
const RESPONSE_TIMEOUT_US: u32 = 1_000_000;
/// Requests sent before a TID gives up until the next join.
const ATTEMPTS: u8 = 3;

const CATEGORY_BLOCK_ACK: u8 = 3;
const ACTION_ADDBA_REQUEST: u8 = 0;
const ACTION_ADDBA_RESPONSE: u8 = 1;
const ACTION_DELBA: u8 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Session {
    Idle,
    /// An ADDBA request is queued (`sent_at` is `None`) or awaiting its response.
    Requested { token: u8, attempts: u8, sent_at: Option<u32> },
    Established { window: u16 },
    /// Refused or never answered; not retried before the next join.
    Refused,
}

/// A frame the firmware wants on air.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outgoing {
    AddbaRequest { tid: u8, token: u8 },
    BlockAckRequest { tid: u8, ssn: u16 },
    /// Our answer to `peer`'s ADDBA request (see `rx_ba`); a window of 0
    /// with a failure status declines it.
    AddbaResponse { peer: [u8; 6], tid: u8, token: u8, status: u16, window: u16 },
}

impl Outgoing {
    pub const fn tid(self) -> u8 {
        match self {
            Self::AddbaRequest { tid, .. }
            | Self::BlockAckRequest { tid, .. }
            | Self::AddbaResponse { tid, .. } => tid,
        }
    }
}

/// What a received frame means for our sessions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Received {
    AddbaResponse { token: u8, status: u16, tid: u8, buffer_size: u16 },
    /// The recipient tore down our session.
    Delba { tid: u8 },
    /// The peer wants to send us aggregates.
    AddbaRequest { token: u8, tid: u8, immediate: bool, buffer_size: u16, ssn: u16 },
    /// The peer ended its session to us.
    PeerDelba { tid: u8 },
}

/// TX BlockAck sessions per peer link (see [`crate::vif::LINKS`]). One
/// firmware BlockAck frame is in flight at a time.
pub struct PeerSessions {
    links: [BlockAckSessions; crate::vif::LINKS],
}

impl Default for PeerSessions {
    fn default() -> Self {
        Self::new()
    }
}

impl PeerSessions {
    pub const fn new() -> Self {
        Self { links: [const { BlockAckSessions::new() }; crate::vif::LINKS] }
    }

    /// The sessions with the peer on `link`.
    pub fn link(&self, link: u8) -> &BlockAckSessions {
        &self.links[usize::from(link) % crate::vif::LINKS]
    }

    pub fn reset(&self) {
        self.links.iter().for_each(BlockAckSessions::reset);
    }

    pub fn wake_in(&self, now: u32) -> Option<u32> {
        self.links.iter().filter_map(|link| link.wake_in(now)).min()
    }

    /// The next frame to send and its peer's link.
    pub fn outgoing(&self, now: u32) -> Option<(u8, Outgoing)> {
        (0..).zip(&self.links).find_map(|(link, sessions)| Some((link, sessions.outgoing(now)?)))
    }

    /// The frame in flight completed at `now`.
    pub fn completed(&self, now: u32) {
        self.links.iter().for_each(|link| link.completed(now));
    }
}

pub struct BlockAckSessions {
    sessions: [Cell<Session>; TIDS],
    /// Frame handed to the MAC and not completed yet.
    in_flight: Cell<Option<Outgoing>>,
    /// BlockAckReq owed per TID, named by the sequence just past the latest
    /// frame that gave up. The frame itself starts at the originator's
    /// window start, chosen when it is sent.
    bar: [Cell<Option<u16>>; TIDS],
    next_token: Cell<u8>,
}

impl Default for BlockAckSessions {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockAckSessions {
    pub const fn new() -> Self {
        Self {
            sessions: [const { Cell::new(Session::Idle) }; TIDS],
            in_flight: Cell::new(None),
            bar: [const { Cell::new(None) }; TIDS],
            next_token: Cell::new(1),
        }
    }

    /// Forget every session; the association they belonged to is gone.
    pub fn reset(&self) {
        self.sessions.iter().for_each(|session| session.set(Session::Idle));
        self.in_flight.set(None);
        self.bar.iter().for_each(|bar| bar.set(None));
    }

    pub fn session(&self, tid: u8) -> Session {
        self.sessions.get(usize::from(tid)).map_or(Session::Refused, Cell::get)
    }

    /// Window of an established session.
    pub fn window(&self, tid: u8) -> Option<u16> {
        match self.session(tid) {
            Session::Established { window } => Some(window),
            _ => None,
        }
    }

    /// A QoS data frame of `tid` was admitted; `allowed` is the host's TX
    /// BlockAck TID mask.
    pub fn on_qos_data(&self, tid: u8, allowed: u8) {
        if tid as usize >= TIDS || allowed & (1 << tid) == 0 {
            return;
        }
        if self.session(tid) == Session::Idle {
            let token = self.next_token.get();
            self.next_token.set(token.wrapping_add(1).max(1));
            self.sessions[usize::from(tid)].set(Session::Requested { token, attempts: 0, sent_at: None });
        }
    }

    /// Microseconds until [`Self::outgoing`] resends or abandons a request
    /// that got no response.
    pub fn wake_in(&self, now: u32) -> Option<u32> {
        self.sessions
            .iter()
            .filter_map(|session| match session.get() {
                Session::Requested { sent_at: Some(sent_at), .. } => {
                    Some(crate::deadline::until(sent_at.wrapping_add(RESPONSE_TIMEOUT_US), now))
                }
                _ => None,
            })
            .min()
    }

    /// The next frame to send, if any; BlockAckReqs first. Also resends or
    /// abandons requests whose response did not arrive in time.
    pub fn outgoing(&self, now: u32) -> Option<Outgoing> {
        for session in &self.sessions {
            if let Session::Requested { token, attempts, sent_at: Some(sent_at) } = session.get()
                && now.wrapping_sub(sent_at) >= RESPONSE_TIMEOUT_US
            {
                session.set(if attempts >= ATTEMPTS {
                    Session::Refused
                } else {
                    Session::Requested { token, attempts, sent_at: None }
                });
            }
        }
        let bar = (0..).zip(&self.bar).find_map(|(tid, bar)| {
            bar.get().map(|ssn| Outgoing::BlockAckRequest { tid, ssn })
        });
        bar.or_else(|| {
            (0..).zip(&self.sessions).find_map(|(tid, session)| match session.get() {
                Session::Requested { token, sent_at: None, .. } => {
                    Some(Outgoing::AddbaRequest { tid, token })
                }
                _ => None,
            })
        })
    }

    /// `frame` was handed to the MAC.
    pub fn published(&self, frame: Outgoing) {
        self.in_flight.set(Some(frame));
    }

    /// The frame handed to the MAC completed at `now`.
    pub fn completed(&self, now: u32) {
        if let Some(frame) = self.in_flight.take() {
            self.sent(frame, now);
        }
    }

    /// `frame` went on air at `now` (acknowledged or not), or could not be sent.
    pub fn sent(&self, frame: Outgoing, now: u32) {
        match frame {
            Outgoing::AddbaRequest { tid, token } => {
                if let Session::Requested { token: pending, attempts, sent_at: None } = self.session(tid)
                    && pending == token
                {
                    self.sessions[usize::from(tid)].set(Session::Requested {
                        token,
                        attempts: attempts + 1,
                        sent_at: Some(now),
                    });
                }
            }
            Outgoing::BlockAckRequest { tid, ssn } => {
                let bar = &self.bar[usize::from(tid)];
                if bar.get() == Some(ssn) {
                    bar.set(None);
                }
            }
            Outgoing::AddbaResponse { .. } => {}
        }
    }

    /// Apply a frame from the recipient. The peer's own sessions to us are
    /// `rx_ba`'s.
    pub fn on_received(&self, frame: Received) {
        match frame {
            Received::AddbaResponse { token, status, tid, buffer_size } => {
                if let Session::Requested { token: pending, .. } = self.session(tid)
                    && pending == token
                {
                    self.sessions[usize::from(tid)].set(if status == 0 {
                        // A zero buffer size lets the originator pick.
                        let window = if buffer_size == 0 { WINDOW } else { buffer_size.min(WINDOW) };
                        Session::Established { window }
                    } else {
                        Session::Refused
                    });
                }
            }
            Received::Delba { tid } => {
                if let Some(session) = self.sessions.get(usize::from(tid)) {
                    session.set(Session::Idle);
                    self.bar[usize::from(tid)].set(None);
                }
            }
            Received::AddbaRequest { .. } | Received::PeerDelba { .. } => {}
        }
    }

    /// A BlockAckReq for `tid` waits to be sent or is on its way.
    pub fn bar_pending(&self, tid: u8) -> bool {
        self.bar.get(usize::from(tid)).is_some_and(|bar| bar.get().is_some())
    }

    /// A frame with sequence number `sequence` of `tid` gave up. Within a
    /// session, owe the recipient a BlockAckReq moving its window past it.
    pub fn on_given_up(&self, tid: u8, sequence: u16) {
        if self.window(tid).is_some() {
            let ssn = sequence.wrapping_add(1) & 0x0fff;
            let bar = &self.bar[usize::from(tid)];
            // Keep the later start: it also releases the earlier hole.
            let later = bar.get().is_none_or(|pending| ssn.wrapping_sub(pending) & 0x0fff < 0x0800);
            if later {
                bar.set(Some(ssn));
            }
        }
    }
}

/// ADDBA response to the peer's request `token`: immediate policy, no
/// A-MSDU, `window` frames, no inactivity timeout.
pub fn addba_response(
    receiver: [u8; 6],
    transmitter: [u8; 6],
    bssid: [u8; 6],
    token: u8,
    status: u16,
    tid: u8,
    window: u16,
) -> [u8; 33] {
    let mut frame = [0; 33];
    frame[0] = 0xd0; // management, action
    frame[4..10].copy_from_slice(&receiver);
    frame[10..16].copy_from_slice(&transmitter);
    frame[16..22].copy_from_slice(&bssid);
    let parameters = (1 << 1) | (u16::from(tid & 0x0f) << 2) | (window << 6);
    frame[24] = CATEGORY_BLOCK_ACK;
    frame[25] = ACTION_ADDBA_RESPONSE;
    frame[26] = token;
    frame[27..29].copy_from_slice(&status.to_le_bytes());
    frame[29..31].copy_from_slice(&parameters.to_le_bytes());
    frame
}

/// ADDBA request (action frame, not robust, so never protected).
pub fn addba_request(
    receiver: [u8; 6],
    transmitter: [u8; 6],
    bssid: [u8; 6],
    token: u8,
    tid: u8,
    ssn: u16,
) -> [u8; 33] {
    let mut frame = [0; 33];
    frame[0] = 0xd0; // management, action
    frame[4..10].copy_from_slice(&receiver);
    frame[10..16].copy_from_slice(&transmitter);
    frame[16..22].copy_from_slice(&bssid);
    // Immediate policy, no A-MSDU, our window.
    let parameters = (1 << 1) | (u16::from(tid & 0x0f) << 2) | (WINDOW << 6);
    frame[24] = CATEGORY_BLOCK_ACK;
    frame[25] = ACTION_ADDBA_REQUEST;
    frame[26] = token;
    frame[27..29].copy_from_slice(&parameters.to_le_bytes());
    // Timeout 0: no inactivity timeout.
    frame[31..33].copy_from_slice(&((ssn & 0x0fff) << 4).to_le_bytes());
    frame
}

/// Compressed BlockAckReq. The recipient answers with a BlockAck, which the
/// MAC reports as the frame's response status (0x0c).
pub fn block_ack_request(receiver: [u8; 6], transmitter: [u8; 6], tid: u8, ssn: u16) -> [u8; 20] {
    let mut frame = [0; 20];
    frame[0] = 0x84; // control, BlockAckReq
    frame[4..10].copy_from_slice(&receiver);
    frame[10..16].copy_from_slice(&transmitter);
    // Normal Ack, compressed bitmap.
    let control = (1 << 2) | (u16::from(tid & 0x0f) << 12);
    frame[16..18].copy_from_slice(&control.to_le_bytes());
    frame[18..20].copy_from_slice(&((ssn & 0x0fff) << 4).to_le_bytes());
    frame
}

/// Recognize a BlockAck action frame for one of our sessions, sent to `own`
/// by `peer`. Other frames, including the peer's ADDBA requests and DELBAs
/// for its own sessions, are left for the host.
pub fn parse(frame: &[u8], own: [u8; 6], peer: [u8; 6]) -> Option<Received> {
    let frame_control = u16::from_le_bytes([*frame.first()?, *frame.get(1)?]);
    // Unprotected management action frame from the peer to us.
    if frame_control & 0x40fc != 0x00d0
        || frame.get(4..10)? != own
        || frame.get(10..16)? != peer
        || *frame.get(24)? != CATEGORY_BLOCK_ACK
    {
        return None;
    }
    let u16_at = |offset: usize| Some(u16::from_le_bytes([*frame.get(offset)?, *frame.get(offset + 1)?]));
    match *frame.get(25)? {
        ACTION_ADDBA_RESPONSE => {
            let parameters = u16_at(29)?;
            Some(Received::AddbaResponse {
                token: *frame.get(26)?,
                status: u16_at(27)?,
                tid: ((parameters >> 2) & 0x0f) as u8,
                buffer_size: parameters >> 6,
            })
        }
        ACTION_ADDBA_REQUEST => {
            let parameters = u16_at(27)?;
            Some(Received::AddbaRequest {
                token: *frame.get(26)?,
                tid: ((parameters >> 2) & 0x0f) as u8,
                immediate: parameters & (1 << 1) != 0,
                buffer_size: parameters >> 6,
                ssn: u16_at(31)? >> 4,
            })
        }
        ACTION_DELBA => {
            let parameters = u16_at(26)?;
            let tid = (parameters >> 12) as u8;
            // Initiator clear: the sender is the recipient, so the session
            // is our TX session; set: it is the peer's session to us.
            Some(if parameters & (1 << 11) == 0 { Received::Delba { tid } } else { Received::PeerDelba { tid } })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: [u8; 6] = [0x12, 0x42, 0x2a, 0x37, 0x70, 0x07];
    const PEER: [u8; 6] = [0x98, 0x5f, 0x41, 0x18, 0x76, 0x17];

    fn action(action: u8, body: &[u8]) -> std::vec::Vec<u8> {
        let mut frame = std::vec![0xd0, 0, 0, 0];
        frame.extend_from_slice(&OWN);
        frame.extend_from_slice(&PEER);
        frame.extend_from_slice(&PEER);
        frame.extend_from_slice(&[0, 0, CATEGORY_BLOCK_ACK, action]);
        frame.extend_from_slice(body);
        frame
    }

    #[test]
    fn frames_match_the_vendor_exchange() {
        // Vendor request on air: 03 00 01 0304 0000 1000 (TID 0, window 16, SSN 256),
        // except that we do not claim A-MSDU support (bit 0).
        let request = addba_request(PEER, OWN, PEER, 1, 0, 256);
        assert_eq!(&request[24..], &[3, 0, 1, 0x02, 0x04, 0, 0, 0x00, 0x10]);
        let bar = block_ack_request(PEER, OWN, 5, 0x123);
        assert_eq!(&bar[16..], &[0x04, 0x50, 0x30, 0x12]);

        // The AP's response on air: 03 01 01 0000 0304 0000.
        let response = action(1, &[1, 0, 0, 0x03, 0x04, 0, 0]);
        assert_eq!(
            parse(&response, OWN, PEER),
            Some(Received::AddbaResponse { token: 1, status: 0, tid: 0, buffer_size: 16 })
        );
        // The AP's DELBA for its own session (initiator set) ends our RX session.
        assert_eq!(parse(&action(2, &[0x00, 0x78, 0x25, 0]), OWN, PEER), Some(Received::PeerDelba { tid: 7 }));
        assert_eq!(parse(&action(2, &[0x00, 0x50, 0x25, 0]), OWN, PEER), Some(Received::Delba { tid: 5 }));
        // The AP's own request: A-MSDU and immediate policy, TID 0, window 64, SSN 20.
        assert_eq!(
            parse(&action(0, &[1, 0x03, 0x10, 0, 0, 0x40, 0x01]), OWN, PEER),
            Some(Received::AddbaRequest { token: 1, tid: 0, immediate: true, buffer_size: 64, ssn: 20 })
        );
        let answer = addba_response(PEER, OWN, PEER, 1, 0, 0, 16);
        assert_eq!(&answer[24..], &[3, 1, 1, 0, 0, 0x02, 0x04, 0, 0]);
        // Frames from anyone else go to the host.
        assert_eq!(parse(&response, OWN, OWN), None);
    }

    #[test]
    fn session_lifecycle() {
        let sessions = BlockAckSessions::new();
        sessions.on_qos_data(0, 0x3e);
        assert_eq!(sessions.session(0), Session::Idle, "TID 0 not allowed");
        sessions.on_qos_data(1, 0x3f);
        let request = sessions.outgoing(0).unwrap();
        let Outgoing::AddbaRequest { tid: 1, .. } = request else { panic!("{request:?}") };
        sessions.sent(request, 10);
        assert_eq!(sessions.outgoing(20), None);

        // Unanswered: resent, then abandoned.
        assert_eq!(sessions.outgoing(10 + RESPONSE_TIMEOUT_US), Some(request));
        sessions.sent(request, 2 * RESPONSE_TIMEOUT_US);
        assert_eq!(sessions.outgoing(3 * RESPONSE_TIMEOUT_US), Some(request));
        sessions.sent(request, 3 * RESPONSE_TIMEOUT_US);
        assert_eq!(sessions.outgoing(4 * RESPONSE_TIMEOUT_US), None);
        assert_eq!(sessions.session(1), Session::Refused);

        sessions.reset();
        sessions.on_qos_data(1, 0xff);
        let request = sessions.outgoing(0).unwrap();
        let Outgoing::AddbaRequest { token, .. } = request else { panic!() };
        sessions.sent(request, 0);
        // A stale token is ignored; the matching one establishes, window capped at ours.
        sessions.on_received(Received::AddbaResponse { token: token + 1, status: 0, tid: 1, buffer_size: 64 });
        assert!(matches!(sessions.session(1), Session::Requested { .. }));
        sessions.on_received(Received::AddbaResponse { token, status: 0, tid: 1, buffer_size: 64 });
        assert_eq!(sessions.window(1), Some(WINDOW));

        // Give-ups queue the later BlockAckReq; the recipient's DELBA ends it all.
        sessions.on_given_up(1, 0xffe);
        sessions.on_given_up(1, 0xfff);
        sessions.on_given_up(1, 0xffd);
        let bar = sessions.outgoing(0).unwrap();
        assert_eq!(bar, Outgoing::BlockAckRequest { tid: 1, ssn: 0 });
        sessions.sent(bar, 0);
        assert_eq!(sessions.outgoing(0), None);
        sessions.on_given_up(1, 7);
        sessions.on_received(Received::Delba { tid: 1 });
        assert_eq!((sessions.session(1), sessions.outgoing(0)), (Session::Idle, None));
    }
}
