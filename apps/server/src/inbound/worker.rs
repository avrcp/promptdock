use std::time::Duration;

use rand::Rng as _;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    InboundCommandService, InboundCommandV5, InboundWorkerError, RunFilter,
    model::{
        ClaimedInboundCommand, ConfirmationActionKind, ControlDispatchOutcome, PendingConfirmation,
        PendingJobPageSelection, RenderedInboundReply, SelectionEntry, SelectionItemKind,
        SelectionLookupError,
    },
    renderer::{
        ConsoleDevice, render_control_error, render_device_selected,
        render_device_selection_required, render_devices, render_gateway_error, render_help,
        render_interrupted_restart, render_job_detail, render_job_page, render_job_tree,
        render_selection_error, render_unknown,
    },
    repository::unix_timestamp_ms,
};
use crate::{
    auth::{DeviceAuthService, DeviceScope, sanitize_device_display_name},
    gateway::{
        GatewayRuntime, RemoteRunFilterV5, RemoteRunPhaseV5, RunQueryActionV5, RunQueryResultV5,
        protocol::{
            CatalogQueryActionV5, CatalogQueryResultV5, ControlActionV5, ControlResultV5,
            GatewayCapabilityV5, WorkspaceSensitivityV5,
        },
    },
    outbox::{InteractiveReplyV1, OutboxService},
};

const IDLE_POLL: Duration = Duration::from_millis(250);
const RECOVERY_SCAN: Duration = Duration::from_secs(60);
const REPLY_PRIORITY: i64 = 200;
const JOB_PAGE_SIZE: u16 = 10;

enum DeviceResolution {
    Selected(ConsoleDevice),
    Offline,
    SelectionRequired,
}

pub struct InboundCommandWorker {
    service: InboundCommandService,
    outbox: OutboxService,
    devices: DeviceAuthService,
    gateway: GatewayRuntime,
    verifier: super::ConfirmationVerifier,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorkerPass {
    Idle,
    Applied,
    Interrupted,
}

impl InboundCommandWorker {
    #[cfg(test)]
    pub fn new(
        service: InboundCommandService,
        outbox: OutboxService,
        devices: DeviceAuthService,
        gateway: GatewayRuntime,
    ) -> Self {
        Self::new_with_verifier(
            service,
            outbox,
            devices,
            gateway,
            super::ConfirmationVerifier::for_test(),
        )
    }

    pub(crate) fn new_with_verifier(
        service: InboundCommandService,
        outbox: OutboxService,
        devices: DeviceAuthService,
        gateway: GatewayRuntime,
        verifier: super::ConfirmationVerifier,
    ) -> Self {
        Self {
            service,
            outbox,
            devices,
            gateway,
            verifier,
        }
    }

    pub async fn run(self, cancellation: CancellationToken) -> Result<(), InboundWorkerError> {
        self.run_with_progress(cancellation, || {}).await
    }

    pub(crate) async fn run_with_progress<F>(
        self,
        cancellation: CancellationToken,
        completed_pass: F,
    ) -> Result<(), InboundWorkerError>
    where
        F: Fn() + Send + 'static,
    {
        if cancellation.is_cancelled() {
            return Ok(());
        }
        self.service.recover_stale_claims().await?;
        let mut recovery = tokio::time::interval(RECOVERY_SCAN);
        recovery.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        recovery.tick().await;
        loop {
            if cancellation.is_cancelled() {
                return Ok(());
            }
            match self.run_pass(cancellation.clone()).await? {
                WorkerPass::Applied => {
                    completed_pass();
                    continue;
                }
                WorkerPass::Idle => completed_pass(),
                WorkerPass::Interrupted if cancellation.is_cancelled() => return Ok(()),
                WorkerPass::Interrupted => {}
            }
            tokio::select! {
                biased;
                () = cancellation.cancelled() => return Ok(()),
                () = self.service.wake().notified() => {}
                _ = recovery.tick() => { self.service.recover_stale_claims().await?; }
                () = tokio::time::sleep(IDLE_POLL) => {}
            }
        }
    }

    #[cfg(test)]
    pub(super) async fn run_once(
        &self,
        cancellation: CancellationToken,
    ) -> Result<bool, InboundWorkerError> {
        self.run_pass(cancellation)
            .await
            .map(|pass| pass == WorkerPass::Applied)
    }

    async fn run_pass(
        &self,
        cancellation: CancellationToken,
    ) -> Result<WorkerPass, InboundWorkerError> {
        if cancellation.is_cancelled() {
            return Ok(WorkerPass::Interrupted);
        }
        let now = unix_timestamp_ms()?;
        let Some(claim) = self.service.claim_next_at(now, &cancellation).await? else {
            return Ok(if cancellation.is_cancelled() {
                WorkerPass::Interrupted
            } else {
                WorkerPass::Idle
            });
        };
        if cancellation.is_cancelled() {
            let _ = self
                .service
                .release_claim_at(&claim, unix_timestamp_ms()?)
                .await?;
            return Ok(WorkerPass::Interrupted);
        }
        let command = match serde_json::from_str::<InboundCommandV5>(&claim.command_json) {
            Ok(command) if command.kind() == claim.command_kind => command,
            _ => {
                let applied = self
                    .service
                    .dead_letter_at(&claim, "INVALID_CLOSED_COMMAND", unix_timestamp_ms()?)
                    .await?;
                return Ok(if applied {
                    WorkerPass::Applied
                } else {
                    WorkerPass::Interrupted
                });
            }
        };

        let rendered = if claim.last_error_code.as_deref() == Some("GATEWAY_INTERRUPTED_RESTART") {
            render_interrupted_restart()
        } else {
            match self.execute_command(&claim, command, &cancellation).await? {
                Some(reply) => reply,
                None => return Ok(WorkerPass::Interrupted),
            }
        };
        self.queue_reply(&claim, rendered, &cancellation).await
    }

