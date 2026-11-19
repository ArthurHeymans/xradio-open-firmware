//! Key commands commit only after pre-command TX owners can no longer emit.
//! Expiry rejects the command; it never authorizes a key change on a live pipe.

use crate::crypto::Crypto;
use crate::wsm::{self, AddKeyRequest, RemoveKeyRequest, STATUS_FAILURE, STATUS_SUCCESS};

pub(super) const DRAIN_US: u32 = 1_000_000;

pub(super) enum KeyChange<'a> {
    Add(Option<AddKeyRequest<'a>>),
    Remove(Option<u8>),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Progress {
    Waiting,
    Complete(u32),
}

impl<'a> KeyChange<'a> {
    pub fn parse(id: u16, payload: &'a [u8]) -> Option<Self> {
        match id {
            wsm::ADD_KEY_REQ_ID => Some(Self::Add(AddKeyRequest::parse(payload).ok())),
            wsm::REMOVE_KEY_REQ_ID => Some(Self::Remove(RemoveKeyRequest::parse(payload).ok().map(|key| key.index))),
            _ => None,
        }
    }

    pub fn progress(self, crypto: &mut Crypto, quiet: bool, since: u32, now: u32) -> Progress {
        // Deadline wins even if TX became quiet while reply storage was full.
        // A stale request must never commit after its host may have given up.
        if now.wrapping_sub(since) >= DRAIN_US || matches!(self, Self::Add(None) | Self::Remove(None)) {
            return Progress::Complete(STATUS_FAILURE);
        }
        if !quiet {
            return Progress::Waiting;
        }
        let status = match self {
            Self::Add(Some(key)) => crypto.add_key(0, &key).map_or(STATUS_FAILURE, |()| STATUS_SUCCESS),
            Self::Remove(Some(index)) => {
                crypto.remove_key(index);
                STATUS_SUCCESS
            }
            _ => STATUS_FAILURE,
        };
        Progress::Complete(status)
    }
}

/// Confirmation-only owners cannot emit again. Every other retained owner,
/// including software-only and reserved frames, must retire before commit.
pub(crate) fn owners_drained(occupied: u32, confirming: u32) -> bool {
    occupied & !confirming == 0
}

pub(super) fn is_key_change(id: u16) -> bool {
    matches!(id, wsm::ADD_KEY_REQ_ID | wsm::REMOVE_KEY_REQ_ID)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{CcmpError, Started};

    fn add_payload(material: u8) -> [u8; 44] {
        let mut payload = [0; 44];
        payload[0] = 5; // AES_PAIRWISE
        payload[4..10].fill(2);
        payload[12..28].fill(material);
        payload
    }

    fn step(crypto: &mut Crypto, id: u16, payload: &[u8], quiet: bool, since: u32, now: u32) -> Progress {
        KeyChange::parse(id, payload)
            .unwrap()
            .progress(crypto, quiet, since, now)
    }

    fn installed(material: u8) -> Crypto {
        let mut crypto = Crypto::new();
        assert_eq!(
            step(&mut crypto, wsm::ADD_KEY_REQ_ID, &add_payload(material), true, 0, 0),
            Progress::Complete(0)
        );
        crypto
    }

    fn frame() -> [u8; 54] {
        let mut frame = [0; 54];
        frame[..2].copy_from_slice(&0x4188_u16.to_le_bytes());
        frame[4..10].fill(2);
        frame[10..16].fill(4);
        frame[16..22].fill(2);
        frame[34..46].copy_from_slice(b"hello world!");
        frame
    }

    fn sealed(crypto: &mut Crypto) -> [u8; 54] {
        let mut frame = frame();
        crypto.encrypt_now(&mut frame, 0).unwrap();
        frame
    }

    #[test]
    fn every_nonconfirming_owner_blocks_key_changes() {
        assert!(owners_drained(0, 0));
        for index in 0..32 {
            let owner = 1 << index;
            assert!(!owners_drained(owner, 0));
            assert!(owners_drained(owner, owner));
            assert!(!owners_drained(owner | (1 << ((index + 1) % 32)), owner));
        }
    }

    #[test]
    fn replacement_waits_without_changing_key_or_consuming_packet_numbers() {
        let mut crypto = installed(0x11);
        let mut old = installed(0x11);
        let new = add_payload(0x22);
        for now in [0, 10, DRAIN_US - 2] {
            assert_eq!(
                step(&mut crypto, wsm::ADD_KEY_REQ_ID, &new, false, 0, now),
                Progress::Waiting
            );
            assert_eq!(sealed(&mut crypto), sealed(&mut old));
        }
        assert_eq!(
            step(&mut crypto, wsm::ADD_KEY_REQ_ID, &new, true, 0, DRAIN_US - 1),
            Progress::Complete(0)
        );
        old.add_key(0, &AddKeyRequest::parse(&new).unwrap()).unwrap();
        assert_eq!(sealed(&mut crypto), sealed(&mut old));
    }

    #[test]
    fn deadline_rejects_replacement_and_retains_old_key() {
        let mut crypto = installed(0x11);
        assert_eq!(
            step(&mut crypto, wsm::ADD_KEY_REQ_ID, &add_payload(0x22), false, 0, DRAIN_US),
            Progress::Complete(STATUS_FAILURE)
        );
        let mut expected = installed(0x11);
        assert_eq!(sealed(&mut crypto), sealed(&mut expected));
        // Recovery is a new request, not a late commit of the failed one.
        assert_eq!(
            step(
                &mut crypto,
                wsm::ADD_KEY_REQ_ID,
                &add_payload(0x22),
                true,
                DRAIN_US,
                DRAIN_US + 1
            ),
            Progress::Complete(0)
        );
        expected
            .add_key(0, &AddKeyRequest::parse(&add_payload(0x22)).unwrap())
            .unwrap();
        assert_eq!(sealed(&mut crypto), sealed(&mut expected));
    }

    #[test]
    fn remove_waits_and_timeout_does_not_revoke_the_old_key() {
        let mut crypto = installed(0x11);
        for (now, expected) in [(1, Progress::Waiting), (DRAIN_US, Progress::Complete(STATUS_FAILURE))] {
            assert_eq!(
                step(&mut crypto, wsm::REMOVE_KEY_REQ_ID, &[0; 4], false, 0, now),
                expected
            );
        }
        assert_eq!(sealed(&mut crypto), sealed(&mut installed(0x11)));
        assert_eq!(
            step(&mut crypto, wsm::REMOVE_KEY_REQ_ID, &[0; 4], true, DRAIN_US, DRAIN_US),
            Progress::Complete(0)
        );
        assert_eq!(crypto.encrypt_now(&mut frame(), 0), Err(CcmpError::MissingKey));
    }

    #[test]
    fn group_selection_changes_only_after_old_transmitters_retire() {
        let group = |slot: u8, id: u8, material: u8| {
            let mut payload = [0; 44];
            payload[0] = 4; // AES_GROUP
            payload[1] = slot;
            payload[4..20].fill(material);
            payload[20] = id;
            payload
        };
        let seal_group = |crypto: &mut Crypto| {
            let mut packet = frame();
            packet[..2].copy_from_slice(&0x4288_u16.to_le_bytes()); // FromDS multicast.
            packet[4..10].fill(0xff);
            crypto.encrypt_now(&mut packet, 0).unwrap();
            packet
        };
        let mut crypto = Crypto::new();
        let mut expected = Crypto::new();
        let old = group(0, 1, 0x11);
        let new = group(1, 2, 0x22);
        for engine in [&mut crypto, &mut expected] {
            engine.add_key(0, &AddKeyRequest::parse(&old).unwrap()).unwrap();
        }
        assert_eq!(
            step(&mut crypto, wsm::ADD_KEY_REQ_ID, &new, false, 0, 1),
            Progress::Waiting
        );
        assert_eq!(seal_group(&mut crypto), seal_group(&mut expected));
        assert_eq!(
            step(&mut crypto, wsm::ADD_KEY_REQ_ID, &new, true, 0, 2),
            Progress::Complete(0)
        );
        expected.add_key(0, &AddKeyRequest::parse(&new).unwrap()).unwrap();
        assert_eq!(seal_group(&mut crypto), seal_group(&mut expected));
        assert_eq!(
            step(&mut crypto, wsm::REMOVE_KEY_REQ_ID, &[0; 4], true, 0, 3),
            Progress::Complete(0)
        );
        // Removing the overlapped old GTK must not undo the new selection.
        assert_eq!(seal_group(&mut crypto), seal_group(&mut expected));
    }

    #[test]
    fn invalid_installation_retains_live_key_and_allows_a_later_request() {
        let mut crypto = installed(0x11);
        let mut invalid = add_payload(0x22);
        invalid[0] = 0xff;
        assert_eq!(
            step(&mut crypto, wsm::ADD_KEY_REQ_ID, &invalid, true, 0, 1),
            Progress::Complete(STATUS_FAILURE)
        );
        assert_eq!(sealed(&mut crypto), sealed(&mut installed(0x11)));
        assert_eq!(
            step(&mut crypto, wsm::ADD_KEY_REQ_ID, &add_payload(0x22), true, 1, 2),
            Progress::Complete(0)
        );
    }

    #[test]
    fn expired_request_cannot_commit_after_reply_backpressure_clears() {
        let add = add_payload(0x22);
        for (id, payload) in [(wsm::ADD_KEY_REQ_ID, &add[..]), (wsm::REMOVE_KEY_REQ_ID, &[0; 4][..])] {
            let mut crypto = installed(0x11);
            assert_eq!(
                step(&mut crypto, id, payload, false, 0, DRAIN_US - 1),
                Progress::Waiting
            );
            // Reply capacity may return only after both the deadline and TX
            // completion. This must not turn a stale request into a late commit.
            assert_eq!(
                step(&mut crypto, id, payload, true, 0, DRAIN_US + 1),
                Progress::Complete(STATUS_FAILURE)
            );
            assert_eq!(sealed(&mut crypto), sealed(&mut installed(0x11)));
        }
    }

    #[test]
    fn malformed_commands_fail_without_waiting_for_a_busy_pipe() {
        for id in [wsm::ADD_KEY_REQ_ID, wsm::REMOVE_KEY_REQ_ID] {
            let mut crypto = installed(0x11);
            assert_eq!(
                step(&mut crypto, id, &[], false, 0, 0),
                Progress::Complete(STATUS_FAILURE)
            );
            assert_eq!(sealed(&mut crypto), sealed(&mut installed(0x11)));
        }
    }

    #[test]
    fn timer_wrap_does_not_extend_the_drain_deadline() {
        let mut crypto = installed(0x11);
        let since = u32::MAX - 10;
        let new = add_payload(0x22);
        assert_eq!(
            step(
                &mut crypto,
                wsm::ADD_KEY_REQ_ID,
                &new,
                false,
                since,
                since.wrapping_add(DRAIN_US - 1)
            ),
            Progress::Waiting
        );
        assert_eq!(
            step(
                &mut crypto,
                wsm::ADD_KEY_REQ_ID,
                &new,
                false,
                since,
                since.wrapping_add(DRAIN_US)
            ),
            Progress::Complete(STATUS_FAILURE)
        );
        assert_eq!(sealed(&mut crypto), sealed(&mut installed(0x11)));
    }

    #[test]
    fn receive_job_does_not_block_commit_or_reauthorize_stale_plaintext() {
        let mut crypto = installed(0x11);
        let mut sender_payload = add_payload(0x11);
        sender_payload[4..10].fill(4);
        let mut sender = Crypto::new();
        sender
            .add_key(0, &AddKeyRequest::parse(&sender_payload).unwrap())
            .unwrap();
        let mut incoming = frame();
        incoming[4..10].fill(4);
        incoming[10..16].fill(2);
        sender.encrypt_now(&mut incoming, 0).unwrap();
        // The frame stays alive and untouched until the job is collected.
        let Started::Running(job) = (unsafe { crypto.start_rx(&mut incoming, 0) }).unwrap() else {
            panic!("RX job");
        };
        let epoch = job.rx_key_epoch().unwrap();
        assert_eq!(
            step(&mut crypto, wsm::ADD_KEY_REQ_ID, &add_payload(0x22), true, 0, 0),
            Progress::Complete(0)
        );
        assert!(matches!(crypto.poll(job), Ok(Ok(()))));
        assert!(!crypto.rx_key_epoch_is_active(0, epoch));
        assert!(!crypto.accept_rx_packet_number(0, &incoming, epoch));
    }
}
