//! Bounded local lifecycle maintenance.
//!
//! This module never removes an active delivery, an unknown capture, or a
//! pending result action.  Callers run it off the UI thread and may stop after
//! any batch; every operation is independently transactional.

use rusqlite::{params, Transaction};
use serde_json::Value;

use crate::db::Db;
use crate::error::AppError;

pub(crate) const BODY_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1_000;
pub(crate) const HISTORY_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
pub(crate) const RETENTION_BATCH_SIZE: i64 = 200;
pub(crate) const MAX_RETAINED_RECORDS: i64 = 50_000;
pub(crate) const MAX_RETAINED_CONTENT_BYTES: i64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Default, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RetentionSummary {
    pub event_count: i64,
    pub output_count: i64,
    pub outbox_count: i64,
    pub content_bytes: i64,
    pub max_records: i64,
    pub max_content_bytes: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RetentionPruneResult {
    pub scrubbed_outputs: usize,
    pub scrubbed_outbox_payloads: usize,
    pub scrubbed_event_metadata: usize,
    pub deleted_outputs: usize,
    pub deleted_events: usize,
    pub deleted_runs: usize,
}

impl RetentionPruneResult {
    pub(crate) fn changed(self) -> bool {
        self.scrubbed_outputs
            + self.scrubbed_outbox_payloads
            + self.scrubbed_event_metadata
            + self.deleted_outputs
            + self.deleted_events
            + self.deleted_runs
            > 0
    }
}

pub(crate) fn summary(tx: &Transaction<'_>) -> Result<RetentionSummary, AppError> {
    tx.query_row(
        "SELECT event_count, output_count, outbox_count, content_bytes
         FROM retention_counters WHERE singleton = 1",
        [],
        |row| {
            Ok(RetentionSummary {
                event_count: row.get(0)?,
                output_count: row.get(1)?,
                outbox_count: row.get(2)?,
                content_bytes: row.get(3)?,
                max_records: MAX_RETAINED_RECORDS,
                max_content_bytes: MAX_RETAINED_CONTENT_BYTES,
            })
        },
    )
    .map_err(AppError::from)
}

/// Rejects a new durable event before projection when retaining it would exceed
/// the local product budget.  `incoming_content_bytes` is the plaintext result
/// size requested by the capture policy and may be zero for metadata-only facts.
pub(crate) fn check_capacity(
    tx: &Transaction<'_>,
    incoming_content_bytes: usize,
) -> Result<RetentionSummary, AppError> {
    let current = summary(tx)?;
    let incoming = i64::try_from(incoming_content_bytes)
        .map_err(|_| AppError::new("RETENTION_CAPACITY_EXCEEDED", "接管内容超过本地容量上限"))?;
    let records = current
        .event_count
        .saturating_add(current.output_count)
        .saturating_add(current.outbox_count)
        .saturating_add(1);
    if records > MAX_RETAINED_RECORDS
        || current.content_bytes.saturating_add(incoming) > MAX_RETAINED_CONTENT_BYTES
    {
        return Err(AppError::new(
            "RETENTION_CAPACITY_EXCEEDED",
            "本地保留容量已满；请等待安全清理完成后再接管新事件",
        ));
    }
    Ok(current)
}

/// Performs at most one small lifecycle batch.  Full text is removed after
/// seven days after terminal completion; lightweight event identity remains
/// until 30 days after terminal completion to preserve the replay/deduplication
/// window. Unknown captures deliberately survive both passes for operator
/// diagnosis instead of being silently discarded.
pub(crate) fn prune_batch(db: &Db, now: i64) -> Result<RetentionPruneResult, AppError> {
    let body_cutoff = now.saturating_sub(BODY_RETENTION_MS);
    let history_cutoff = now.saturating_sub(HISTORY_RETENTION_MS);
    db.with_transaction(|tx| {
        let scrubbed_outputs = tx.execute(
            "UPDATE agent_outputs
             SET raw_text = '', content_available = 0, content_bytes = 0
             WHERE id IN (
                 SELECT output.id
                 FROM agent_outputs AS output
                 JOIN agent_runs AS run ON run.run_key = output.run_key
                 WHERE output.content_available = 1
                   AND run.status IN ('completed', 'failed', 'interrupted', 'cancelled', 'abandoned')
                   AND MAX(output.observed_at, CASE run.status
                       WHEN 'completed' THEN MAX(COALESCE(run.completed_at, run.last_event_at), run.last_event_at)
                       WHEN 'failed' THEN MAX(COALESCE(run.failed_at, run.last_event_at), run.last_event_at)
                       WHEN 'interrupted' THEN MAX(COALESCE(run.interrupted_at, run.last_event_at), run.last_event_at)
                       WHEN 'cancelled' THEN MAX(COALESCE(run.cancelled_at, run.last_event_at), run.last_event_at)
                       ELSE run.last_event_at
                   END) <= ?1
                   AND NOT EXISTS (
                       SELECT 1 FROM notification_outbox AS outbox
                       LEFT JOIN relay_result_publications AS result ON result.outbox_id = outbox.id
                       WHERE outbox.agent_run_key = run.run_key
                         AND (outbox.status NOT IN ('cancelled', 'expired', 'dead_letter', 'delivered')
                              OR (outbox.status = 'delivered' AND outbox.acceptance_stage = 'relay'
                                  AND (outbox.remote_status IS NULL
                                       OR outbox.remote_status NOT IN ('provider_accepted', 'dead_letter', 'expired', 'cancelled', 'blocked_target_changed')))
                              OR result.pending_resend_request_id IS NOT NULL
                              OR result.pending_revoke_request_id IS NOT NULL)
                   )
                 ORDER BY output.observed_at ASC, output.id ASC
                 LIMIT ?2
             )",
            params![body_cutoff, RETENTION_BATCH_SIZE],
        )?;
        let scrubbed_outbox_payloads = scrub_outbox_payloads(tx, body_cutoff, now)?;
        let scrubbed_event_metadata = scrub_event_metadata(tx, body_cutoff, now)?;
        let deleted_outputs = tx.execute(
            "DELETE FROM agent_outputs
             WHERE id IN (
                 SELECT output.id
                 FROM agent_outputs AS output
                 LEFT JOIN agent_runs AS run ON run.run_key = output.run_key
                 WHERE ((run.run_key IS NULL AND output.observed_at <= ?1)
                        OR (run.status IN ('completed', 'failed', 'interrupted', 'cancelled', 'abandoned')
                            AND MAX(output.observed_at, CASE run.status
                                WHEN 'completed' THEN MAX(COALESCE(run.completed_at, run.last_event_at), run.last_event_at)
                                WHEN 'failed' THEN MAX(COALESCE(run.failed_at, run.last_event_at), run.last_event_at)
                                WHEN 'interrupted' THEN MAX(COALESCE(run.interrupted_at, run.last_event_at), run.last_event_at)
                                WHEN 'cancelled' THEN MAX(COALESCE(run.cancelled_at, run.last_event_at), run.last_event_at)
                                ELSE run.last_event_at
                            END) <= ?1))
                   AND NOT EXISTS (
                       SELECT 1 FROM notification_outbox AS outbox
                       LEFT JOIN relay_result_publications AS result ON result.outbox_id = outbox.id
                       WHERE outbox.agent_run_key = output.run_key
                         AND (outbox.status NOT IN ('cancelled', 'expired', 'dead_letter', 'delivered')
                              OR (outbox.status = 'delivered' AND outbox.acceptance_stage = 'relay'
                                  AND (outbox.remote_status IS NULL
                                       OR outbox.remote_status NOT IN ('provider_accepted', 'dead_letter', 'expired', 'cancelled', 'blocked_target_changed')))
                              OR result.pending_resend_request_id IS NOT NULL
                              OR result.pending_revoke_request_id IS NOT NULL)
                   )
                 ORDER BY output.observed_at ASC, output.id ASC
                 LIMIT ?2
             )",
            params![history_cutoff, RETENTION_BATCH_SIZE],
        )?;
        let deleted_events = tx.execute(
            "DELETE FROM agent_events
             WHERE id IN (
                 SELECT event.id
                 FROM agent_events AS event
                 LEFT JOIN agent_runs AS run ON run.run_key = event.run_key
                 WHERE event.retention_class IN ('normal', 'verification')
                   AND ((run.run_key IS NULL AND event.observed_at <= ?1)
                        OR (run.status IN ('completed', 'failed', 'interrupted', 'cancelled', 'abandoned')
                            AND MAX(event.observed_at, CASE run.status
                                WHEN 'completed' THEN MAX(COALESCE(run.completed_at, run.last_event_at), run.last_event_at)
                                WHEN 'failed' THEN MAX(COALESCE(run.failed_at, run.last_event_at), run.last_event_at)
                                WHEN 'interrupted' THEN MAX(COALESCE(run.interrupted_at, run.last_event_at), run.last_event_at)
                                WHEN 'cancelled' THEN MAX(COALESCE(run.cancelled_at, run.last_event_at), run.last_event_at)
                                ELSE run.last_event_at
                            END) <= ?1))
                   AND NOT EXISTS (
                       SELECT 1 FROM notification_outbox AS outbox
                       LEFT JOIN relay_result_publications AS result ON result.outbox_id = outbox.id
                       WHERE outbox.agent_run_key = event.run_key
                         AND (outbox.status NOT IN ('cancelled', 'expired', 'dead_letter', 'delivered')
                              OR (outbox.status = 'delivered' AND outbox.acceptance_stage = 'relay'
                                  AND (outbox.remote_status IS NULL
                                       OR outbox.remote_status NOT IN ('provider_accepted', 'dead_letter', 'expired', 'cancelled', 'blocked_target_changed')))
                              OR result.pending_resend_request_id IS NOT NULL
                              OR result.pending_revoke_request_id IS NOT NULL)
                   )
                 ORDER BY event.observed_at ASC, event.id ASC
                 LIMIT ?2
             )",
            params![history_cutoff, RETENTION_BATCH_SIZE],
        )?;
        let deleted_runs = tx.execute(
            "DELETE FROM agent_runs
             WHERE run_key IN (
                 SELECT run.run_key
                 FROM agent_runs AS run
                 WHERE run.status IN ('completed', 'failed', 'interrupted', 'cancelled', 'abandoned')
                   AND CASE run.status
                       WHEN 'completed' THEN MAX(COALESCE(run.completed_at, run.last_event_at), run.last_event_at)
                       WHEN 'failed' THEN MAX(COALESCE(run.failed_at, run.last_event_at), run.last_event_at)
                       WHEN 'interrupted' THEN MAX(COALESCE(run.interrupted_at, run.last_event_at), run.last_event_at)
                       WHEN 'cancelled' THEN MAX(COALESCE(run.cancelled_at, run.last_event_at), run.last_event_at)
                       ELSE run.last_event_at
                   END <= ?1
                   AND NOT EXISTS (SELECT 1 FROM agent_outputs AS output WHERE output.run_key = run.run_key)
                   AND NOT EXISTS (
                       SELECT 1 FROM notification_outbox AS outbox
                       LEFT JOIN relay_result_publications AS result ON result.outbox_id = outbox.id
                       WHERE outbox.agent_run_key = run.run_key
                         AND (outbox.status NOT IN ('cancelled', 'expired', 'dead_letter', 'delivered')
                              OR (outbox.status = 'delivered' AND outbox.acceptance_stage = 'relay'
                                  AND (outbox.remote_status IS NULL
                                       OR outbox.remote_status NOT IN ('provider_accepted', 'dead_letter', 'expired', 'cancelled', 'blocked_target_changed')))
                              OR result.pending_resend_request_id IS NOT NULL
                              OR result.pending_revoke_request_id IS NOT NULL)
                   )
                 ORDER BY run.last_event_at ASC, run.run_key ASC
                 LIMIT ?2
             )",
            params![history_cutoff, RETENTION_BATCH_SIZE],
        )?;
        Ok(RetentionPruneResult {
            scrubbed_outputs,
            scrubbed_outbox_payloads,
            scrubbed_event_metadata,
            deleted_outputs,
            deleted_events,
            deleted_runs,
        })
    })
}

fn scrub_outbox_payloads(
    tx: &Transaction<'_>,
    body_cutoff: i64,
    now: i64,
) -> Result<usize, AppError> {
    let ids = {
        let mut statement = tx.prepare(
            "SELECT outbox.id FROM notification_outbox AS outbox
             LEFT JOIN relay_result_publications AS result ON result.outbox_id = outbox.id
             WHERE outbox.updated_at <= ? AND outbox.content_scrubbed_at IS NULL
               AND outbox.status IN ('delivered', 'cancelled', 'expired', 'dead_letter')
               AND NOT (outbox.status = 'delivered' AND outbox.acceptance_stage = 'relay'
                        AND (outbox.remote_status IS NULL
                             OR outbox.remote_status NOT IN ('provider_accepted', 'dead_letter', 'expired', 'cancelled', 'blocked_target_changed')))
               AND result.pending_resend_request_id IS NULL
               AND result.pending_revoke_request_id IS NULL
             ORDER BY outbox.updated_at ASC, outbox.id ASC LIMIT ?",
        )?;
        let mapped = statement.query_map(params![body_cutoff, RETENTION_BATCH_SIZE], |row| {
            row.get::<_, String>(0)
        })?;
        mapped.collect::<Result<Vec<_>, _>>()?
    };
    let mut scrubbed = 0;
    for id in ids {
        let mut payload = crate::notification::outbox::NotificationPayloadV2::for_run(
            "PromptDock",
            None,
            None,
            None,
            None,
            None,
        );
        payload.title = "本地正文已清除".into();
        payload.body = "本地保留期已结束；投递元数据仍可查看。".into();
        let protected = crate::content_crypto::protect_json(
            crate::content_crypto::ContentPurpose::NotificationPayload,
            &payload,
        )?;
        scrubbed += tx.execute(
            "UPDATE notification_outbox
             SET payload_json = ?, list_body_available = 0, content_scrubbed_at = ?
             WHERE id = ?",
            params![protected, now, id],
        )?;
    }
    Ok(scrubbed)
}

fn scrub_event_metadata(
    tx: &Transaction<'_>,
    body_cutoff: i64,
    now: i64,
) -> Result<usize, AppError> {
    let rows = {
        let mut statement = tx.prepare(
            "SELECT event.id, event.metadata_json FROM agent_events AS event
             LEFT JOIN agent_runs AS run ON run.run_key = event.run_key
             WHERE event.retention_class = 'normal'
               AND event.content_scrubbed_at IS NULL
               AND ((run.run_key IS NULL AND event.observed_at <= ?1)
                    OR (run.status IN ('completed', 'failed', 'interrupted', 'cancelled', 'abandoned')
                        AND MAX(event.observed_at, CASE run.status
                            WHEN 'completed' THEN MAX(COALESCE(run.completed_at, run.last_event_at), run.last_event_at)
                            WHEN 'failed' THEN MAX(COALESCE(run.failed_at, run.last_event_at), run.last_event_at)
                            WHEN 'interrupted' THEN MAX(COALESCE(run.interrupted_at, run.last_event_at), run.last_event_at)
                            WHEN 'cancelled' THEN MAX(COALESCE(run.cancelled_at, run.last_event_at), run.last_event_at)
                            ELSE run.last_event_at
                        END) <= ?1))
               AND NOT EXISTS (
                   SELECT 1 FROM notification_outbox AS outbox
                   LEFT JOIN relay_result_publications AS result ON result.outbox_id = outbox.id
                   WHERE outbox.agent_run_key = event.run_key
                     AND (outbox.status NOT IN ('cancelled', 'expired', 'dead_letter', 'delivered')
                          OR (outbox.status = 'delivered' AND outbox.acceptance_stage = 'relay'
                              AND (outbox.remote_status IS NULL
                                   OR outbox.remote_status NOT IN ('provider_accepted', 'dead_letter', 'expired', 'cancelled', 'blocked_target_changed')))
                          OR result.pending_resend_request_id IS NOT NULL
                          OR result.pending_revoke_request_id IS NOT NULL)
               )
             ORDER BY event.observed_at ASC, event.id ASC LIMIT ?2",
        )?;
        let mapped = statement.query_map(params![body_cutoff, RETENTION_BATCH_SIZE], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        mapped.collect::<Result<Vec<_>, _>>()?
    };
    let mut scrubbed = 0;
    for (id, protected) in rows {
        let Ok(metadata) = crate::content_crypto::unprotect_json::<Value>(
            crate::content_crypto::ContentPurpose::AgentEventMetadata,
            &protected,
        ) else {
            continue;
        };
        let retained = metadata
            .get("captureMetadata")
            .cloned()
            .map(|capture| serde_json::json!({ "captureMetadata": capture }))
            .unwrap_or_else(|| serde_json::json!({}));
        let protected = crate::content_crypto::protect_json(
            crate::content_crypto::ContentPurpose::AgentEventMetadata,
            &retained,
        )?;
        scrubbed += tx.execute(
            "UPDATE agent_events SET metadata_json = ?, content_scrubbed_at = ? WHERE id = ?",
            params![protected, now, id],
        )?;
    }
    Ok(scrubbed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_is_constant_time_and_fails_with_an_explicit_backpressure_code() {
        let db = Db::open_in_memory().unwrap();
        db.with_transaction(|tx| {
            tx.execute(
                "UPDATE retention_counters SET content_bytes = ? WHERE singleton = 1",
                [MAX_RETAINED_CONTENT_BYTES],
            )?;
            Ok(())
        })
        .unwrap();
        let error = db
            .with_transaction(|tx| check_capacity(tx, 1).map(|_| ()))
            .unwrap_err();
        assert_eq!(error.code, "RETENTION_CAPACITY_EXCEEDED");
    }

    #[test]
    fn unknown_event_identity_is_not_silently_collected() {
        let db = Db::open_in_memory().unwrap();
        let now = HISTORY_RETENTION_MS + 1;
        db.with_transaction(|tx| {
            tx.execute(
                "INSERT INTO source_streams(source_key, agent_kind, source_id, source_kind, instance_id, created_at)
                 VALUES ('source', 'fixture', 'source', 'jsonl', 'default', 0)",
                [],
            )?;
            for (id, class) in [("normal", "normal"), ("unknown", "unknown")] {
                tx.execute(
                    "INSERT INTO agent_events(
                        id, source_key, event_id, event_kind, occurred_at, observed_at,
                        last_observed_at, retention_class, metadata_json, payload_hash
                     ) VALUES (?1, 'source', ?1, 'prompt_submitted', 0, 0, 0, ?2, '{}', ?3)",
                    params![id, class, format!("hash-{id}")],
                )?;
            }
            Ok(())
        })
        .unwrap();
        let result = prune_batch(&db, now).unwrap();
        assert_eq!(result.deleted_events, 1);
        let remaining: String = db
            .with_connection(|conn| {
                conn.query_row("SELECT id FROM agent_events", [], |row| row.get(0))
                    .map_err(AppError::from)
            })
            .unwrap();
        assert_eq!(remaining, "unknown");
    }

    #[test]
    fn terminal_retention_starts_when_a_long_running_run_reaches_its_terminal_time() {
        let db = Db::open_in_memory().unwrap();
        let completed_at = HISTORY_RETENTION_MS + 1;
        let event_metadata = crate::content_crypto::protect_json(
            crate::content_crypto::ContentPurpose::AgentEventMetadata,
            &serde_json::json!({ "body": "old diagnostic" }),
        )
        .unwrap();
        db.with_transaction(|tx| {
            tx.execute(
                "INSERT INTO source_streams(source_key, agent_kind, source_id, source_kind, instance_id, created_at)
                 VALUES ('source', 'fixture', 'source', 'jsonl', 'default', 0)",
                [],
            )?;
            tx.execute(
                "INSERT INTO agent_runs(
                    run_key, agent_kind, instance_id, agent_label, status,
                    started_at, completed_at, last_event_at
                 ) VALUES ('long-run', 'fixture', 'default', 'Fixture', 'completed', 0, ?1, ?1)",
                [completed_at],
            )?;
            tx.execute(
                "INSERT INTO agent_events(
                    id, source_key, event_id, event_kind, run_key, occurred_at, observed_at,
                    last_observed_at, retention_class, metadata_json, payload_hash
                 ) VALUES ('event', 'source', 'event', 'run_completed', 'long-run',
                           0, 0, 0, 'normal', ?1, 'event-hash')",
                [&event_metadata],
            )?;
            tx.execute(
                "INSERT INTO agent_outputs(
                    id, run_key, source_key, event_id, output_kind, raw_text, content_hash,
                    content_mode, content_available, content_bytes, result_revision,
                    occurred_at, observed_at
                 ) VALUES ('output', 'long-run', 'source', 'output-event', 'assistant_final',
                           'old full output', 'output-hash', 'full_final', 1, 15, 1, 0, 0)",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        let fresh_terminal = prune_batch(&db, completed_at).unwrap();
        assert!(!fresh_terminal.changed());
        db.with_connection(|conn| {
            let content_available: bool = conn.query_row(
                "SELECT content_available FROM agent_outputs WHERE id = 'output'",
                [],
                |row| row.get(0),
            )?;
            assert!(content_available);
            let events: i64 =
                conn.query_row("SELECT COUNT(*) FROM agent_events", [], |row| row.get(0))?;
            let outputs: i64 =
                conn.query_row("SELECT COUNT(*) FROM agent_outputs", [], |row| row.get(0))?;
            assert_eq!((events, outputs), (1, 1));
            Ok(())
        })
        .unwrap();

        let expired_terminal = prune_batch(&db, completed_at + HISTORY_RETENTION_MS + 1).unwrap();
        assert_eq!(expired_terminal.deleted_events, 1);
        assert_eq!(expired_terminal.deleted_outputs, 1);
        assert_eq!(expired_terminal.deleted_runs, 1);
    }

    fn remote_terminal_fixture(remote_status: Option<&str>) -> Db {
        let db = Db::open_in_memory().unwrap();
        let metadata = crate::content_crypto::protect_json(
            crate::content_crypto::ContentPurpose::AgentEventMetadata,
            &serde_json::json!({ "body": "retain while remote queued" }),
        )
        .unwrap();
        let mut payload = crate::notification::outbox::NotificationPayloadV2::for_run(
            "Fixture",
            None,
            None,
            Some(0),
            Some(0),
            Some(0),
        );
        payload.title = "Frozen terminal output".into();
        payload.body = "frozen body".into();
        payload.content_mode = crate::model::ResultContentMode::FullFinal;
        payload.result_revision = Some(1);
        payload.source_hash = Some(crate::agent::source_hash_sha256(&payload.body));
        payload.content_bytes = payload.body.len();
        payload.validate().unwrap();
        let protected_payload = crate::content_crypto::protect_json(
            crate::content_crypto::ContentPurpose::NotificationPayload,
            &payload,
        )
        .unwrap();
        db.with_transaction(|tx| {
            tx.execute(
                "INSERT INTO source_streams(source_key, agent_kind, source_id, source_kind, instance_id, created_at)
                 VALUES ('source', 'fixture', 'source', 'jsonl', 'default', 0)",
                [],
            )?;
            tx.execute(
                "INSERT INTO agent_runs(
                    run_key, agent_kind, instance_id, agent_label, status, completed_at, last_event_at
                 ) VALUES ('run', 'fixture', 'default', 'Fixture', 'completed', 0, 0)",
                [],
            )?;
            tx.execute(
                "INSERT INTO agent_events(
                    id, source_key, event_id, event_kind, run_key, occurred_at, observed_at,
                    last_observed_at, retention_class, metadata_json, payload_hash
                 ) VALUES ('event', 'source', 'event', 'run_completed', 'run',
                           0, 0, 0, 'normal', ?1, 'event-hash')",
                [&metadata],
            )?;
            tx.execute(
                "INSERT INTO agent_outputs(
                    id, run_key, source_key, event_id, output_kind, raw_text, content_hash,
                    content_mode, content_available, content_bytes, result_revision,
                    occurred_at, observed_at
                 ) VALUES ('output', 'run', 'source', 'output-event', 'assistant_final',
                           'old output', 'output-hash', 'full_final', 1, 10, 1, 0, 0)",
                [],
            )?;
            tx.execute(
                "INSERT INTO notification_outbox(
                    id, agent_run_key, event_kind, dedupe_key, client_id, payload_json,
                    list_title, list_content_mode, list_content_bytes, list_body_available,
                    priority, status, acceptance_stage, remote_status,
                    not_before, expires_at, created_at, updated_at, delivered_at
                 ) VALUES ('outbox', 'run', 'run_completed', 'dedupe', 'client', ?1,
                           'Frozen terminal output', 'full_final', 11, 1,
                           1, 'delivered', 'relay', ?2, 0, 1, 0, 0, 0)",
                params![protected_payload, remote_status],
            )?;
            Ok(())
        })
        .unwrap();
        db
    }

    #[test]
    fn nonterminal_or_unknown_remote_states_preserve_all_local_terminal_content() {
        let now = HISTORY_RETENTION_MS + 1;
        for remote_status in [
            None,
            Some("delivery_unknown"),
            Some("pending_channel"),
            Some("retry_wait"),
            Some("sending_channel"),
            Some("queued"),
            Some("delivering"),
            Some("partial_failed"),
            Some("sending"),
            Some("blocked_activation"),
            Some("future_remote_state"),
        ] {
            let db = remote_terminal_fixture(remote_status);
            assert!(
                !prune_batch(&db, now).unwrap().changed(),
                "{remote_status:?} must fail closed"
            );
            db.with_connection(|conn| {
                let outputs: i64 =
                    conn.query_row("SELECT COUNT(*) FROM agent_outputs", [], |row| row.get(0))?;
                let events: i64 =
                    conn.query_row("SELECT COUNT(*) FROM agent_events", [], |row| row.get(0))?;
                let runs: i64 =
                    conn.query_row("SELECT COUNT(*) FROM agent_runs", [], |row| row.get(0))?;
                let payloads: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM notification_outbox WHERE content_scrubbed_at IS NULL",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!((outputs, events, runs, payloads), (1, 1, 1, 1));
                Ok(())
            })
            .unwrap();
        }
    }

    #[test]
    fn explicit_terminal_remote_states_allow_local_terminal_retention_to_advance() {
        let now = HISTORY_RETENTION_MS + 1;
        for remote_status in [
            "provider_accepted",
            "dead_letter",
            "expired",
            "cancelled",
            "blocked_target_changed",
        ] {
            let db = remote_terminal_fixture(Some(remote_status));
            let result = prune_batch(&db, now).unwrap();
            assert_eq!(result.deleted_outputs, 1, "{remote_status}");
            assert_eq!(result.deleted_events, 1, "{remote_status}");
            assert_eq!(result.deleted_runs, 1, "{remote_status}");
            // The run delete cascades the outbox row after its payload is
            // scrubbed, so the batch receipt is the durable observation.
            assert_eq!(result.scrubbed_outbox_payloads, 1, "{remote_status}");
        }
    }

    #[test]
    fn seven_day_scrub_removes_outbox_body_and_event_metadata_body() {
        let db = Db::open_in_memory().unwrap();
        let now = BODY_RETENTION_MS + 1;
        let event_metadata = crate::content_crypto::protect_json(
            crate::content_crypto::ContentPurpose::AgentEventMetadata,
            &serde_json::json!({
                "secretOutput": "do not retain",
                "captureMetadata": { "sourceReason": "unknown_stop" }
            }),
        )
        .unwrap();
        let mut payload = crate::notification::outbox::NotificationPayloadV2::for_run(
            "Secret terminal output",
            Some("do not retain"),
            None,
            None,
            None,
            None,
        );
        payload.title = "Secret terminal output".into();
        payload.body = "do not retain".into();
        let payload = crate::content_crypto::protect_json(
            crate::content_crypto::ContentPurpose::NotificationPayload,
            &payload,
        )
        .unwrap();
        db.with_transaction(|tx| {
            tx.execute(
                "INSERT INTO source_streams(source_key, agent_kind, source_id, source_kind, instance_id, created_at)
                 VALUES ('source', 'fixture', 'source', 'jsonl', 'default', 0)",
                [],
            )?;
            tx.execute(
                "INSERT INTO agent_events(
                    id, source_key, event_id, event_kind, occurred_at, observed_at,
                    last_observed_at, retention_class, metadata_json, payload_hash
                 ) VALUES ('event', 'source', 'event', 'run_completed', 0, 0, 0, 'normal', ?1, 'hash-event')",
                [&event_metadata],
            )?;
            tx.execute(
                "INSERT INTO notification_outbox(
                    id, event_kind, dedupe_key, client_id, payload_json,
                    list_title, list_content_mode, list_content_bytes, list_body_available,
                    priority, status, not_before, expires_at, created_at, updated_at
                 ) VALUES (
                    'outbox', 'test', 'dedupe', 'client', ?1,
                    'Secret terminal output', 'full_final', 13, 1,
                    0, 'cancelled', 0, 1, 0, 0
                 )",
                [&payload],
            )?;
            Ok(())
        })
        .unwrap();

        let result = prune_batch(&db, now).unwrap();
        assert_eq!(result.scrubbed_outbox_payloads, 1);
        assert_eq!(result.scrubbed_event_metadata, 1);
        let remaining_content_bytes = db
            .with_connection(|conn| {
                conn.query_row(
                    "SELECT content_bytes FROM retention_counters WHERE singleton = 1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(AppError::from)
            })
            .unwrap();
        assert_eq!(remaining_content_bytes, 0);
        db.with_connection(|conn| {
            let payload: String = conn.query_row(
                "SELECT payload_json FROM notification_outbox WHERE id = 'outbox'",
                [],
                |row| row.get(0),
            )?;
            let payload: crate::notification::outbox::NotificationPayloadV2 =
                crate::content_crypto::unprotect_json(
                    crate::content_crypto::ContentPurpose::NotificationPayload,
                    &payload,
                )?;
            assert_eq!(payload.title, "本地正文已清除");
            assert!(!payload.body.contains("do not retain"));
            let body_available: bool = conn.query_row(
                "SELECT list_body_available FROM notification_outbox WHERE id = 'outbox'",
                [],
                |row| row.get(0),
            )?;
            assert!(!body_available);

            let metadata: String = conn.query_row(
                "SELECT metadata_json FROM agent_events WHERE id = 'event'",
                [],
                |row| row.get(0),
            )?;
            let metadata: Value = crate::content_crypto::unprotect_json(
                crate::content_crypto::ContentPurpose::AgentEventMetadata,
                &metadata,
            )?;
            assert_eq!(
                metadata,
                serde_json::json!({
                    "captureMetadata": { "sourceReason": "unknown_stop" }
                })
            );
            Ok(())
        })
        .unwrap();
    }
}