    async fn execute_command(
        &self,
        claim: &ClaimedInboundCommand,
        command: InboundCommandV5,
        cancellation: &CancellationToken,
    ) -> Result<Option<RenderedInboundReply>, InboundWorkerError> {
        match command {
            InboundCommandV5::Help => Ok(Some(render_help())),
            InboundCommandV5::Unknown => Ok(Some(render_unknown())),
            InboundCommandV5::ListDevices => {
                let devices = self.online_query_devices().await?;
                let entries = devices
                    .iter()
                    .enumerate()
                    .map(|(index, device)| SelectionEntry {
                        slot: u16::try_from(index + 1).expect("device selection is bounded"),
                        device_id: device.id,
                        client_opaque_handle: None,
                        item_kind: SelectionItemKind::Device,
                    })
                    .collect::<Vec<_>>();
                let selected = (devices.len() == 1).then(|| devices[0].id);
                self.service
                    .replace_selection_at(
                        &claim.sender_fingerprint,
                        selected,
                        &entries,
                        unix_timestamp_ms()?,
                    )
                    .await?;
                Ok(Some(render_devices(&devices)))
            }
            InboundCommandV5::SelectDevice { slot } => {
                let selected = self
                    .service
                    .select_device_slot_at(&claim.sender_fingerprint, slot, unix_timestamp_ms()?)
                    .await?;
                match selected {
                    Ok(device_id) => {
                        let name = self.device_name(device_id).await?;
                        Ok(Some(render_device_selected(&name)))
                    }
                    Err(error) => Ok(Some(render_selection_error(matches!(
                        error,
                        SelectionLookupError::Expired
                    )))),
                }
            }
            InboundCommandV5::ListRuntimes => {
                let device = match self
                    .resolve_catalog_device(&claim.sender_fingerprint)
                    .await?
                {
                    DeviceResolution::Selected(device) => device,
                    DeviceResolution::Offline => {
                        return Ok(Some(render_gateway_error(
                            crate::gateway::GatewayRequestError::Offline,
                        )));
                    }
                    DeviceResolution::SelectionRequired => {
                        return Ok(Some(render_device_selection_required()));
                    }
                };
                let Some(result) = self
                    .dispatch_catalog(
                        claim,
                        device.id,
                        CatalogQueryActionV5::ListRuntimes,
                        cancellation,
                    )
                    .await?
                else {
                    return Ok(None);
                };
                let result = match result {
                    Ok(CatalogQueryResultV5::ListRuntimes(page)) => page,
                    Ok(_) => {
                        return Ok(Some(render_gateway_error(
                            crate::gateway::GatewayRequestError::ProtocolMismatch,
                        )));
                    }
                    Err(error) => return Ok(Some(render_gateway_error(error))),
                };
                let truncated = result.items.len() > super::repository::MAX_SELECTION_ENTRIES;
                let items = result
                    .items
                    .iter()
                    .take(super::repository::MAX_SELECTION_ENTRIES)
                    .map(|item| super::renderer::SafeCatalogItem {
                        handle: item.runtime_handle.clone(),
                        label: item.label.clone(),
                        sensitivity: None,
                        enabled: Some(item.ready),
                        remote_start_enabled: None,
                    })
                    .collect::<Vec<_>>();
                let entries = items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| SelectionEntry {
                        slot: u16::try_from(index + 1).expect("catalog selection is bounded"),
                        device_id: device.id,
                        client_opaque_handle: Some(item.handle.clone()),
                        item_kind: SelectionItemKind::Runtime,
                    })
                    .collect::<Vec<_>>();
                self.service
                    .replace_catalog_selection_at(
                        &claim.sender_fingerprint,
                        device.id,
                        "runtimes",
                        &entries,
                        unix_timestamp_ms()?,
                    )
                    .await?;
                Ok(Some(super::renderer::render_catalog(
                    "runtime", &items, truncated,
                )))
            }
            InboundCommandV5::SelectRuntime { slot } => {
                let selected = self
                    .service
                    .select_catalog_slot_at(
                        &claim.sender_fingerprint,
                        slot,
                        SelectionItemKind::Runtime,
                        unix_timestamp_ms()?,
                    )
                    .await?;
                match selected {
                    Ok(_) => Ok(Some(super::renderer::render_catalog_selected(
                        "runtime",
                        &format!("运行时 {slot}"),
                    ))),
                    Err(error) => Ok(Some(render_selection_error(matches!(
                        error,
                        SelectionLookupError::Expired
                    )))),
                }
            }
            InboundCommandV5::ListWorkspaces => {
                let device = match self
                    .resolve_catalog_device(&claim.sender_fingerprint)
                    .await?
                {
                    DeviceResolution::Selected(device) => device,
                    DeviceResolution::Offline => {
                        return Ok(Some(render_gateway_error(
                            crate::gateway::GatewayRequestError::Offline,
                        )));
                    }
                    DeviceResolution::SelectionRequired => {
                        return Ok(Some(render_device_selection_required()));
                    }
                };
                let Some(result) = self
                    .dispatch_catalog(
                        claim,
                        device.id,
                        CatalogQueryActionV5::ListWorkspaces,
                        cancellation,
                    )
                    .await?
                else {
                    return Ok(None);
                };
                let result = match result {
                    Ok(CatalogQueryResultV5::ListWorkspaces(page)) => page,
                    Ok(_) => {
                        return Ok(Some(render_gateway_error(
                            crate::gateway::GatewayRequestError::ProtocolMismatch,
                        )));
                    }
                    Err(error) => return Ok(Some(render_gateway_error(error))),
                };
                let truncated = result.items.len() > super::repository::MAX_SELECTION_ENTRIES;
                let items = result
                    .items
                    .iter()
                    .take(super::repository::MAX_SELECTION_ENTRIES)
                    .map(|item| super::renderer::SafeCatalogItem {
                        handle: item.workspace_handle.clone(),
                        label: item.label.clone(),
                        sensitivity: Some(
                            match item.sensitivity {
                                WorkspaceSensitivityV5::Public => "公开",
                                WorkspaceSensitivityV5::InternalSafe => "内部安全",
                                WorkspaceSensitivityV5::Confidential => "机密",
                            }
                            .to_owned(),
                        ),
                        enabled: None,
                        remote_start_enabled: Some(item.remote_start_enabled),
                    })
                    .collect::<Vec<_>>();
                let entries = items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| SelectionEntry {
                        slot: u16::try_from(index + 1).expect("catalog selection is bounded"),
                        device_id: device.id,
                        client_opaque_handle: Some(item.handle.clone()),
                        item_kind: SelectionItemKind::Workspace,
                    })
                    .collect::<Vec<_>>();
                self.service
                    .replace_catalog_selection_at(
                        &claim.sender_fingerprint,
                        device.id,
                        "workspaces",
                        &entries,
                        unix_timestamp_ms()?,
                    )
                    .await?;
                Ok(Some(super::renderer::render_catalog(
                    "workspace",
                    &items,
                    truncated,
                )))
            }
            InboundCommandV5::SelectWorkspace { slot } => {
                let selected = self
                    .service
                    .select_catalog_slot_at(
                        &claim.sender_fingerprint,
                        slot,
                        SelectionItemKind::Workspace,
                        unix_timestamp_ms()?,
                    )
                    .await?;
                match selected {
                    Ok(_) => Ok(Some(super::renderer::render_catalog_selected(
                        "workspace",
                        &format!("工作区 {slot}"),
                    ))),
                    Err(error) => Ok(Some(render_selection_error(matches!(
                        error,
                        SelectionLookupError::Expired
                    )))),
                }
            }
            InboundCommandV5::ListHarnessProfiles => {
                let context = match self
                    .service
                    .selected_catalog_context_at(&claim.sender_fingerprint, unix_timestamp_ms()?)
                    .await?
                {
                    Ok(context) => context,
                    Err(error) => {
                        return Ok(Some(render_selection_error(matches!(
                            error,
                            SelectionLookupError::Expired
                        ))));
                    }
                };
                let (Some(runtime_handle), Some(workspace_handle)) =
                    (context.runtime_handle, context.workspace_handle)
                else {
                    return Ok(Some(render_selection_error(false)));
                };
                let Some(result) = self
                    .dispatch_catalog(
                        claim,
                        context.device_id,
                        CatalogQueryActionV5::ListHarnessProfiles {
                            runtime_handle,
                            workspace_handle,
                        },
                        cancellation,
                    )
                    .await?
                else {
                    return Ok(None);
                };
                let result = match result {
                    Ok(CatalogQueryResultV5::ListHarnessProfiles(page)) => page,
                    Ok(_) => {
                        return Ok(Some(render_gateway_error(
                            crate::gateway::GatewayRequestError::ProtocolMismatch,
                        )));
                    }
                    Err(error) => return Ok(Some(render_gateway_error(error))),
                };
                let truncated = result.items.len() > super::repository::MAX_SELECTION_ENTRIES;
                let items = result
                    .items
                    .iter()
                    .take(super::repository::MAX_SELECTION_ENTRIES)
                    .map(|item| super::renderer::SafeCatalogItem {
                        handle: item.harness_profile_handle.clone(),
                        label: item.label.clone(),
                        sensitivity: None,
                        enabled: None,
                        remote_start_enabled: None,
                    })
                    .collect::<Vec<_>>();
                let entries = items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| SelectionEntry {
                        slot: u16::try_from(index + 1).expect("catalog selection is bounded"),
                        device_id: context.device_id,
                        client_opaque_handle: Some(item.handle.clone()),
                        item_kind: SelectionItemKind::HarnessProfile,
                    })
                    .collect::<Vec<_>>();
                self.service
                    .replace_catalog_selection_at(
                        &claim.sender_fingerprint,
                        context.device_id,
                        "harness_profiles",
                        &entries,
                        unix_timestamp_ms()?,
                    )
                    .await?;
                Ok(Some(super::renderer::render_catalog(
                    "profile", &items, truncated,
                )))
            }
            InboundCommandV5::SelectHarnessProfile { slot } => {
                let selected = self
                    .service
                    .select_catalog_slot_at(
                        &claim.sender_fingerprint,
                        slot,
                        SelectionItemKind::HarnessProfile,
                        unix_timestamp_ms()?,
                    )
                    .await?;
                match selected {
                    Ok(_) => Ok(Some(super::renderer::render_catalog_selected(
                        "profile",
                        &format!("配置 {slot}"),
                    ))),
                    Err(error) => Ok(Some(render_selection_error(matches!(
                        error,
                        SelectionLookupError::Expired
                    )))),
                }
            }
            InboundCommandV5::ListTaskPresets => {
                let context = match self
                    .service
                    .selected_catalog_context_at(&claim.sender_fingerprint, unix_timestamp_ms()?)
                    .await?
                {
                    Ok(context) => context,
                    Err(error) => {
                        return Ok(Some(render_selection_error(matches!(
                            error,
                            SelectionLookupError::Expired
                        ))));
                    }
                };
                let (Some(runtime_handle), Some(workspace_handle), Some(profile_handle)) = (
                    context.runtime_handle,
                    context.workspace_handle,
                    context.harness_profile_handle,
                ) else {
                    return Ok(Some(render_selection_error(false)));
                };
                let Some(result) = self
                    .dispatch_catalog(
                        claim,
                        context.device_id,
                        CatalogQueryActionV5::ListTaskPresets {
                            runtime_handle,
                            workspace_handle,
                            harness_profile_handle: profile_handle,
                        },
                        cancellation,
                    )
                    .await?
                else {
                    return Ok(None);
                };
                let result = match result {
                    Ok(CatalogQueryResultV5::ListTaskPresets(page)) => page,
                    Ok(_) => {
                        return Ok(Some(render_gateway_error(
                            crate::gateway::GatewayRequestError::ProtocolMismatch,
                        )));
                    }
                    Err(error) => return Ok(Some(render_gateway_error(error))),
                };
                let truncated = result.items.len() > super::repository::MAX_SELECTION_ENTRIES;
                let items = result
                    .items
                    .iter()
                    .take(super::repository::MAX_SELECTION_ENTRIES)
                    .map(|item| super::renderer::SafeCatalogItem {
                        handle: item.preset_handle.clone(),
                        label: item.label.clone(),
                        sensitivity: None,
                        enabled: None,
                        remote_start_enabled: None,
                    })
                    .collect::<Vec<_>>();
                let entries = items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| SelectionEntry {
                        slot: u16::try_from(index + 1).expect("catalog selection is bounded"),
                        device_id: context.device_id,
                        client_opaque_handle: Some(item.handle.clone()),
                        item_kind: SelectionItemKind::TaskPreset,
                    })
                    .collect::<Vec<_>>();
                self.service
                    .replace_catalog_selection_at(
                        &claim.sender_fingerprint,
                        context.device_id,
                        "task_presets",
                        &entries,
                        unix_timestamp_ms()?,
                    )
                    .await?;
                Ok(Some(super::renderer::render_catalog(
                    "preset", &items, truncated,
                )))
            }
            InboundCommandV5::StartRun { slot } => {
                self.prepare_start_confirmation(claim, slot).await
            }
            InboundCommandV5::Confirm { confirmation_id } => {
                self.execute_confirmation(claim, &confirmation_id, cancellation)
                    .await
            }
            InboundCommandV5::CancelConfirmation => {
                self.service
                    .cancel_confirmations_at(&claim.sender_fingerprint, unix_timestamp_ms()?)
                    .await?;
                Ok(Some(super::renderer::render_cancelled_confirmation()))
            }
            InboundCommandV5::ListRuns { filter } => {
                let device = match self.resolve_query_device(&claim.sender_fingerprint).await? {
                    DeviceResolution::Selected(device) => device,
                    DeviceResolution::Offline => {
                        return Ok(Some(render_gateway_error(
                            crate::gateway::GatewayRequestError::Offline,
                        )));
                    }
                    DeviceResolution::SelectionRequired => {
                        return Ok(Some(render_device_selection_required()));
                    }
                };
                let action = page_action(filter, None);
                let Some(result) = self
                    .dispatch(claim, device.id, action, cancellation)
                    .await?
                else {
                    return Ok(None);
                };
                let page = match result {
                    Ok(RunQueryResultV5::ListRuns(page)) => page,
                    Ok(_) => {
                        return Ok(Some(render_gateway_error(
                            crate::gateway::GatewayRequestError::ProtocolMismatch,
                        )));
                    }
                    Err(error) => return Ok(Some(render_gateway_error(error))),
                };
                let entries = page
                    .items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| SelectionEntry {
                        slot: u16::try_from(index + 1).expect("job page is bounded"),
                        device_id: device.id,
                        client_opaque_handle: Some(item.run_handle.clone()),
                        item_kind: SelectionItemKind::Run,
                    })
                    .collect::<Vec<_>>();
                let mut rendered = render_job_page(&device.name, &page);
                rendered.job_page_selection = Some(PendingJobPageSelection {
                    selected_device: device.id,
                    filter,
                    next_cursor: page.next_cursor.clone(),
                    entries,
                });
                Ok(Some(rendered))
            }
            InboundCommandV5::NextPage => {
                let context = match self
                    .service
                    .next_page_context_at(&claim.sender_fingerprint, unix_timestamp_ms()?)
                    .await?
                {
                    Ok(context) => context,
                    Err(error) => {
                        return Ok(Some(render_selection_error(matches!(
                            error,
                            SelectionLookupError::Expired
                        ))));
                    }
                };
                let device = self
                    .online_query_devices()
                    .await?
                    .into_iter()
                    .find(|device| device.id == context.selected_device);
                let Some(device) = device else {
                    return Ok(Some(render_selection_error(false)));
                };
                let Some(result) = self
                    .dispatch(
                        claim,
                        device.id,
                        page_action(context.filter, Some(context.next_cursor)),
                        cancellation,
                    )
                    .await?
                else {
                    return Ok(None);
                };
                let page = match result {
                    Ok(RunQueryResultV5::ListRuns(page)) => page,
                    Ok(_) => {
                        return Ok(Some(render_gateway_error(
                            crate::gateway::GatewayRequestError::ProtocolMismatch,
                        )));
                    }
                    Err(error) => return Ok(Some(render_gateway_error(error))),
                };
                let entries = page
                    .items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| SelectionEntry {
                        slot: u16::try_from(index + 1).expect("job page is bounded"),
                        device_id: device.id,
                        client_opaque_handle: Some(item.run_handle.clone()),
                        item_kind: SelectionItemKind::Run,
                    })
                    .collect::<Vec<_>>();
                let mut rendered = render_job_page(&device.name, &page);
                rendered.job_page_selection = Some(PendingJobPageSelection {
                    selected_device: device.id,
                    filter: context.filter,
                    next_cursor: page.next_cursor.clone(),
                    entries,
                });
                Ok(Some(rendered))
            }
            InboundCommandV5::GetRunStatus { slot } => {
                match self
                    .service
                    .resolve_job_slot_at(&claim.sender_fingerprint, slot, unix_timestamp_ms()?)
                    .await?
                {
                    Ok(entry) => self.query_detail(claim, entry, true, cancellation).await,
                    Err(SelectionLookupError::WrongKind) => {
                        let selected = self
                            .service
                            .select_device_slot_at(
                                &claim.sender_fingerprint,
                                slot,
                                unix_timestamp_ms()?,
                            )
                            .await?;
                        match selected {
                            Ok(device_id) => {
                                let name = self.device_name(device_id).await?;
                                Ok(Some(render_device_selected(&name)))
                            }
                            Err(error) => Ok(Some(render_selection_error(matches!(
                                error,
                                SelectionLookupError::Expired
                            )))),
                        }
                    }
                    Err(error) => Ok(Some(render_selection_error(matches!(
                        error,
                        SelectionLookupError::Expired
                    )))),
                }
            }
            InboundCommandV5::GetRunDetail { slot } => {
                self.query_selected_detail(claim, slot, false, cancellation)
                    .await
            }
            InboundCommandV5::GetRunTree { slot } => {
                let entry = match self
                    .service
                    .resolve_job_slot_at(&claim.sender_fingerprint, slot, unix_timestamp_ms()?)
                    .await?
                {
                    Ok(entry) => entry,
                    Err(error) => {
                        return Ok(Some(render_selection_error(matches!(
                            error,
                            SelectionLookupError::Expired
                        ))));
                    }
                };
                let Some(run_handle) = entry.client_opaque_handle else {
                    return Ok(Some(render_selection_error(false)));
                };
                let Some(result) = self
                    .dispatch(
                        claim,
                        entry.device_id,
                        RunQueryActionV5::GetRunTree {
                            run_handle,
                            max_depth: 4,
                            max_nodes: 100,
                        },
                        cancellation,
                    )
                    .await?
                else {
                    return Ok(None);
                };
                match result {
                    Ok(RunQueryResultV5::GetRunTree(tree)) => {
                        let name = self.device_name(entry.device_id).await?;
                        Ok(Some(render_job_tree(&name, &tree)))
                    }
                    Ok(_) => Ok(Some(render_gateway_error(
                        crate::gateway::GatewayRequestError::ProtocolMismatch,
                    ))),
                    Err(error) => Ok(Some(render_gateway_error(error))),
                }
            }
            InboundCommandV5::CancelRun { slot } => {
                self.prepare_cancel_confirmation(claim, slot).await
            }
        }
    }

    async fn prepare_start_confirmation(
        &self,
        claim: &ClaimedInboundCommand,
        slot: u16,
    ) -> Result<Option<RenderedInboundReply>, InboundWorkerError> {
        let entry = match self
            .service
            .resolve_catalog_slot_at(
                &claim.sender_fingerprint,
                slot,
                SelectionItemKind::TaskPreset,
                unix_timestamp_ms()?,
            )
            .await?
        {
            Ok(entry) => entry,
            Err(error) => {
                return Ok(Some(render_selection_error(matches!(
                    error,
                    SelectionLookupError::Expired
                ))));
            }
        };
        let Some(preset_handle) = entry.client_opaque_handle else {
            return Ok(Some(render_selection_error(false)));
        };
        let context = match self
            .service
            .selected_catalog_context_at(&claim.sender_fingerprint, unix_timestamp_ms()?)
            .await?
        {
            Ok(context) => context,
            Err(error) => {
                return Ok(Some(render_selection_error(matches!(
                    error,
                    SelectionLookupError::Expired
                ))));
            }
        };
        let (Some(runtime_handle), Some(workspace_handle), Some(profile_handle)) = (
            context.runtime_handle,
            context.workspace_handle,
            context.harness_profile_handle,
        ) else {
            return Ok(Some(render_selection_error(false)));
        };
        if !self
            .control_device_available(context.device_id, true)
            .await?
        {
            return Ok(Some(render_control_error("unauthorized")));
        }
        let device = self.device_name(context.device_id).await?;
        let code = new_confirmation_code();
        let now = unix_timestamp_ms()?;
        let intent_id = Uuid::new_v4().to_string();
        let mut rendered = super::renderer::render_confirmation_prompt(
            "start_run",
            &device,
            Some("已选工作区"),
            Some("已选配置"),
            Some("已选预设"),
            None,
            &code,
        );
        let mut confirmation = PendingConfirmation {
            sender_fingerprint: claim.sender_fingerprint.clone(),
            confirmation_id: format!("confirm_{}", Uuid::new_v4().simple()),
            confirmation_digest: String::new(),
            confirmation_nonce: Uuid::new_v4().simple().to_string(),
            device_id: context.device_id,
            action_kind: ConfirmationActionKind::StartRun,
            runtime_handle: Some(runtime_handle),
            workspace_handle: Some(workspace_handle),
            harness_profile_handle: Some(profile_handle),
            task_preset_handle: Some(preset_handle),
            run_handle: None,
            intent_id,
            expires_at: now.saturating_add(super::repository::CONTROL_CONFIRMATION_TTL_MS),
        };
        confirmation.confirmation_digest = self.verifier.digest(&confirmation, &code);
        rendered.pending_confirmation = Some(confirmation);
        Ok(Some(rendered))
    }

    async fn prepare_cancel_confirmation(
        &self,
        claim: &ClaimedInboundCommand,
        slot: u16,
    ) -> Result<Option<RenderedInboundReply>, InboundWorkerError> {
        let entry = match self
            .service
            .resolve_job_slot_at(&claim.sender_fingerprint, slot, unix_timestamp_ms()?)
            .await?
        {
            Ok(entry) => entry,
            Err(error) => {
                return Ok(Some(render_selection_error(matches!(
                    error,
                    SelectionLookupError::Expired
                ))));
            }
        };
        let Some(run_handle) = entry.client_opaque_handle else {
            return Ok(Some(render_selection_error(false)));
        };
        if !self
            .control_device_available(entry.device_id, false)
            .await?
        {
            return Ok(Some(render_control_error("unauthorized")));
        }
        let device = self.device_name(entry.device_id).await?;
        let code = new_confirmation_code();
        let now = unix_timestamp_ms()?;
        let mut rendered = super::renderer::render_confirmation_prompt(
            "cancel_run",
            &device,
            None,
            None,
            None,
            Some("已选任务"),
            &code,
        );
        let mut confirmation = PendingConfirmation {
            sender_fingerprint: claim.sender_fingerprint.clone(),
            confirmation_id: format!("confirm_{}", Uuid::new_v4().simple()),
            confirmation_digest: String::new(),
            confirmation_nonce: Uuid::new_v4().simple().to_string(),
            device_id: entry.device_id,
            action_kind: ConfirmationActionKind::CancelRun,
            runtime_handle: None,
            workspace_handle: None,
            harness_profile_handle: None,
            task_preset_handle: None,
            run_handle: Some(run_handle),
            intent_id: Uuid::new_v4().to_string(),
            expires_at: now.saturating_add(super::repository::CONTROL_CONFIRMATION_TTL_MS),
        };
        confirmation.confirmation_digest = self.verifier.digest(&confirmation, &code);
        rendered.pending_confirmation = Some(confirmation);
        Ok(Some(rendered))
    }

    async fn execute_confirmation(
        &self,
        claim: &ClaimedInboundCommand,
        confirmation_id: &str,
        cancellation: &CancellationToken,
    ) -> Result<Option<RenderedInboundReply>, InboundWorkerError> {
        let now = unix_timestamp_ms()?;
        let dispatch = match self
            .service
            .prepare_control_dispatch_at(claim, confirmation_id, now)
            .await
        {
            Ok(dispatch) => dispatch,
            Err(error) => return Ok(Some(render_confirmation_error_label(error))),
        };
        let action_kind = dispatch.confirmation.action_kind;
        let device_id = dispatch.confirmation.device_id;
        if let Some(outcome) = dispatch.outcome.as_ref() {
            return self
                .render_control_outcome(action_kind, device_id, outcome)
                .await
                .map(Some);
        }
        if let Some(error_code) = dispatch.error_code.as_deref() {
            return Ok(Some(render_control_error(error_code)));
        }
        let intent_id = dispatch.confirmation.intent_id.clone();
        let result = match action_kind {
            ConfirmationActionKind::StartRun => {
                let (Some(workspace_handle), Some(profile_handle), Some(preset_handle)) = (
                    dispatch.confirmation.workspace_handle.clone(),
                    dispatch.confirmation.profile_handle.clone(),
                    dispatch.confirmation.preset_handle.clone(),
                ) else {
                    let applied = self
                        .service
                        .fail_control_dispatch_at(claim, confirmation_id, "wrong_scope", now)
                        .await?;
                    return Ok(applied.then(|| render_control_error("wrong_scope")));
                };
                self.dispatch_control(
                    claim,
                    device_id,
                    ControlActionV5::StartRun {
                        runtime_handle: dispatch
                            .confirmation
                            .runtime_handle
                            .clone()
                            .ok_or(InboundWorkerError::DeviceDirectory)?,
                        workspace_handle,
                        harness_profile_handle: profile_handle,
                        preset_handle,
                        intent_id,
                    },
                    cancellation,
                )
                .await?
            }
            ConfirmationActionKind::CancelRun => {
                let Some(run_handle) = dispatch.confirmation.run_handle.clone() else {
                    let applied = self
                        .service
                        .fail_control_dispatch_at(claim, confirmation_id, "wrong_scope", now)
                        .await?;
                    return Ok(applied.then(|| render_control_error("wrong_scope")));
                };
                self.dispatch_control(
                    claim,
                    device_id,
                    ControlActionV5::CancelRun {
                        run_handle,
                        intent_id,
                    },
                    cancellation,
                )
                .await?
            }
        };
        let Some(result) = result else {
            return Ok(None);
        };
        match (action_kind, result) {
            (ConfirmationActionKind::StartRun, Ok(ControlResultV5::StartRun(value))) => {
                let outcome = ControlDispatchOutcome {
                    accepted: value.accepted,
                    run_handle: value.run_handle,
                    status: phase_status(value.phase).to_owned(),
                };
                if !self
                    .service
                    .persist_control_outcome_at(claim, confirmation_id, &outcome, now)
                    .await?
                {
                    return Ok(None);
                }
                self.render_control_outcome(action_kind, device_id, &outcome)
                    .await
                    .map(Some)
            }
            (ConfirmationActionKind::CancelRun, Ok(ControlResultV5::CancelRun(value))) => {
                let outcome = ControlDispatchOutcome {
                    accepted: value.accepted,
                    run_handle: value.run_handle,
                    status: cancel_status(value.phase).to_owned(),
                };
                if !self
                    .service
                    .persist_control_outcome_at(claim, confirmation_id, &outcome, now)
                    .await?
                {
                    return Ok(None);
                }
                self.render_control_outcome(action_kind, device_id, &outcome)
                    .await
                    .map(Some)
            }
            (_, Ok(_)) => {
                self.finish_control_error(
                    claim,
                    confirmation_id,
                    crate::gateway::GatewayRequestError::ProtocolMismatch,
                    now,
                )
                .await
            }
            (_, Err(error)) => {
                if is_retryable_control_error(&error) {
                    self.service.retry_control_dispatch_at(claim, now).await?;
                    Ok(None)
                } else {
                    self.finish_control_error(claim, confirmation_id, error, now)
                        .await
                }
            }
        }
    }

    async fn render_control_outcome(
        &self,
        action_kind: ConfirmationActionKind,
        device_id: Uuid,
        outcome: &ControlDispatchOutcome,
    ) -> Result<RenderedInboundReply, InboundWorkerError> {
        let device = self.device_name(device_id).await?;
        Ok(match action_kind {
            ConfirmationActionKind::StartRun if outcome.accepted => {
                super::renderer::render_start_accepted(&device, "已选预设", &outcome.status)
            }
            ConfirmationActionKind::StartRun => {
                super::renderer::render_start_rejected(&device, "已选预设", &outcome.status)
            }
            ConfirmationActionKind::CancelRun if outcome.accepted => {
                super::renderer::render_cancel_accepted(&device, "已选任务", &outcome.status)
            }
            ConfirmationActionKind::CancelRun => {
                super::renderer::render_cancel_rejected(&device, "已选任务", &outcome.status)
            }
        })
    }

    async fn finish_control_error(
        &self,
        claim: &ClaimedInboundCommand,
        confirmation_id: &str,
        error: crate::gateway::GatewayRequestError,
        now: i64,
    ) -> Result<Option<RenderedInboundReply>, InboundWorkerError> {
        let code = gateway_error_code(error);
        if !self
            .service
            .fail_control_dispatch_at(claim, confirmation_id, code, now)
            .await?
        {
            return Ok(None);
        }
        Ok(Some(render_control_error(code)))
    }

    async fn query_selected_detail(
        &self,
        claim: &ClaimedInboundCommand,
        slot: u16,
        compact: bool,
        cancellation: &CancellationToken,
    ) -> Result<Option<RenderedInboundReply>, InboundWorkerError> {
        let entry = match self
            .service
            .resolve_job_slot_at(&claim.sender_fingerprint, slot, unix_timestamp_ms()?)
            .await?
        {
            Ok(entry) => entry,
            Err(error) => {
                return Ok(Some(render_selection_error(matches!(
                    error,
                    SelectionLookupError::Expired
                ))));
            }
        };
        self.query_detail(claim, entry, compact, cancellation).await
    }

    async fn query_detail(
        &self,
        claim: &ClaimedInboundCommand,
        entry: SelectionEntry,
        compact: bool,
        cancellation: &CancellationToken,
    ) -> Result<Option<RenderedInboundReply>, InboundWorkerError> {
        let Some(run_handle) = entry.client_opaque_handle else {
            return Ok(Some(render_selection_error(false)));
        };
        let Some(result) = self
            .dispatch(
                claim,
                entry.device_id,
                RunQueryActionV5::GetRunDetail { run_handle },
                cancellation,
            )
            .await?
        else {
            return Ok(None);
        };
        match result {
            Ok(RunQueryResultV5::GetRunDetail(detail)) => {
                let name = self.device_name(entry.device_id).await?;
                Ok(Some(render_job_detail(&name, &detail, compact)))
            }
            Ok(_) => Ok(Some(render_gateway_error(
                crate::gateway::GatewayRequestError::ProtocolMismatch,
            ))),
            Err(error) => Ok(Some(render_gateway_error(error))),
        }
    }

    async fn dispatch(
        &self,
        claim: &ClaimedInboundCommand,
        device_id: Uuid,
        action: RunQueryActionV5,
        cancellation: &CancellationToken,
    ) -> Result<
        Option<Result<RunQueryResultV5, crate::gateway::GatewayRequestError>>,
        InboundWorkerError,
    > {
        if !self
            .service
            .mark_waiting_gateway_at(claim, unix_timestamp_ms()?)
            .await?
        {
            return Ok(None);
        }
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Ok(None),
            result = self.gateway.request_job(device_id, action) => Ok(Some(result)),
        }
    }

    // All catalog/control calls stay in these two helpers so the inbound
    // worker does not duplicate Gateway authorization or capability logic.
    // Expected v5 signatures:
    //   request_catalog(Uuid, CatalogQueryActionV5)
    //   request_control(Uuid, ControlActionV5)
    async fn dispatch_catalog(
        &self,
        claim: &ClaimedInboundCommand,
        device_id: Uuid,
        action: CatalogQueryActionV5,
        cancellation: &CancellationToken,
    ) -> Result<
        Option<Result<CatalogQueryResultV5, crate::gateway::GatewayRequestError>>,
        InboundWorkerError,
    > {
        if !self
            .service
            .mark_waiting_gateway_at(claim, unix_timestamp_ms()?)
            .await?
        {
            return Ok(None);
        }
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Ok(None),
            result = self.gateway.request_catalog(device_id, action) => Ok(Some(result)),
        }
    }

    async fn dispatch_control(
        &self,
        _claim: &ClaimedInboundCommand,
        device_id: Uuid,
        action: ControlActionV5,
        cancellation: &CancellationToken,
    ) -> Result<
        Option<Result<ControlResultV5, crate::gateway::GatewayRequestError>>,
        InboundWorkerError,
    > {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Ok(None),
            result = self.gateway.request_control(device_id, action) => Ok(Some(result)),
        }
    }

    async fn resolve_query_device(
        &self,
        sender_fingerprint: &str,
    ) -> Result<DeviceResolution, InboundWorkerError> {
        let devices = self.online_query_devices().await?;
        if devices.is_empty() {
            return Ok(DeviceResolution::Offline);
        }
        if devices.len() == 1 {
            let selected = devices[0].clone();
            self.service
                .replace_selection_at(
                    sender_fingerprint,
                    Some(selected.id),
                    &[],
                    unix_timestamp_ms()?,
                )
                .await?;
            return Ok(DeviceResolution::Selected(selected));
        }
        let selected = self
            .service
            .selected_device_at(sender_fingerprint, unix_timestamp_ms()?)
            .await?
            .ok();
        Ok(selected
            .and_then(|selected| devices.into_iter().find(|device| device.id == selected))
            .map_or(
                DeviceResolution::SelectionRequired,
                DeviceResolution::Selected,
            ))
    }

    async fn resolve_catalog_device(
        &self,
        sender_fingerprint: &str,
    ) -> Result<DeviceResolution, InboundWorkerError> {
        let devices = self.online_catalog_devices().await?;
        if devices.is_empty() {
            return Ok(DeviceResolution::Offline);
        }
        if devices.len() == 1 {
            let selected = devices[0].clone();
            self.service
                .replace_selection_at(
                    sender_fingerprint,
                    Some(selected.id),
                    &[],
                    unix_timestamp_ms()?,
                )
                .await?;
            return Ok(DeviceResolution::Selected(selected));
        }
        let selected = self
            .service
            .selected_device_at(sender_fingerprint, unix_timestamp_ms()?)
            .await?
            .ok();
        Ok(selected
            .and_then(|selected| devices.into_iter().find(|device| device.id == selected))
            .map_or(
                DeviceResolution::SelectionRequired,
                DeviceResolution::Selected,
            ))
    }

    async fn online_query_devices(&self) -> Result<Vec<ConsoleDevice>, InboundWorkerError> {
        let summaries = self
            .devices
            .list_devices()
            .await
            .map_err(|_| InboundWorkerError::DeviceDirectory)?;
        let mut devices = Vec::new();
        for summary in summaries {
            if devices.len() >= 50
                || !summary.enabled
                || summary.revoked_at.is_some()
                || !summary.scopes.contains(&DeviceScope::GatewayConnect)
                || !summary.scopes.contains(&DeviceScope::RunQuery)
            {
                continue;
            }
            let Ok(device_id) = Uuid::parse_str(&summary.id) else {
                return Err(InboundWorkerError::DeviceDirectory);
            };
            let Some(connection) = self.gateway.registry().connection(device_id).await else {
                continue;
            };
            if !GatewayCapabilityV5::has_all(
                &connection.capabilities,
                GatewayCapabilityV5::RUN_QUERY_REQUIREMENTS,
            ) {
                continue;
            }
            devices.push(ConsoleDevice {
                id: device_id,
                name: sanitize_device_display_name(&summary.name),
            });
        }
        Ok(devices)
    }

    async fn online_catalog_devices(&self) -> Result<Vec<ConsoleDevice>, InboundWorkerError> {
        let summaries = self
            .devices
            .list_devices()
            .await
            .map_err(|_| InboundWorkerError::DeviceDirectory)?;
        let mut devices = Vec::new();
        for summary in summaries {
            if devices.len() >= 50
                || !summary.enabled
                || summary.revoked_at.is_some()
                || !summary.scopes.contains(&DeviceScope::GatewayConnect)
                || !summary.scopes.contains(&DeviceScope::RunQuery)
            {
                continue;
            }
            let Ok(device_id) = Uuid::parse_str(&summary.id) else {
                return Err(InboundWorkerError::DeviceDirectory);
            };
            let Some(connection) = self.gateway.registry().connection(device_id).await else {
                continue;
            };
            if !GatewayCapabilityV5::has_all(
                &connection.capabilities,
                GatewayCapabilityV5::CATALOG_REQUIREMENTS,
            ) {
                continue;
            }
            devices.push(ConsoleDevice {
                id: device_id,
                name: sanitize_device_display_name(&summary.name),
            });
        }
        Ok(devices)
    }

    async fn device_name(&self, device_id: Uuid) -> Result<String, InboundWorkerError> {
        self.devices
            .list_devices()
            .await
            .map_err(|_| InboundWorkerError::DeviceDirectory)?
            .into_iter()
            .find(|device| device.id == device_id.to_string())
            .map(|device| sanitize_device_display_name(&device.name))
            .ok_or(InboundWorkerError::DeviceDirectory)
    }

    async fn control_device_available(
        &self,
        device_id: Uuid,
        requires_launch: bool,
    ) -> Result<bool, InboundWorkerError> {
        let summary = self
            .devices
            .list_devices()
            .await
            .map_err(|_| InboundWorkerError::DeviceDirectory)?
            .into_iter()
            .find(|device| device.id == device_id.to_string());
        let Some(summary) = summary else {
            return Ok(false);
        };
        if !summary.enabled
            || summary.revoked_at.is_some()
            || !summary.scopes.contains(&DeviceScope::GatewayConnect)
            || !summary.scopes.contains(&DeviceScope::RunControl)
        {
            return Ok(false);
        }
        let Some(connection) = self.gateway.registry().connection(device_id).await else {
            return Ok(false);
        };
        let requirements = if requires_launch {
            GatewayCapabilityV5::START_RUN_REQUIREMENTS
        } else {
            GatewayCapabilityV5::CANCEL_RUN_REQUIREMENTS
        };
        Ok(GatewayCapabilityV5::has_all(
            &connection.capabilities,
            requirements,
        ))
    }

    async fn queue_reply(
        &self,
        claim: &ClaimedInboundCommand,
        rendered: RenderedInboundReply,
        cancellation: &CancellationToken,
    ) -> Result<WorkerPass, InboundWorkerError> {
        if cancellation.is_cancelled() {
            let _ = self
                .service
                .interrupt_claim_at(claim, unix_timestamp_ms()?)
                .await?;
            return Ok(WorkerPass::Interrupted);
        }
        let queued_at = unix_timestamp_ms()?;
        let reply_expires_at = rendered
            .pending_confirmation
            .as_ref()
            .map_or(claim.expires_at, |value| {
                value.expires_at.min(claim.expires_at)
            });
        if queued_at >= claim.expires_at || queued_at >= reply_expires_at {
            let applied = self.service.release_claim_at(claim, queued_at).await?;
            return Ok(if applied {
                WorkerPass::Applied
            } else {
                WorkerPass::Interrupted
            });
        }
        let identity = reply_identity(&claim.message_key);
        let selection = rendered.job_page_selection;
        let confirmation = rendered.pending_confirmation;
        let sensitive_body = confirmation.is_some();
        let reply = InteractiveReplyV1 {
            schema_version: 1,
            notification_id: format!("interactive:{identity}"),
            dedupe_key: format!("inbound:{identity}"),
            priority: REPLY_PRIORITY,
            target_account_fingerprint: claim.sender_fingerprint.clone(),
            title: rendered.title,
            body: rendered.body,
            sensitive_body,
            correlation_key: Some(format!("inbound:{}", &identity[..32])),
            created_at: queued_at,
            expires_at: reply_expires_at,
        };
        let queued = if let Some(confirmation) = confirmation {
            self.service
                .queue_reply_with_confirmation_at(
                    &self.outbox,
                    claim,
                    reply,
                    confirmation,
                    queued_at,
                )
                .await?
        } else if let Some(selection) = selection {
            self.service
                .queue_reply_with_job_page_at(&self.outbox, claim, reply, selection, queued_at)
                .await?
        } else {
            self.service
                .queue_reply_at(&self.outbox, claim, reply, queued_at)
                .await?
        };
        if !queued {
            let _ = self
                .service
                .release_claim_at(claim, unix_timestamp_ms()?)
                .await?;
            return Ok(WorkerPass::Interrupted);
        }
        Ok(WorkerPass::Applied)
    }
}

