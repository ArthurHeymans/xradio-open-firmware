use super::*;

pub struct PreparedProbe {
    pub(crate) bytes: [u8; MAX_TEMPLATE_FRAME_LEN],
    pub(crate) length: usize,
    pub(crate) rate: u8,
}

impl PreparedProbe {
    pub(crate) const fn new() -> Self {
        Self {
            bytes: [0; MAX_TEMPLATE_FRAME_LEN],
            length: 0,
            rate: 0,
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublishedProbePublication {
    pub context: ContextId,
    pub pipe: u8,
    pub slot: u8,
    pub checksum: u32,
}

/// Software-owned pipe reservation with a fully written command list.
///
/// The slot points at the context, but neither the hardware ring nor the MAC
/// trigger has been published. Consuming this value through `cancel` restores
/// the slot and returns the context exactly once.
pub struct PreparedProbePublication {
    pub(crate) context: ContextId,
    pub(crate) pipe: u8,
    pub(crate) slot: u8,
    pub(crate) original_slot: SlotImage,
    pub(crate) checksum: u32,
    pub(crate) start_phy: bool,
}

impl PreparedProbePublication {
    /// Transfers the prepared frame to MAC ownership.
    ///
    /// # Safety
    /// The reservation and all pipe/MMIO state must remain exclusively owned;
    /// the event and completion services must run after publication.
    #[cfg(target_arch = "arm")]
    pub fn publish(
        self,
        backend: &mut SingleProbeMacBackend,
    ) -> Result<PublishedProbePublication, ProbeBuildError> {
        let pipes = pipes();
        if !single_frame_slot_matches(pipes.pipe(self.pipe).slot(self.slot), self.context) {
            let cancellation = self.cancel();
            return Err(cancellation
                .err()
                .unwrap_or(ProbeBuildError::UnsupportedPublicationShape));
        }

        let publication = PublishedProbePublication {
            context: self.context,
            pipe: self.pipe,
            slot: self.slot,
            checksum: self.checksum,
        };

        if !backend.register_publication(BatchPosition::Only, self.context, self.pipe, self.slot) {
            let cancellation = self.cancel();
            return Err(cancellation
                .err()
                .unwrap_or(ProbeBuildError::InvalidContextPointer));
        }
        if self.start_phy {
            start_phy_operation_1();
        }
        if pipes.pipe(self.pipe).hardware_ring(self.pipe).is_none() {
            let cancellation = self.cancel();
            return Err(cancellation
                .err()
                .unwrap_or(ProbeBuildError::PipeStateUnavailable));
        }
        // Vendor queue accounting counts the context before hardware
        // ownership; the completion drain performs the matching decrement.
        let contexts = contexts();
        contexts
            .active_pas
            .set(contexts.active_pas.get().wrapping_add(1));
        execute_single_probe_publication(
            &mut Mmio,
            pipes,
            SingleProbePublicationInput {
                pipe: self.pipe,
                slot: self.slot,
                context: self.context,
                batch: BatchPosition::Only,
            },
        );
        Ok(publication)
    }

    /// Cancels a reservation that has not been transferred to hardware.
    pub fn cancel(self) -> Result<u32, ProbeBuildError> {
        let slot = pipes().pipe(self.pipe).slot(self.slot);
        if slot.frame.get() != Some(self.context) {
            return Err(ProbeBuildError::PipeSlotOwnershipMismatch);
        }
        slot.restore(self.original_slot);
        contexts().free(self.context);
        Ok(self.checksum)
    }
}

/// Takes an internal context and prepares `probe` in its buffer, following
/// `tx_ctx_alloc_init(6, 0, 1)` and the probe-specific tail of `0x141b0`.
/// No queue or hardware ownership is transferred.
///
/// # Safety
/// The frame buffers and VIF/PAS tables must be initialized.
pub fn prepare_probe_context(
    probe: &PreparedProbe,
    if_id: u8,
) -> Result<ContextId, ProbeBuildError> {
    if probe.length > TX_BUFFER_SIZE - 0x40 || probe.length > usize::from(u16::MAX) {
        return Err(ProbeBuildError::FrameTooLarge);
    }
    let contexts = contexts();
    let id = contexts
        .allocate_internal()
        .ok_or(ProbeBuildError::ContextPoolEmpty)?;
    let context = id.get();
    let frame = TxFrameAddress::new(context.frame.get());
    copy_to_packet_ram(frame.raw(), probe.bytes());
    if !packet_ram_matches(frame.raw(), probe.bytes()) {
        contexts.free(id);
        return Err(ProbeBuildError::PacketRamMismatch);
    }
    let if_id = if_id.min(2);
    for word in 0..3 {
        let Some(value) = crate::vif::own_mac_word(if_id, word) else {
            contexts.free(id);
            return Err(ProbeBuildError::InvalidContextPointer);
        };
        write_u16(
            frame.address_2_halfword_unchecked(word as u32) as usize,
            value,
        );
    }

    // Ordinary foreground scans store explicit rate 0xff, after which
    // `lmc_tx_assign_default_rate` resolves the VIF default.
    let default_index = usize::from(crate::vif::rate_byte(if_id, 6).unwrap_or(0) != 0);
    let mut rate = crate::vif::default_rate(if_id, default_index).unwrap_or(0);
    if rate == 0xff || crate::vif::rate_byte(if_id, 7).unwrap_or(0) < 3 {
        rate = crate::vif::rate_byte(if_id, 4).unwrap_or(0);
    }
    let mut flags = 0x1000_u32;
    if rate & 0x80 != 0 {
        rate &= 0x7f;
    } else {
        flags |= 8;
    }
    if read_u32(frame.address_1() as usize) & 1 != 0 {
        flags |= 0x300;
    }

    context.class.set(CLASS_INTERNAL);
    context.frame_length.set(probe.length as u16);
    context
        .frame_control
        .set(read_u16(frame.frame_control() as usize));
    context.interface.set(if_id);
    context.host_link.set(0x0f);
    // Vendor sources the AC from a ROM-owned pointer; the mapped AC of queue
    // 0 is its value.
    context.access_category.set(QUEUE_TO_ACCESS_CATEGORY[0]);
    context.tx_rate.set(rate);
    context.retry_policy.set(0x0f);
    context.request_flag_rate_bits.set(0);
    context.flags.set(flags);
    context
        .duration_slot
        .set(crate::mac::low_mac().pas(if_id).slot_bits.get() & 1);
    context.try_count.set(0);
    context.auxiliary_state.set(0);
    if let Err(error) = prepare_single_frame_pas_timing(context) {
        contexts.free(id);
        return Err(error);
    }
    Ok(id)
}

/// Builds a software-owned pipe reservation without publishing hardware state.
///
/// # Safety
/// The retained template storage and the selected TX pipe must be exclusively
/// owned by the firmware runtime.
pub fn prepare_probe_publication(
    scratch: &mut PreparedProbe,
    template: Option<&[u8]>,
    ssid: &[u8],
    channel: u8,
    if_id: u8,
) -> Result<PreparedProbePublication, ProbeBuildError> {
    let probe = prepare_probe_into(scratch, template, ssid, channel)?;
    let context = prepare_probe_context(probe, if_id)?;
    prepare_context_publication(context)
}

pub(crate) fn prepare_context_publication(
    id: ContextId,
) -> Result<PreparedProbePublication, ProbeBuildError> {
    let context = id.get();
    let Some(&pipe) = ACCESS_CATEGORY_TO_QUEUE.get(usize::from(context.access_category.get()))
    else {
        contexts().free(id);
        return Err(ProbeBuildError::PipeStateUnavailable);
    };
    let record = pipes().pipe(pipe);
    if record.hardware_ring(pipe).is_none() {
        contexts().free(id);
        return Err(ProbeBuildError::PipeStateUnavailable);
    }
    let slot = record.producer.get() & 3;
    let command = TxDescriptorAddress::new(packet_ram::tx_command(
        usize::from(pipe),
        usize::from(slot),
    ) as u32);
    // `0xa712` stores the frame's response class in the slot; success handler
    // `0x9cdc` requires it to be `0xff` for a frame expecting no response
    // before entering the release loop.
    let original_slot = record.slot(slot).reserve(id, context.frame_kind.get());
    write_u32(command.word_unchecked(0) as usize, 0);
    write_u32(command.word_unchecked(1) as usize, 0);
    write_u32(command.word_unchecked(2) as usize, 0xdc00_0000);
    let checksum = match emit_descriptor(context, command.word_unchecked(3)) {
        Ok(checksum) => checksum,
        Err(error) => {
            record.slot(slot).restore(original_slot);
            contexts().free(id);
            return Err(error);
        }
    };
    Ok(PreparedProbePublication {
        context: id,
        pipe,
        slot,
        original_slot,
        checksum,
        start_phy: true,
    })
}

/// Copies a host frame over a freshly prepared internal context.
#[cfg(target_arch = "arm")]
fn load_host_frame(id: ContextId, frame: &[u8]) -> Result<(), ProbeBuildError> {
    let address = id.get().frame.get();
    copy_to_packet_ram(address, frame);
    if packet_ram_matches(address, frame) {
        Ok(())
    } else {
        contexts().free(id);
        Err(ProbeBuildError::PacketRamMismatch)
    }
}

/// Load a host frame and, if it is protected, seal it in place: the engine
/// reaches only packet RAM. The context is marked as having no crypto work
/// left for the MAC.
#[cfg(target_arch = "arm")]
fn load_protected_host_frame(
    id: ContextId,
    frame: &[u8],
    crypto: &mut crate::crypto::Crypto,
    if_id: u8,
) -> Result<(), ProbeBuildError> {
    load_host_frame(id, frame)?;
    let frame_control = u16::from_le_bytes([frame[0], frame[1]]);
    if frame_control & 0x400c != 0x4008 {
        return Ok(());
    }
    let context = id.get();
    // SAFETY: the context was just taken for this frame and owns its
    // packet-RAM buffer; nothing else touches it until it is published.
    let loaded =
        unsafe { core::slice::from_raw_parts_mut(context.frame.get() as *mut u8, frame.len()) };
    if crypto.encrypt_now(loaded, if_id).is_err() {
        contexts().free(id);
        return Err(ProbeBuildError::CryptoFailure);
    }
    context.terminal_status.set(0x0010);
    Ok(())
}

/// The request fields every host management frame takes over.
#[cfg(target_arch = "arm")]
fn apply_host_request(context: &TxContext, request: &crate::wsm::TxRequest<'_>, mut tx_flags: u32) {
    // The AC is selected through the four-entry WSM queue map, not the raw
    // WSM queue id. The WSM TX flags carry the PTA priority and the
    // retry-policy selector.
    context
        .access_category
        .set(QUEUE_TO_ACCESS_CATEGORY[usize::from(request.queue_id & 3)]);
    context
        .request_flag_rate_bits
        .set((request.flags & 0x0f) >> 1);
    context.retry_policy.set((request.flags & 0x7f) >> 4);
    // These frames use an internal context rather than the WSM class-0 pool;
    // link slot 1 is the validated internal slot for them.
    context.host_link.set(1);
    if request.max_tx_rate < 22 {
        context.tx_rate.set(request.max_tx_rate);
    }
    if request.frame.get(4).is_some_and(|octet| octet & 1 != 0) {
        tx_flags |= 0x300;
    }
    if request.ht_tx_parameters & 3 == 1 {
        tx_flags |= 8;
    }
    if request.flags & 1 != 0 {
        tx_flags |= 0x0001_0000;
    }
    tx_flags |= (request.ht_tx_parameters >> 11) & 0xe0;
    context.flags.set(tx_flags);
}

/// Prepare one host-supplied unicast management or EAPOL frame for the
/// single-context MAC publication path.
///
/// # Safety
/// JOIN must own the selected VIF/channel and no scan probe or host frame may
/// currently own the shared context/pipe backend.
#[cfg(target_arch = "arm")]
pub(crate) fn prepare_legacy_control_publication(
    scratch: &mut PreparedProbe,
    crypto: &mut crate::crypto::Crypto,
    request: &crate::wsm::TxRequest<'_>,
    if_id: u8,
) -> Result<PreparedProbePublication, ProbeBuildError> {
    if if_id > 1
        || !crate::vif::is_active(if_id)
        || (!request.is_unicast_management() && !request.is_unicast_eapol())
    {
        return Err(ProbeBuildError::InvalidInterface);
    }
    if request.frame.len() > MAX_TEMPLATE_FRAME_LEN {
        return Err(ProbeBuildError::FrameTooLarge);
    }

    scratch.bytes[..request.frame.len()].copy_from_slice(request.frame);
    scratch.length = request.frame.len();
    scratch.rate = request.max_tx_rate;

    let id = prepare_probe_context(scratch, if_id)?;
    // EAPOL after the keys are in (rekeying) is protected like any data.
    load_protected_host_frame(id, request.frame, crypto, if_id)?;
    let context = id.get();
    apply_host_request(context, request, 0x0080_1000);
    if let Err(error) = prepare_single_frame_pas_timing(context) {
        contexts().free(id);
        return Err(error);
    }
    prepare_context_publication(id)
}

#[cfg(target_arch = "arm")]
pub(crate) fn prepare_host_management_publication(
    scratch: &mut PreparedProbe,
    crypto: &mut crate::crypto::Crypto,
    request: &crate::wsm::TxRequest<'_>,
    if_id: u8,
) -> Result<PreparedProbePublication, ProbeBuildError> {
    if request.is_unicast_management() || request.is_unicast_eapol() {
        return prepare_legacy_control_publication(scratch, crypto, request, if_id);
    }
    if if_id > 1 || !crate::vif::is_active(if_id) {
        return Err(ProbeBuildError::InvalidInterface);
    }
    let control = request.is_unicast_control();
    if !request.is_unicast_data() && !request.is_unicast_null_data() && !control {
        return Err(ProbeBuildError::WrongTemplateType);
    }
    if request.frame.len() > MAX_TEMPLATE_FRAME_LEN {
        return Err(ProbeBuildError::FrameTooLarge);
    }

    scratch.bytes[..request.frame.len()].copy_from_slice(request.frame);
    scratch.length = request.frame.len();
    scratch.rate = request.max_tx_rate;

    let id = prepare_probe_context(scratch, if_id)?;
    // Probe preparation patches the source address; host frames already carry
    // their final addresses and are restored byte for byte.
    let prepared_frame = &scratch.bytes[..scratch.length];
    load_protected_host_frame(id, prepared_frame, crypto, if_id)?;
    let frame_control = u16::from_le_bytes([prepared_frame[0], prepared_frame[1]]);
    let context = id.get();
    // `tx_lmac_req_submit` seeds bit 23, then `tx_classify_hdr_len` adds the
    // direct-frame marker and multicast/no-ACK classification.
    let mut tx_flags = 0x0080_1000_u32;
    if !control && classify_dot11_header(frame_control, false).qos_data {
        tx_flags |= 0x0040_0000;
    }
    if control {
        // A BlockAckReq is answered with a compressed BlockAck after SIFS
        // (`frame_kind` 0x0c). Publishing it with no expected response leaves
        // it uncompleted and stalls the host's TID queue behind it.
        tx_flags |= 0x4000;
    }
    apply_host_request(context, request, tx_flags);
    // Recompute the airtime after replacing the probe template's rate, flags
    // and header with the host-supplied frame.
    if let Err(error) = prepare_single_frame_pas_timing(context) {
        contexts().free(id);
        return Err(error);
    }
    prepare_context_publication(id)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActiveProbeReport {
    Idle,
    Published {
        opportunity: crate::scan::ProbeOpportunity,
        publication: PublishedProbePublication,
    },
    Servicing,
    /// The publisher is busy with a management frame: the probe waits for
    /// the scan's next pass.
    Deferred {
        opportunity: crate::scan::ProbeOpportunity,
    },
    Completed {
        opportunity: crate::scan::ProbeOpportunity,
        context: ContextId,
        status: u16,
    },
    Failed {
        opportunity: crate::scan::ProbeOpportunity,
        error: ProbeBuildError,
    },
}

#[cfg(target_arch = "arm")]
pub(crate) struct ActiveProbeRuntime {
    pub(crate) backend: SingleProbeMacBackend,
    pub(crate) published: Option<PublishedProbePublication>,
    pub(crate) opportunity: Option<crate::scan::ProbeOpportunity>,
    pub(crate) host_published: Option<PublishedProbePublication>,
    pub(crate) host_owner: ManagementOwner,
    pub(crate) host_rate: u8,
    pub(crate) completed_count: u32,
    pub(crate) diagnostic: u16,
}

#[cfg(target_arch = "arm")]
impl ActiveProbeRuntime {
    pub(crate) const fn new() -> Self {
        Self {
            backend: SingleProbeMacBackend::new(),
            published: None,
            opportunity: None,
            host_published: None,
            host_owner: ManagementOwner::BlockAck,
            host_rate: 0,
            completed_count: 0,
            diagnostic: 0,
        }
    }
}

/// Scan-owned active probe transmission. Publications are serialized through
/// complete hardware return before another scan opportunity can publish.
///
/// # Safety
/// The caller must own probe preparation, event FIFO and completion servicing.
#[cfg(target_arch = "arm")]
pub fn service_active_probe(
    engine: &mut TxEngine,
    template: Option<&[u8]>,
    opportunity: Option<crate::scan::ProbeOpportunity>,
    ssid: &[u8],
    max_events: u32,
) -> ActiveProbeReport {
    let runtime = &mut engine.probe;
    if runtime.host_published.is_some() {
        // Hand the opportunity back: dropped, it left the scan waiting for a
        // probe that never went out.
        return opportunity.map_or(ActiveProbeReport::Servicing, |opportunity| {
            ActiveProbeReport::Deferred { opportunity }
        });
    }
    if let Some(published) = runtime.published {
        let Some(completion) = service_single_probe_runtime(&mut runtime.backend, max_events)
        else {
            runtime.diagnostic = 0x2000;
            return ActiveProbeReport::Servicing;
        };
        let context = completion.context;
        let status = completion.status;
        if context != published.context {
            terminal_probe_backend_fault(published.pipe);
        }
        let Some(opportunity) = runtime.opportunity.take() else {
            terminal_probe_backend_fault(published.pipe);
        };
        runtime.published = None;
        runtime.completed_count = runtime.completed_count.saturating_add(1);

        runtime.diagnostic =
            0x3000 | ((runtime.completed_count.min(0x0f) as u16) << 8) | (status & 0x00ff);
        return ActiveProbeReport::Completed {
            opportunity,
            context,
            status,
        };
    }

    let Some(opportunity) = opportunity else {
        return ActiveProbeReport::Idle;
    };

    let prepared = match {
        prepare_probe_publication(
            &mut engine.scratch,
            template,
            ssid,
            opportunity.channel,
            opportunity.if_id,
        )
    } {
        Ok(prepared) => prepared,
        Err(error) => {
            runtime.diagnostic = 0xf000 | error as u16;
            return ActiveProbeReport::Failed { opportunity, error };
        }
    };
    let published = match prepared.publish(&mut runtime.backend) {
        Ok(published) => published,
        Err(error) => {
            runtime.diagnostic = 0xf100 | error as u16;
            return ActiveProbeReport::Failed { opportunity, error };
        }
    };
    runtime.published = Some(published);
    runtime.opportunity = Some(opportunity);
    runtime.diagnostic = 0x1000 | u16::from(published.pipe) | (u16::from(published.slot) << 4);
    ActiveProbeReport::Published {
        opportunity,
        publication: published,
    }
}

/// Whose frame the management publisher sends: the host's, which it
/// confirms, or one of the firmware's own, whose completion stays in the
/// firmware.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagementOwner {
    Host { packet_id: u32 },
    BlockAck,
    PowerSave,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostManagementTxReport {
    Idle,
    Published {
        packet_id: u32,
    },
    Servicing,
    Completed {
        owner: ManagementOwner,
        status: u32,
        tx_rate: u8,
        ack_failures: u8,
    },
    Failed {
        packet_id: u32,
        error: ProbeBuildError,
    },
}

/// Publish or service one host-supplied authentication/association frame.
/// The shared backend admits exactly one hardware-owned context at a time.
///
/// # Safety
/// The caller must exclusively service the MAC event FIFO and completion ring.
#[cfg(target_arch = "arm")]
pub fn host_management_runtime_active(engine: &TxEngine) -> bool {
    let runtime = &engine.probe;
    runtime.host_published.is_some() || runtime.published.is_some()
}

#[cfg(target_arch = "arm")]
pub fn service_host_management_tx(
    engine: &mut TxEngine,
    crypto: &mut crate::crypto::Crypto,
    request: Option<(&crate::wsm::TxRequest<'_>, u8, ManagementOwner)>,
    max_events: u32,
) -> HostManagementTxReport {
    let runtime = &mut engine.probe;
    if let Some(published) = runtime.host_published {
        if let Some((request, ..)) = request {
            return HostManagementTxReport::Failed {
                packet_id: request.packet_id,
                error: ProbeBuildError::PipeSlotBusy,
            };
        }
        let Some(completion) = service_single_probe_runtime(&mut runtime.backend, max_events)
        else {
            return HostManagementTxReport::Servicing;
        };
        let context = completion.context;
        let completion_status = completion.status;
        if context != published.context {
            terminal_probe_backend_fault(published.pipe);
        }
        runtime.host_published = None;
        let status = wsm_status_from_internal(completion_status);
        return HostManagementTxReport::Completed {
            owner: runtime.host_owner,
            status,
            tx_rate: runtime.host_rate,
            ack_failures: completion.ack_failures,
        };
    }

    let Some((request, if_id, owner)) = request else {
        if runtime.published.is_some() {
            return HostManagementTxReport::Servicing;
        }
        // Vendor source 0x16 services `mac_irq_handler()` continuously from
        // FIQ, including between host transmissions. Cooperatively drain the
        // FIFO and completion task here as well, otherwise beacon/radio/pipe
        // events remain stale until the next class-6 publication.
        let _ = service_single_probe_runtime(&mut runtime.backend, max_events);
        return HostManagementTxReport::Idle;
    };
    if runtime.published.is_some() {
        return HostManagementTxReport::Failed {
            packet_id: request.packet_id,
            error: ProbeBuildError::PipeSlotBusy,
        };
    }
    let prepared =
        match prepare_host_management_publication(&mut engine.scratch, crypto, request, if_id) {
            Ok(prepared) => prepared,
            Err(error) => {
                return HostManagementTxReport::Failed {
                    packet_id: request.packet_id,
                    error,
                };
            }
        };
    let published = match prepared.publish(&mut runtime.backend) {
        Ok(published) => published,
        Err(error) => {
            return HostManagementTxReport::Failed {
                packet_id: request.packet_id,
                error,
            };
        }
    };
    runtime.host_published = Some(published);
    runtime.host_owner = owner;
    runtime.host_rate = request.max_tx_rate;
    HostManagementTxReport::Published {
        packet_id: request.packet_id,
    }
}

/// Tell the AP a station sleeps (`pm`) or is awake, with a null-data frame
/// at the lowest basic rate.
#[cfg(target_arch = "arm")]
pub fn publish_power_save_null(
    engine: &mut TxEngine,
    crypto: &mut crate::crypto::Crypto,
    if_id: u8,
    pm: bool,
) -> bool {
    use core::cell::Cell;
    let Some(vif) = crate::vif::vif(if_id) else {
        return false;
    };
    let frame = crate::power_save::null_data(
        vif.bssid.each_ref().map(Cell::get),
        vif.own_mac.each_ref().map(Cell::get),
        pm,
    );
    let request = crate::wsm::TxRequest {
        packet_id: 0,
        max_tx_rate: vif.rate_bytes[4].get(),
        queue_id: 3,
        more: false,
        flags: 0,
        expire_time: 0,
        ht_tx_parameters: 0,
        frame: &frame,
    };
    matches!(
        service_host_management_tx(
            engine,
            crypto,
            Some((&request, if_id, ManagementOwner::PowerSave)),
            0
        ),
        HostManagementTxReport::Published { .. }
    )
}

/// Rate index of 24 Mbit/s OFDM.
const BLOCK_ACK_REQUEST_RATE: u8 = 10;

/// Hand a firmware BlockAck frame to the management publisher. An ADDBA
/// request to `recipient` starts its session at `ssn`, a BlockAckReq the
/// recipient's window. A response goes to the requester it names.
#[cfg(target_arch = "arm")]
pub fn publish_block_ack_frame(
    engine: &mut TxEngine,
    crypto: &mut crate::crypto::Crypto,
    frame: ba::Outgoing,
    if_id: u8,
    recipient: [u8; 6],
    ssn: u16,
) -> bool {
    use core::cell::Cell;
    let Some(vif) = crate::vif::vif(if_id) else {
        return false;
    };
    let own = vif.own_mac.each_ref().map(Cell::get);
    let bssid = vif.bssid.each_ref().map(Cell::get);
    let mut bytes = [0; 33];
    // WSM queues: 0 BE, 1 BK, 2 VI, 3 VO. Session setup goes on VO; a
    // BlockAckReq on its TID's access category, as mac80211 sends it.
    let (length, queue_id) = match frame {
        ba::Outgoing::AddbaRequest { tid, token } => {
            bytes = ba::addba_request(recipient, own, bssid, token, tid, ssn);
            (bytes.len(), 3)
        }
        ba::Outgoing::BlockAckRequest { tid, .. } => {
            bytes[..20].copy_from_slice(&ba::block_ack_request(recipient, own, tid, ssn));
            (20, [0, 1, 1, 0, 2, 2, 3, 3][usize::from(tid & 7)])
        }
        ba::Outgoing::AddbaResponse {
            peer: requester,
            tid,
            token,
            status,
            window,
        } => {
            bytes = ba::addba_response(requester, own, bssid, token, status, tid, window);
            (bytes.len(), 3)
        }
    };
    // A BlockAckReq goes at 24 Mbit/s, mandatory for every OFDM station, and
    // is answered at that rate: about 80 us on air instead of 850 us at the
    // 1 Mbit/s management rate.
    let rate = match frame {
        ba::Outgoing::AddbaRequest { .. } | ba::Outgoing::AddbaResponse { .. } => {
            vif.rate_bytes[4].get()
        }
        ba::Outgoing::BlockAckRequest { .. } => BLOCK_ACK_REQUEST_RATE,
    };
    let request = crate::wsm::TxRequest {
        packet_id: 0,
        max_tx_rate: rate,
        queue_id,
        more: false,
        flags: 0,
        expire_time: 0,
        ht_tx_parameters: 0,
        frame: &bytes[..length],
    };
    matches!(
        service_host_management_tx(
            engine,
            crypto,
            Some((&request, if_id, ManagementOwner::BlockAck)),
            0
        ),
        HostManagementTxReport::Published { .. }
    )
}

#[cfg(target_arch = "arm")]
pub fn probe_runtime_quiescent(engine: &TxEngine) -> bool {
    let runtime = &engine.probe;
    runtime.published.is_none()
        && runtime.host_published.is_none()
        && contexts().internal_in_use() == 0
        && contexts().active_pas.get() == 0
        && COMPLETION_RING.is_empty()
        && pipes().active_tx.get() == 0
}

#[cfg(target_arch = "arm")]
pub fn fatal_scan_stop_timeout() -> ! {
    terminal_probe_backend_fault(0)
}

#[cfg(target_arch = "arm")]
pub fn active_probe_diagnostic_word(engine: &TxEngine) -> u16 {
    engine.probe.diagnostic
}

#[cfg(not(target_arch = "arm"))]
pub const fn active_probe_diagnostic_word(_engine: &TxEngine) -> u16 {
    0
}

/// Builds the 802.11 probe request portion of vendor
/// `syn_scan_build_probe_req` (`0x141b0`). The four-byte WSM template header is
/// consumed, the requested SSID is substituted, and every DS Parameter Set IE
/// is rewritten for the active channel.
pub(crate) fn prepare_probe_into<'a>(
    prepared: &'a mut PreparedProbe,
    template: Option<&[u8]>,
    ssid: &[u8],
    channel: u8,
) -> Result<&'a PreparedProbe, ProbeBuildError> {
    if ssid.len() > 32 {
        return Err(ProbeBuildError::SsidTooLong);
    }
    let template = template.ok_or(ProbeBuildError::MissingTemplate)?;
    if template.len() < 4 {
        return Err(ProbeBuildError::MalformedTemplate);
    }
    if template[0] != 0 {
        return Err(ProbeBuildError::WrongTemplateType);
    }
    let rate = template[1];
    let declared = usize::from(u16::from_le_bytes([template[2], template[3]]));
    if declared < 24 || declared != template.len() - 4 {
        return Err(ProbeBuildError::MalformedTemplate);
    }
    let frame = &template[4..];
    let bytes = &mut prepared.bytes;
    bytes[..24].copy_from_slice(&frame[..24]);
    let mut output = 24;
    let mut input = 24;
    let mut wrote_ssid = false;
    while input < frame.len() {
        if input + 2 > frame.len() {
            return Err(ProbeBuildError::MalformedTemplate);
        }
        let id = frame[input];
        let length = usize::from(frame[input + 1]);
        let next = input
            .checked_add(2 + length)
            .ok_or(ProbeBuildError::MalformedTemplate)?;
        if next > frame.len() {
            return Err(ProbeBuildError::MalformedTemplate);
        }
        if id == 0 {
            if !wrote_ssid {
                let end = output + 2 + ssid.len();
                if end > bytes.len() {
                    return Err(ProbeBuildError::FrameTooLarge);
                }
                bytes[output] = 0;
                bytes[output + 1] = ssid.len() as u8;
                bytes[output + 2..end].copy_from_slice(ssid);
                output = end;
                wrote_ssid = true;
            }
        } else if id == 3 {
            if output + 3 > bytes.len() {
                return Err(ProbeBuildError::FrameTooLarge);
            }
            bytes[output..output + 3].copy_from_slice(&[3, 1, channel]);
            output += 3;
        } else {
            let encoded = &frame[input..next];
            let end = output + encoded.len();
            if end > bytes.len() {
                return Err(ProbeBuildError::FrameTooLarge);
            }
            bytes[output..end].copy_from_slice(encoded);
            output = end;
        }
        input = next;
    }
    if !wrote_ssid {
        let end = output + 2 + ssid.len();
        if end > bytes.len() {
            return Err(ProbeBuildError::FrameTooLarge);
        }
        bytes[output] = 0;
        bytes[output + 1] = ssid.len() as u8;
        bytes[output + 2..end].copy_from_slice(ssid);
        output = end;
    }
    prepared.length = output;
    prepared.rate = rate;
    Ok(prepared)
}
