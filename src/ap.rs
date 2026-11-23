//! Access point mode. The host driver sends START, then the beacon and
//! probe-response templates, then BEACON_TRANSMIT; from then on the MAC
//! sends the beacon template at every target beacon transmission time
//! (TBTT, a multiple of the beacon interval on the TSF).
//!
//! Each beacon is armed shortly before its TBTT: the descriptor in response
//! command 3, pointer slot 0 and the TBTT in MAC `0x0e08` (`mac::arm_beacon`).
//! The MAC reports the TBTT with event bit 26 (`tx::event`), after which the
//! next one is armed.

use crate::global::Global;
use core::cell::Cell;

/// How long before its TBTT a beacon is armed (vendor
/// `beacon_schedule_next`).
const PREPARE_US: u64 = 4_000;

/// Give up on the TBTT event this long after the TBTT.
const EVENT_TIMEOUT_US: u64 = 10_000;

/// TBTT events (MAC event bit 26) seen, counted by the event path.
pub(crate) static TBTT_EVENTS: Global<Cell<u32>> = Global::new(Cell::new(0));

/// The access point the host started.
pub struct AccessPoint {
    bss: Option<Bss>,
}

struct Bss {
    interface: u8,
    interval_us: u64,
    /// While beaconing: the beacon frame's packet-RAM buffer.
    beacon_buffer: Option<u32>,
    /// The beacon in the MAC: its TBTT, the TBTT event count when it was
    /// armed, and whether it announces buffered group frames.
    armed: Option<(u64, u32, bool)>,
    /// The host has not yet been told to release the advertised group frames.
    multicast_start_owed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApError {
    NotStarted,
    Join(crate::join::JoinError),
    /// The beacon buffer is in use by an internal frame.
    Busy,
}

impl Default for AccessPoint {
    fn default() -> Self {
        Self::new()
    }
}

impl AccessPoint {
    pub const fn new() -> Self {
        Self { bss: None }
    }

    /// START: become an access point on `interface`.
    #[cfg(target_arch = "arm")]
    pub fn start(
        &mut self,
        engine: &crate::tx::TxEngine,
        crypto: &mut crate::crypto::Crypto,
        radio: &mut crate::radio::Radio,
        scanning: bool,
        station_id: Option<[u8; 6]>,
        interface: u8,
        request: &crate::wsm::StartRequest<'_>,
    ) -> Result<(), ApError> {
        // P2P group owners (1) are not supported, and listen mode (2) is
        // `join::listen`; refusing them leaves a running access point alone.
        if request.mode != 0 || request.band != 0 {
            return Err(ApError::Join(crate::join::JoinError::Unsupported));
        }
        self.stop();
        crate::join::activate_ap(
            engine, crypto, radio, scanning, station_id, interface, request,
        )
        .map_err(ApError::Join)?;
        self.bss = Some(Bss {
            interface,
            interval_us: u64::from(request.beacon_interval.max(1)) * 1024,
            beacon_buffer: None,
            armed: None,
            multicast_start_owed: false,
        });
        Ok(())
    }

    /// BEACON_TRANSMIT: beacon or stop beaconing. Stopping is idempotent.
    #[cfg(target_arch = "arm")]
    pub fn set_beaconing(&mut self, enable: bool) -> Result<(), ApError> {
        let Some(bss) = self.bss.as_mut() else {
            return if enable {
                Err(ApError::NotStarted)
            } else {
                Ok(())
            };
        };
        match (enable, bss.beacon_buffer) {
            (true, None) => {
                let buffer = crate::tx::contexts()
                    .reserve_beacon_buffer()
                    .ok_or(ApError::Busy)?;
                bss.beacon_buffer = Some(buffer);
                crate::mac::start_beaconing(0);
            }
            (false, Some(_)) => bss.stop_beaconing(),
            _ => {}
        }
        Ok(())
    }

    /// RESET: no longer an access point.
    pub fn stop(&mut self) {
        if let Some(mut bss) = self.bss.take() {
            bss.stop_beaconing();
        }
    }

    /// Once per pass: arm the next beacon when its TBTT is near.
    #[cfg(target_arch = "arm")]
    pub fn service(&mut self, configuration: &mut crate::configuration::Configuration) {
        let Some(bss) = self.bss.as_mut() else {
            return;
        };
        let Some(buffer) = bss.beacon_buffer else {
            return;
        };
        let Some(tbtt) = bss.prepare_at(crate::mac::tsf(), TBTT_EVENTS.get()) else {
            return;
        };
        if let Some((true, multicast)) = configuration
            .with_beacon(tbtt / bss.interval_us, |template| {
                load_beacon(bss.interface, buffer, template)
            })
        {
            crate::mac::arm_beacon(tbtt);
            bss.armed = Some((tbtt, TBTT_EVENTS.get(), multicast));
        }
    }