fn new_confirmation_code() -> String {
    format!("{:06}", rand::rng().random_range(0..1_000_000_u32))
}

fn render_confirmation_error_label(
    error: super::repository::ConfirmationConsumeError,
) -> RenderedInboundReply {
    super::renderer::render_confirmation_error(match error {
        super::repository::ConfirmationConsumeError::Expired => "expired",
        super::repository::ConfirmationConsumeError::Replay => "replay",
        super::repository::ConfirmationConsumeError::WrongScope => "wrong_scope",
        super::repository::ConfirmationConsumeError::Missing => "missing",
    })
}

fn is_retryable_control_error(error: &crate::gateway::GatewayRequestError) -> bool {
    matches!(
        error,
        crate::gateway::GatewayRequestError::Offline
            | crate::gateway::GatewayRequestError::Disconnected
            | crate::gateway::GatewayRequestError::Expired
            | crate::gateway::GatewayRequestError::PendingLimit
            | crate::gateway::GatewayRequestError::SlowConsumer
    )
}

fn gateway_error_code(error: crate::gateway::GatewayRequestError) -> &'static str {
    match error {
        crate::gateway::GatewayRequestError::Offline => "offline",
        crate::gateway::GatewayRequestError::Disconnected => "disconnected",
        crate::gateway::GatewayRequestError::Expired => "expired",
        crate::gateway::GatewayRequestError::PendingLimit => "pending_limit",
        crate::gateway::GatewayRequestError::SlowConsumer => "slow_consumer",
        crate::gateway::GatewayRequestError::Unauthorized => "unauthorized",
        crate::gateway::GatewayRequestError::AuthorizationUnavailable => {
            "authorization_unavailable"
        }
        crate::gateway::GatewayRequestError::UnsupportedCapability => "unsupported_capability",
        crate::gateway::GatewayRequestError::Rejected => "rejected",
        crate::gateway::GatewayRequestError::InvalidRequest => "invalid_request",
        crate::gateway::GatewayRequestError::ProtocolMismatch => "protocol_mismatch",
    }
}

