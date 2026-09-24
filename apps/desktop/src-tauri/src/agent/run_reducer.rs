use crate::agent::{AgentEventEnvelopeV2, AgentEventPayloadV2, CompletionConfidence, RunStartedV2};
use crate::storage::AgentRunRecord;

#[cfg(test)]
pub(crate) const CODEX_SETTLE_QUIET_MS: i64 = crate::model::DEFAULT_COMPLETION_QUIET_MS;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunTransition {
    pub run: AgentRunRecord,
    pub terminal_applied: bool,
    pub reopened_inferred_completion: bool,
    pub replaced_inferred_completion: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum RunStateError {
    #[error("run correlation is missing")]
    MissingRunCorrelation,
    #[error("run identity belongs to a different agent instance")]
    IdentityConflict,
    #[error("run correlation field {0} conflicts with the stored identity")]
    CorrelationConflict(&'static str),
    #[error("event kind does not project a run")]
    UnsupportedEvent,
}

#[cfg(test)]
pub(crate) fn reduce_run(
    current: Option<AgentRunRecord>,
    event: &AgentEventEnvelopeV2,
) -> Result<RunTransition, RunStateError> {
    reduce_run_with_quiet(current, event, CODEX_SETTLE_QUIET_MS)
}

pub(crate) fn reduce_run_with_quiet(
    current: Option<AgentRunRecord>,
    event: &AgentEventEnvelopeV2,
    completion_quiet_ms: i64,
) -> Result<RunTransition, RunStateError> {
    let mut run = merge_run_identity(current, event)?;
    let mut transition = RunTransition {
        run: run.clone(),
        terminal_applied: false,
        reopened_inferred_completion: false,
        replaced_inferred_completion: false,
    };

    match &event.payload {
        AgentEventPayloadV2::RunStarted(started) => {
            transition.reopened_inferred_completion = can_reopen_inferred(&run);
            apply_run_started(&mut run, event, started);
        }
        AgentEventPayloadV2::RunSettling(_) => {
            apply_run_settling(&mut run, event, completion_quiet_ms)
        }
        AgentEventPayloadV2::RunCompleted(terminal) => {
            transition.replaced_inferred_completion = can_reopen_inferred(&run);
            transition.terminal_applied = apply_terminal(
                &mut run,
                event,
                "completed",
                "success",
                terminal.completion_confidence,
            );
        }
        AgentEventPayloadV2::RunFailed(terminal) => {
            transition.replaced_inferred_completion = can_reopen_inferred(&run);
            transition.terminal_applied = apply_terminal(
                &mut run,
                event,
                "failed",
                "failure",
                terminal.completion_confidence,
            );
        }
        AgentEventPayloadV2::RunInterrupted(terminal) => {
            transition.replaced_inferred_completion = can_reopen_inferred(&run);
            transition.terminal_applied = apply_terminal(
                &mut run,
                event,
                "interrupted",
                "interrupted",
                terminal.completion_confidence,
            );
        }
        AgentEventPayloadV2::RunCancelled(terminal) => {
            transition.replaced_inferred_completion = can_reopen_inferred(&run);
            transition.terminal_applied = apply_terminal(
                &mut run,
                event,
                "cancelled",
                "cancelled",
                terminal.completion_confidence,
            );
        }
        AgentEventPayloadV2::OutputProduced(_) => {
            transition.reopened_inferred_completion = can_reopen_inferred(&run);
            apply_activity(&mut run, event);
        }
        AgentEventPayloadV2::PromptSubmitted(_) | AgentEventPayloadV2::AttentionRequired(_) => {
            return Err(RunStateError::UnsupportedEvent);
        }
    }

    transition.run = run;
    transition.replaced_inferred_completion &= transition.terminal_applied;
    Ok(transition)
}

pub(crate) fn finalize_due_run(mut run: AgentRunRecord, now: i64) -> Option<AgentRunRecord> {
    if run.status != "settling" || run.settle_not_before.is_none_or(|deadline| deadline > now) {
        return None;
    }
    run.status = "completed".into();
    // A quiet Stop window proves only that the observed turn ended. It does
    // not prove that the user's goal or any validation succeeded.
    run.outcome = "unknown".into();
    run.completion_confidence = CompletionConfidence::Inferred.as_str().into();
    run.completed_at = Some(
        run.settling_at
            .unwrap_or(run.last_event_at)
            .max(run.last_event_at),
    );
    run.failed_at = None;
    run.interrupted_at = None;
    run.cancelled_at = None;
    run.settle_not_before = None;
    Some(run)
}

fn apply_run_started(
    run: &mut AgentRunRecord,
    event: &AgentEventEnvelopeV2,
    started: &RunStartedV2,
) {
    if !is_absorbing_terminal(run) {
        reopen_running(run);
        run.agent_label = started.agent_label.clone();
        run.started_at = Some(
            run.started_at
                .map_or(event.occurred_at, |value| value.min(event.occurred_at)),
        );
        run.last_event_at = run.last_event_at.max(event.occurred_at);
        return;
    }

    if run.agent_label == run.agent_kind {
        run.agent_label = started.agent_label.clone();
    }
    if terminal_at(run).is_some_and(|terminal| event.occurred_at <= terminal) {
        run.started_at = Some(
            run.started_at
                .map_or(event.occurred_at, |value| value.min(event.occurred_at)),
        );
    }
}

fn apply_run_settling(
    run: &mut AgentRunRecord,
    event: &AgentEventEnvelopeV2,
    completion_quiet_ms: i64,
) {
    if is_absorbing_terminal(run) {
        return;
    }
    if run.status != "settling" {
        run.settle_generation = run.settle_generation.saturating_add(1);
        run.settling_at = Some(event.occurred_at);
    } else {
        run.settling_at = Some(
            run.settling_at
                .map_or(event.occurred_at, |value| value.min(event.occurred_at)),
        );
    }
    run.status = "settling".into();
    run.outcome = "unknown".into();
    run.completion_confidence = CompletionConfidence::Provisional.as_str().into();
    run.completed_at = None;
    run.failed_at = None;
    run.interrupted_at = None;
    run.cancelled_at = None;
    let deadline = event.observed_at.saturating_add(completion_quiet_ms);
    run.settle_not_before = Some(
        run.settle_not_before
            .map_or(deadline, |current| current.max(deadline)),
    );
    run.last_event_at = run.last_event_at.max(event.occurred_at);
}

fn apply_activity(run: &mut AgentRunRecord, event: &AgentEventEnvelopeV2) {
    if is_absorbing_terminal(run) {
        return;
    }
    reopen_running(run);
    run.last_event_at = run.last_event_at.max(event.occurred_at);
}

fn apply_terminal(
    run: &mut AgentRunRecord,
    event: &AgentEventEnvelopeV2,
    status: &'static str,
    outcome: &'static str,
    confidence: CompletionConfidence,
) -> bool {
    if status == "completed" && confidence != CompletionConfidence::Authoritative {
        return false;
    }
    let applied = !is_terminal(&run.status)
        || confidence_rank(confidence) > stored_confidence_rank(&run.completion_confidence);
    if !applied {
        return false;
    }

    run.status = status.into();
    run.outcome = outcome.into();
    run.completion_confidence = confidence.as_str().into();
    run.settling_at = None;
    run.settle_not_before = None;
    run.completed_at = None;
    run.failed_at = None;
    run.interrupted_at = None;
    run.cancelled_at = None;
    match status {
        "completed" => run.completed_at = Some(event.occurred_at),
        "failed" => run.failed_at = Some(event.occurred_at),
        "interrupted" => run.interrupted_at = Some(event.occurred_at),
        "cancelled" => run.cancelled_at = Some(event.occurred_at),
        _ => {}
    }
    run.last_event_at = run.last_event_at.max(event.occurred_at);
    true
}

fn merge_run_identity(
    current: Option<AgentRunRecord>,
    event: &AgentEventEnvelopeV2,
) -> Result<AgentRunRecord, RunStateError> {
    let run_key = event
        .correlation
        .run_key
        .as_deref()
        .ok_or(RunStateError::MissingRunCorrelation)?;
    let agent_kind = event.source.agent_kind.as_str();
    if let Some(mut run) = current {
        if run.agent_kind != agent_kind || run.instance_id != event.source.instance_id {
            return Err(RunStateError::IdentityConflict);
        }
        run.conversation_key = merge_correlation(
            "conversation_key",
            run.conversation_key,
            event.correlation.conversation_key.clone(),
        )?;
        run.parent_run_key = merge_correlation(
            "parent_run_key",
            run.parent_run_key,
            event.correlation.parent_run_key.clone(),
        )?;
        run.agent_id = merge_correlation(
            "agent_id",
            run.agent_id,
            non_blank(event.correlation.agent_id.clone()),
        )?;
        run.agent_role = merge_correlation(
            "agent_role",
            run.agent_role,
            non_blank(event.correlation.agent_role.clone()),
        )?;
        return Ok(run);
    }

    Ok(AgentRunRecord {
        run_key: run_key.into(),
        agent_kind: agent_kind.into(),
        instance_id: event.source.instance_id.clone(),
        conversation_key: event.correlation.conversation_key.clone(),
        parent_run_key: event.correlation.parent_run_key.clone(),
        agent_id: non_blank(event.correlation.agent_id.clone()),
        agent_role: non_blank(event.correlation.agent_role.clone()),
        agent_label: agent_kind.into(),
        status: "running".into(),
        outcome: "unknown".into(),
        completion_confidence: CompletionConfidence::Provisional.as_str().into(),
        started_at: None,
        settling_at: None,
        settle_not_before: None,
        settle_generation: 0,
        completed_at: None,
        failed_at: None,
        interrupted_at: None,
        cancelled_at: None,
        last_event_at: event.occurred_at,
    })
}

fn non_blank(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn merge_correlation(
    field: &'static str,
    stored: Option<String>,
    incoming: Option<String>,
) -> Result<Option<String>, RunStateError> {
    match (stored, incoming) {
        (Some(stored), Some(incoming)) if stored != incoming => {
            Err(RunStateError::CorrelationConflict(field))
        }
        (Some(stored), _) => Ok(Some(stored)),
        (None, incoming) => Ok(incoming),
    }
}

fn reopen_running(run: &mut AgentRunRecord) {
    run.status = "running".into();
    run.outcome = "unknown".into();
    run.completion_confidence = CompletionConfidence::Provisional.as_str().into();
    run.settling_at = None;
    run.settle_not_before = None;
    run.completed_at = None;
    run.failed_at = None;
    run.interrupted_at = None;
    run.cancelled_at = None;
}

fn can_reopen_inferred(run: &AgentRunRecord) -> bool {
    run.status == "completed" && run.completion_confidence == "inferred"
}

fn is_absorbing_terminal(run: &AgentRunRecord) -> bool {
    is_terminal(&run.status) && !can_reopen_inferred(run)
}

fn is_terminal(status: &str) -> bool {
    matches!(
        status,
        "completed" | "failed" | "interrupted" | "cancelled" | "abandoned"
    )
}

fn confidence_rank(value: CompletionConfidence) -> u8 {
    match value {
        CompletionConfidence::Authoritative => 4,
        CompletionConfidence::DefinitiveAdapter => 3,
        CompletionConfidence::Inferred => 2,
        CompletionConfidence::Provisional => 1,
    }
}

fn stored_confidence_rank(value: &str) -> u8 {
    match value {
        "authoritative" => 4,
        "definitive_adapter" => 3,
        "inferred" => 2,
        _ => 1,
    }
}

pub(crate) fn terminal_at(run: &AgentRunRecord) -> Option<i64> {
    match run.status.as_str() {
        "completed" => run.completed_at,
        "failed" => run.failed_at,
        "interrupted" => run.interrupted_at,
        "cancelled" => run.cancelled_at,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{
        AgentCorrelationV2, AgentEventPayloadV2, OutputKind, OutputProducedV2, RunCompletedV2,
        RunFailedV2, RunSettlingV2, SettlingReason, AGENT_EVENT_SCHEMA_VERSION,
    };
    use crate::source::{AgentKind, SourceDescriptor};

    fn envelope(
        event_id: &str,
        occurred_at: i64,
        observed_at: i64,
        payload: AgentEventPayloadV2,
    ) -> AgentEventEnvelopeV2 {
        AgentEventEnvelopeV2 {
            metadata: None,
            schema_version: AGENT_EVENT_SCHEMA_VERSION,
            source: SourceDescriptor::new(
                AgentKind::new("fixture-agent").unwrap(),
                "events",
                "jsonl",
                "default",
            )
            .unwrap(),
            event_id: event_id.into(),
            occurred_at,
            observed_at,
            correlation: AgentCorrelationV2 {
                conversation_key: Some("conversation-1".into()),
                run_key: Some("run-1".into()),
                ..AgentCorrelationV2::default()
            },
            payload,
        }
    }

    fn started(at: i64) -> AgentEventEnvelopeV2 {
        envelope(
            "started",
            at,
            at,
            AgentEventPayloadV2::RunStarted(RunStartedV2 {
                agent_label: "Fixture Agent".into(),
                workspace_path: None,
                model_name: Some("fixture-model".into()),
                task: None,
                reasoning_effort: None,
            }),
        )
    }

    fn settling(event_id: &str, occurred_at: i64, observed_at: i64) -> AgentEventEnvelopeV2 {
        envelope(
            event_id,
            occurred_at,
            observed_at,
            AgentEventPayloadV2::RunSettling(RunSettlingV2 {
                reason: SettlingReason::AgentStopHook,
                codex_usage: None,
            }),
        )
    }

    fn output(at: i64) -> AgentEventEnvelopeV2 {
        envelope(
            "output",
            at,
            at,
            AgentEventPayloadV2::OutputProduced(OutputProducedV2 {
                output_kind: OutputKind::AssistantCandidate,
                text: "private output".into(),
                content_mode: crate::model::ResultContentMode::RedactedExcerpt,
                content_available: true,
                source_hash: Some(crate::agent::source_hash_sha256("private output")),
                capture_generation: Some(0),
            }),
        )
    }

    #[test]
    fn stop_uses_observed_time_and_repeated_stop_only_extends_deadline() {
        let running = reduce_run(None, &started(10)).unwrap().run;
        let first = reduce_run(Some(running), &settling("stop-1", 20, 25))
            .unwrap()
            .run;
        assert_eq!(first.status, "settling");
        assert_eq!(first.settling_at, Some(20));
        assert_eq!(first.settle_not_before, Some(25 + CODEX_SETTLE_QUIET_MS));
        assert_eq!(first.settle_generation, 1);

        let extended = reduce_run(Some(first), &settling("stop-2", 21, 40))
            .unwrap()
            .run;
        assert_eq!(extended.settle_not_before, Some(40 + CODEX_SETTLE_QUIET_MS));
        assert_eq!(extended.settle_generation, 1);
        let out_of_order = reduce_run(Some(extended), &settling("stop-3", 19, 30))
            .unwrap()
            .run;
        assert_eq!(
            out_of_order.settle_not_before,
            Some(40 + CODEX_SETTLE_QUIET_MS)
        );
    }

    #[test]
    fn configured_quiet_window_controls_stop_deadline_and_resume_semantics() {
        let running = reduce_run(None, &started(10)).unwrap().run;
        let settling_run = reduce_run_with_quiet(
            Some(running),
            &settling("stop", 20, 25),
            crate::model::COMPLETION_QUIET_MS_MIN,
        )
        .unwrap()
        .run;
        assert_eq!(settling_run.settle_not_before, Some(25 + 500));
        assert!(finalize_due_run(settling_run.clone(), 524).is_none());
        let inferred = finalize_due_run(settling_run, 525).unwrap();
        let resumed = reduce_run_with_quiet(
            Some(inferred),
            &output(526),
            crate::model::COMPLETION_QUIET_MS_MIN,
        )
        .unwrap();
        assert!(resumed.reopened_inferred_completion);
        assert_eq!(resumed.run.status, "running");
        assert_eq!(resumed.run.settle_not_before, None);

        let running = reduce_run(None, &started(10)).unwrap().run;
        let settling_run = reduce_run_with_quiet(
            Some(running),
            &settling("stop-long", 20, 25),
            crate::model::COMPLETION_QUIET_MS_MAX,
        )
        .unwrap()
        .run;
        assert_eq!(settling_run.settle_not_before, Some(25 + 20_000));
        assert!(finalize_due_run(settling_run, 20_024).is_none());
    }

    #[test]
    fn output_and_started_reopen_settling_without_reusing_generation() {
        let running = reduce_run(None, &started(10)).unwrap().run;
        let settling_run = reduce_run(Some(running), &settling("stop", 20, 20))
            .unwrap()
            .run;
        let resumed = reduce_run(Some(settling_run), &output(21)).unwrap().run;
        assert_eq!(resumed.status, "running");
        assert_eq!(resumed.settle_not_before, None);
        assert_eq!(resumed.settle_generation, 1);

        let next = reduce_run(Some(resumed), &settling("next-stop", 30, 30))
            .unwrap()
            .run;
        assert_eq!(next.settle_generation, 2);
        let resumed_by_start = reduce_run(Some(next), &started(31)).unwrap().run;
        assert_eq!(resumed_by_start.status, "running");
    }

    #[test]
    fn inferred_completion_reopens_but_authoritative_completion_absorbs_activity() {
        let running = reduce_run(None, &started(10)).unwrap().run;
        let settling_run = reduce_run(Some(running), &settling("stop", 20, 20))
            .unwrap()
            .run;
        let inferred = finalize_due_run(settling_run, 20 + CODEX_SETTLE_QUIET_MS).unwrap();
        let reopened = reduce_run(Some(inferred), &output(50)).unwrap();
        assert!(reopened.reopened_inferred_completion);
        assert_eq!(reopened.run.status, "running");

        let authoritative = reduce_run(
            Some(reopened.run),
            &envelope(
                "completed",
                60,
                60,
                AgentEventPayloadV2::RunCompleted(RunCompletedV2 {
                    completion_confidence: CompletionConfidence::Authoritative,
                }),
            ),
        )
        .unwrap()
        .run;
        let absorbed = reduce_run(Some(authoritative), &output(70)).unwrap();
        assert!(!absorbed.reopened_inferred_completion);
        assert_eq!(absorbed.run.status, "completed");
        assert_eq!(absorbed.run.last_event_at, 60);
    }

    #[test]
    fn authoritative_terminal_replaces_inferred_but_equal_conflicts_are_first_wins() {
        let running = reduce_run(None, &started(10)).unwrap().run;
        let settling_run = reduce_run(Some(running), &settling("stop", 20, 20))
            .unwrap()
            .run;
        let inferred = finalize_due_run(settling_run, 20 + CODEX_SETTLE_QUIET_MS).unwrap();
        let failed = reduce_run(
            Some(inferred),
            &envelope(
                "failed",
                30,
                30,
                AgentEventPayloadV2::RunFailed(RunFailedV2 {
                    completion_confidence: CompletionConfidence::Authoritative,
                }),
            ),
        )
        .unwrap();
        assert!(failed.terminal_applied);
        assert!(failed.replaced_inferred_completion);
        assert_eq!(failed.run.status, "failed");

        let conflicting_completion = reduce_run(
            Some(failed.run),
            &envelope(
                "completed",
                31,
                31,
                AgentEventPayloadV2::RunCompleted(RunCompletedV2 {
                    completion_confidence: CompletionConfidence::Authoritative,
                }),
            ),
        )
        .unwrap();
        assert!(!conflicting_completion.terminal_applied);
        assert_eq!(conflicting_completion.run.status, "failed");
    }

    #[test]
    fn finalizer_requires_the_exact_deadline_and_uses_last_activity_time() {
        let running = reduce_run(None, &started(10)).unwrap().run;
        let settling_run = reduce_run(Some(running), &settling("stop", 20, 25))
            .unwrap()
            .run;
        let deadline = 25 + CODEX_SETTLE_QUIET_MS;
        assert!(finalize_due_run(settling_run.clone(), deadline - 1).is_none());
        let completed = finalize_due_run(settling_run, deadline).unwrap();
        assert_eq!(completed.status, "completed");
        assert_eq!(completed.outcome, "unknown");
        assert_eq!(completed.completion_confidence, "inferred");
        assert_eq!(completed.completed_at, Some(20));
    }

    #[test]
    fn identity_and_correlation_conflicts_fail_without_a_projection() {
        let run = reduce_run(None, &started(10)).unwrap().run;
        let mut conflicting = output(20);
        conflicting.correlation.conversation_key = Some("other".into());
        assert_eq!(
            reduce_run(Some(run.clone()), &conflicting).unwrap_err(),
            RunStateError::CorrelationConflict("conversation_key")
        );

        conflicting.correlation.conversation_key = run.conversation_key.clone();
        conflicting.source.instance_id = "other".into();
        assert_eq!(
            reduce_run(Some(run), &conflicting).unwrap_err(),
            RunStateError::IdentityConflict
        );
    }

    #[test]
    fn agent_identity_is_first_value_wins_replay_safe_and_conflict_checked() {
        let mut first = started(10);
        first.correlation.agent_id = Some("agent-private-1".into());
        first.correlation.agent_role = Some("reviewer".into());
        let run = reduce_run(None, &first).unwrap().run;
        assert_eq!(run.agent_id.as_deref(), Some("agent-private-1"));
        assert_eq!(run.agent_role.as_deref(), Some("reviewer"));

        let mut replay = output(20);
        replay.correlation.agent_id = Some("agent-private-1".into());
        replay.correlation.agent_role = Some("reviewer".into());
        let replayed = reduce_run(Some(run.clone()), &replay).unwrap().run;
        assert_eq!(replayed.agent_id, run.agent_id);
        assert_eq!(replayed.agent_role, run.agent_role);

        let mut conflicting_id = replay.clone();
        conflicting_id.correlation.agent_id = Some("agent-private-2".into());
        assert_eq!(
            reduce_run(Some(run.clone()), &conflicting_id).unwrap_err(),
            RunStateError::CorrelationConflict("agent_id")
        );

        let mut conflicting_role = replay;
        conflicting_role.correlation.agent_role = Some("implementer".into());
        assert_eq!(
            reduce_run(Some(run), &conflicting_role).unwrap_err(),
            RunStateError::CorrelationConflict("agent_role")
        );
    }

    #[test]
    fn blank_identity_values_are_ignored_and_cannot_overwrite_projection() {
        let run = reduce_run(None, &started(10)).unwrap().run;
        let mut blank = output(20);
        blank.correlation.agent_id = Some("   ".into());
        blank.correlation.agent_role = Some(String::new());
        let unchanged = reduce_run(Some(run), &blank).unwrap().run;
        assert_eq!(unchanged.agent_id, None);
        assert_eq!(unchanged.agent_role, None);
    }
}