    /// Whether a multicast-start indication is owed; `published` clears it.
    pub fn multicast_start_owed(&self) -> bool {
        self.bss
            .as_ref()
            .is_some_and(|bss| bss.multicast_start_owed)
    }

    pub fn multicast_start_published(&mut self) {
        if let Some(bss) = &mut self.bss {
            bss.multicast_start_owed = false;
        }
    }

    /// Microseconds until [`Self::service`] has work.
    #[cfg(target_arch = "arm")]
    pub fn wake_in(&self, has_beacon_template: bool) -> Option<u32> {
        let bss = self
            .bss
            .as_ref()
            .filter(|bss| bss.beacon_buffer.is_some())?;
        let tsf = crate::mac::tsf();
        let due = match bss.armed {
            Some((tbtt, ..)) => tbtt + EVENT_TIMEOUT_US,
            None if has_beacon_template => {
                next_tbtt(tsf, bss.interval_us).saturating_sub(PREPARE_US)
            }
            None => return None,
        };
        Some(due.saturating_sub(tsf).min(u64::from(u32::MAX)) as u32)
    }
}

impl Bss {
    fn prepare_at(&mut self, tsf: u64, events: u32) -> Option<u64> {
        if let Some((tbtt, armed_events, multicast)) = self.armed {
            if events != armed_events {
                self.multicast_start_owed |= multicast;
            } else if tsf < tbtt + EVENT_TIMEOUT_US {
                return None;
            } else {
                #[cfg(target_arch = "arm")]
                crate::mac::disarm_beacon();
            }
            self.armed = None;
        }
        let tbtt = next_tbtt(tsf, self.interval_us);
        (tbtt - tsf <= PREPARE_US).then_some(tbtt)
    }

    fn stop_beaconing(&mut self) {
        if self.beacon_buffer.take().is_some() {
            #[cfg(target_arch = "arm")]
            crate::mac::disarm_beacon();
            crate::tx::contexts().release_beacon_buffer();
        }
        self.armed = None;
        self.multicast_start_owed = false;
    }
}

/// The first TBTT after `tsf`: TBTTs are the multiples of the interval.
const fn next_tbtt(tsf: u64, interval_us: u64) -> u64 {
    tsf - tsf % interval_us + interval_us
}

/// Copy the beacon template's frame to `buffer` and describe it to the MAC.
#[cfg(target_arch = "arm")]
fn load_beacon(interface: u8, buffer: u32, template: &[u8]) -> bool {
    let frame = &template[4..];
    if frame.len() > crate::tx::INTERNAL_FRAME_CAPACITY {
        return false;
    }
    crate::tx::copy_to_packet_ram(buffer, frame);
    let sequence_slot = (crate::mac::low_mac().pas(interface).slot_bits.get() & 1) as u8;
    crate::mac::build_beacon_descriptor(
        buffer,
        frame.len() as u16,
        template[1],
        u16::from_le_bytes([frame[0], frame[1]]),
        sequence_slot,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multicast_belongs_to_one_beaconing_lifetime() {
        let mut b = Bss {
            interface: 0,
            interval_us: 102_400,
            beacon_buffer: None,
            armed: None,
            multicast_start_owed: false,
        };
        assert_eq!(b.prepare_at(102_399, 7), Some(102_400));
        b.armed = Some((102_400, 7, true));
        assert_eq!(b.prepare_at(102_401, 7), None);
        b.prepare_at(112_400, 7); // timeout, not an acknowledgment
        assert!(!b.multicast_start_owed);
        b.armed = Some((204_800, 7, true));
        b.prepare_at(204_801, 8);
        assert!(b.multicast_start_owed);
        b.stop_beaconing(); // output still blocked
        b.prepare_at(307_199, 8);
        assert!(!b.multicast_start_owed);
        b.armed = Some((307_200, 8, false));
        b.prepare_at(307_201, 9);
        assert!(!b.multicast_start_owed);
    }

    #[test]
    fn tbtts_are_the_multiples_of_the_interval() {
        assert_eq!(next_tbtt(0, 102_400), 102_400);
        assert_eq!(next_tbtt(102_399, 102_400), 102_400);
        assert_eq!(next_tbtt(102_400, 102_400), 204_800);
    }
}