fn phase_status(status: RemoteRunPhaseV5) -> &'static str {
    match status {
        RemoteRunPhaseV5::Pending => "queued",
        RemoteRunPhaseV5::Provisioning => "provisioning",
        RemoteRunPhaseV5::Starting => "starting",
        RemoteRunPhaseV5::Active => "running",
        RemoteRunPhaseV5::WaitingInput => "waiting_input",
        RemoteRunPhaseV5::Stopping | RemoteRunPhaseV5::Finalizing => "cancelling",
        RemoteRunPhaseV5::Finished => "terminal",
    }
}

fn cancel_status(phase: RemoteRunPhaseV5) -> &'static str {
    match phase {
        RemoteRunPhaseV5::Stopping => "requested",
        RemoteRunPhaseV5::Finalizing => "closing_session",
        RemoteRunPhaseV5::Finished => "terminal",
        RemoteRunPhaseV5::Pending
        | RemoteRunPhaseV5::Provisioning
        | RemoteRunPhaseV5::Starting
        | RemoteRunPhaseV5::Active
        | RemoteRunPhaseV5::WaitingInput => "none",
    }
}

fn page_action(filter: RunFilter, cursor: Option<String>) -> RunQueryActionV5 {
    RunQueryActionV5::ListRuns {
        filter: match filter {
            RunFilter::All => RemoteRunFilterV5::All,
            RunFilter::Recent => RemoteRunFilterV5::Recent,
            RunFilter::Failed => RemoteRunFilterV5::Failed,
        },
        page_size: JOB_PAGE_SIZE,
        cursor,
    }
}

fn reply_identity(message_key: &str) -> String {
    let mut hasher = blake3::Hasher::new_derive_key("promptdock-relay/inbound-reply/v1");
    hasher.update(&(message_key.len() as u64).to_be_bytes());
    hasher.update(message_key.as_bytes());
    hasher.finalize().to_hex().to_string()
}
