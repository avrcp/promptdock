use std::{sync::Arc, time::Duration};

use sqlx::SqlitePool;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::config::RetentionConfig;

const RETENTION_INTERVAL: Duration = Duration::from_secs(60 * 60);
const RETENTION_BATCH: i64 = 500;

#[derive(Clone)]
pub struct RetentionService {
    pool: SqlitePool,
    config: RetentionConfig,
    run_lock: Arc<Mutex<()>>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionRunReport {
    pub outbox_deleted: u64,
    pub inbound_deleted: u64,
    pub selection_deleted: u64,
    pub started_at: i64,
    pub completed_at: i64,
    #[serde(skip)]
    completed: bool,
}

impl RetentionRunReport {
    pub fn is_complete(self) -> bool {
        self.completed
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RoundResult {
    outbox: u64,
    inbound: u64,
    selections: u64,
    confirmations: u64,
}

impl RoundResult {
    fn may_have_more(self) -> bool {
        [
            self.outbox,
            self.inbound,
            self.selections,
            self.confirmations,
        ]
        .into_iter()
        .any(|deleted| deleted == RETENTION_BATCH as u64)
    }
}

impl RetentionService {
    pub fn new(pool: SqlitePool, config: RetentionConfig) -> Self {
        Self {
            pool,
            config,
            run_lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn run(self, cancellation: CancellationToken) -> Result<(), RetentionError> {
        self.run_with_progress(cancellation, || {}).await
    }

    pub(crate) async fn run_with_progress<F>(
        self,
        cancellation: CancellationToken,
        completed_pass: F,
    ) -> Result<(), RetentionError>
    where
        F: Fn() + Send + 'static,
    {
        let report = self.run_once(&cancellation).await?;
        if report.is_complete() {
            completed_pass();
        }
        let mut interval = tokio::time::interval(RETENTION_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await;
        loop {
            tokio::select! {
                () = cancellation.cancelled() => return Ok(()),
                _ = interval.tick() => {
                    let report = self.run_once(&cancellation).await?;
                    if report.is_complete() {
                        completed_pass();
                    }
                }
            }
        }
    }

    pub async fn run_once(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<RetentionRunReport, RetentionError> {
        let started_at = unix_timestamp_ms()?;
        let _run_guard = tokio::select! {
            () = cancellation.cancelled() => return Ok(RetentionRunReport {
                started_at,
                completed_at: started_at,
                ..RetentionRunReport::default()
            }),
            guard = self.run_lock.lock() => guard,
        };
        let mut report = RetentionRunReport {
            started_at,
            completed_at: started_at,
            ..RetentionRunReport::default()
        };
        loop {
            if cancellation.is_cancelled() {
                break;
            }
            let result = self
                .run_round_at(unix_timestamp_ms()?, cancellation)
                .await?;
            report.outbox_deleted = report.outbox_deleted.saturating_add(result.outbox);
            report.inbound_deleted = report.inbound_deleted.saturating_add(result.inbound);
            report.selection_deleted = report.selection_deleted.saturating_add(result.selections);
            if !result.may_have_more() {
                report.completed = true;
                break;
            }
            tokio::task::yield_now().await;
        }
        report.completed_at = unix_timestamp_ms()?.max(report.started_at);
        Ok(report)
    }

    async fn run_round_at(
        &self,
        now: i64,
        cancellation: &CancellationToken,
    ) -> Result<RoundResult, RetentionError> {
        let mut result = RoundResult::default();
        if cancellation.is_cancelled() {
            return Ok(result);
        }
        result.outbox = self.delete_outbox_at(now).await?;
        tokio::task::yield_now().await;

        if cancellation.is_cancelled() {
            return Ok(result);
        }
        result.inbound = self.delete_inbound_at(now).await?;
        tokio::task::yield_now().await;

        if cancellation.is_cancelled() {
            return Ok(result);
        }
        (result.selections, result.confirmations) = self.delete_selections_at(now).await?;
        Ok(result)
    }

    async fn delete_outbox_at(&self, now: i64) -> Result<u64, RetentionError> {
        let purged_results = sqlx::query(
            "UPDATE results SET body_ciphertext=NULL, body_nonce=NULL, body_purged_at=?1,
                 updated_at=MAX(updated_at,?1)
             WHERE id IN (SELECT id FROM results WHERE body_purged_at IS NULL
                 AND body_retain_until <= ?1 ORDER BY body_retain_until,id LIMIT ?2)",
        )
        .bind(now)
        .bind(RETENTION_BATCH)
        .execute(&self.pool)
        .await
        .map_err(|_| RetentionError::Database)?
        .rows_affected();
        if purged_results > 0 {
            let _ = crate::db::checkpoint_wal(&self.pool).await;
        }
        crate::outbox::OutboxService::new(self.pool.clone())
            .purge_bundle_content_at(now, RETENTION_BATCH)
            .await
            .map_err(|_| RetentionError::Database)?;
        let mut outbox_tx = self
            .pool
            .begin()
            .await
            .map_err(|_| RetentionError::Database)?;
        // A result receipt is durable evidence. Before expiring its associated
        // outbox row, persist the last observed provider state on the result so
        // an owner replay never regresses from accepted/dead-letter to pending.
        sqlx::query(
            "UPDATE results
             SET notification_status=(SELECT o.status FROM notification_outbox o
                                      WHERE o.notification_id=results.current_notification_id
                                        AND o.result_row_id=results.id),
                 updated_at=MAX(updated_at,?3)
             WHERE id IN (
                 SELECT result_row_id FROM notification_outbox
                 WHERE result_row_id IS NOT NULL
                   AND notification_id IN (SELECT current_notification_id FROM results)
                   AND (result_row_id IS NULL OR NOT EXISTS (
                         SELECT 1 FROM results r
                         WHERE r.id=notification_outbox.result_row_id
                           AND r.page_expires_at > ?3
                       )) AND ((status IN ('provider_accepted', 'cancelled', 'expired')
                                  AND updated_at < ?1)
                               OR (status = 'dead_letter' AND updated_at < ?2))
                 ORDER BY updated_at, id LIMIT ?4
             )",
        )
        .bind(now - days_to_milliseconds(self.config.accepted_days))
        .bind(now - days_to_milliseconds(self.config.dead_letter_days))
        .bind(now)
        .bind(RETENTION_BATCH)
        .execute(&mut *outbox_tx)
        .await
        .map_err(|_| RetentionError::Database)?;
        let outbox_deleted = sqlx::query(
            "DELETE FROM notification_outbox WHERE id IN (
                 SELECT id FROM notification_outbox
                 WHERE (result_row_id IS NULL OR NOT EXISTS (
                           SELECT 1 FROM results r
                           WHERE r.id=notification_outbox.result_row_id
                             AND r.page_expires_at > ?3
                       )) AND ((status IN ('provider_accepted', 'cancelled', 'expired')
                                  AND updated_at < ?1)
                               OR (status = 'dead_letter' AND updated_at < ?2))
                 ORDER BY updated_at, id LIMIT ?4
             )",
        )
        .bind(now - days_to_milliseconds(self.config.accepted_days))
        .bind(now - days_to_milliseconds(self.config.dead_letter_days))
        .bind(now)
        .bind(RETENTION_BATCH)
        .execute(&mut *outbox_tx)
        .await
        .map(|result| result.rows_affected())
        .map_err(|_| RetentionError::Database)?;
        outbox_tx
            .commit()
            .await
            .map_err(|_| RetentionError::Database)?;
        let bundles_deleted = sqlx::query(
            "DELETE FROM notification_bundles WHERE id IN (
                 SELECT id FROM notification_bundles
                 WHERE (
                     status IN ('provider_accepted', 'expired') AND updated_at < ?1
                 ) OR (
                     status IN ('partial_failed', 'blocked_target_changed', 'delivery_unknown')
                     AND updated_at < ?2
                 )
                 ORDER BY updated_at, id LIMIT ?3
             )",
        )
        .bind(now - days_to_milliseconds(self.config.accepted_days))
        .bind(now - days_to_milliseconds(self.config.dead_letter_days))
        .bind(RETENTION_BATCH)
        .execute(&self.pool)
        .await
        .map_err(|_| RetentionError::Database)?
        .rows_affected();
        Ok(outbox_deleted + bundles_deleted)
    }

    async fn delete_inbound_at(&self, now: i64) -> Result<u64, RetentionError> {
        sqlx::query(
            "DELETE FROM inbound_commands WHERE message_key IN (
                 SELECT message_key FROM inbound_commands
                 WHERE (
                     status IN ('reply_queued', 'dead_letter') AND updated_at < ?1
                 ) OR (status = 'expired' AND updated_at < ?2)
                 ORDER BY updated_at, message_key LIMIT ?3
             )",
        )
        .bind(now - days_to_milliseconds(self.config.inbound_terminal_days))
        .bind(now - days_to_milliseconds(self.config.inbound_expired_days))
        .bind(RETENTION_BATCH)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected())
        .map_err(|_| RetentionError::Database)
    }

    async fn delete_selections_at(&self, now: i64) -> Result<(u64, u64), RetentionError> {
        let selections = sqlx::query(
            "DELETE FROM selection_contexts WHERE sender_fingerprint IN (
                 SELECT sender_fingerprint FROM selection_contexts
                 WHERE expires_at <= ?1
                 ORDER BY expires_at, sender_fingerprint LIMIT ?2
             )",
        )
        .bind(now)
        .bind(RETENTION_BATCH)
        .execute(&self.pool)
        .await
        .map_err(|_| RetentionError::Database)?
        .rows_affected();
        // Confirmations have a shorter, independent lifetime than inbound
        // messages and must disappear as soon as their two-minute TTL ends.
        // Keep this in the same bounded retention pass, but keep its count
        // separate from the public selection deletion report.
        let confirmations = sqlx::query(
            "DELETE FROM control_confirmations WHERE rowid IN (
                 SELECT rowid FROM control_confirmations
                 WHERE expires_at <= ?1
                   AND dispatch_state <> 'pending'
                 ORDER BY expires_at, sender_fingerprint, confirmation_id LIMIT ?2
             )",
        )
        .bind(now)
        .bind(RETENTION_BATCH)
        .execute(&self.pool)
        .await
        .map_err(|_| RetentionError::Database)?
        .rows_affected();
        Ok((selections, confirmations))
    }
}

fn days_to_milliseconds(days: u32) -> i64 {
    i64::from(days) * 24 * 60 * 60 * 1_000
}

fn unix_timestamp_ms() -> Result<i64, RetentionError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| RetentionError::Clock)
        .and_then(|duration| i64::try_from(duration.as_millis()).map_err(|_| RetentionError::Clock))
}

#[derive(Debug, Error)]
pub enum RetentionError {
    #[error("retention database operation failed")]
    Database,
    #[error("system clock is invalid")]
    Clock,
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use sqlx::Row;
    use tempfile::TempDir;

    use super::*;
    use crate::{config::DatabaseConfig, db};

    const NOW: i64 = 1_800_000_000_000;

    async fn service() -> (TempDir, RetentionService) {
        let directory = tempfile::tempdir().expect("directory");
        let pool = db::open(&DatabaseConfig {
            path: directory.path().join("relay.db"),
            ..DatabaseConfig::default()
        })
        .await
        .expect("database");
        let device = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO devices(id,name,token_hash,created_at) VALUES(?1,'retention',?2,1)",
        )
        .bind(&device)
        .bind(vec![0_u8; 32])
        .execute(&pool)
        .await
        .expect("device");
        (
            directory,
            RetentionService::new(pool, RetentionConfig::default()),
        )
    }

    async fn seed_round(service: &RetentionService, count: usize) {
        seed_round_at(service, count, NOW).await;
    }

    async fn seed_round_at(service: &RetentionService, count: usize, now: i64) {
        let device: String = sqlx::query_scalar("SELECT id FROM devices LIMIT 1")
            .fetch_one(&service.pool)
            .await
            .expect("device");
        let mut transaction = service.pool.begin().await.expect("transaction");
        for index in 0..count {
            let old = now - days_to_milliseconds(100);
            sqlx::query(
                "INSERT INTO notification_outbox(
                    id,origin_kind,origin_key,origin_device_id,notification_id,dedupe_key,
                    payload_hash,kind,title,body,priority,status,not_before,expires_at,
                    created_at,updated_at
                 ) VALUES(?1,'device','device:'||?2,?2,?1,?1,'hash','test','t','b',1,
                          'provider_accepted',?3,?4,?3,?3)",
            )
            .bind(format!("outbox-{index}"))
            .bind(&device)
            .bind(old)
            .bind(now + 1)
            .execute(&mut *transaction)
            .await
            .expect("outbox");
            sqlx::query(
                r#"INSERT INTO inbound_commands(
                    message_key,sender_fingerprint,command_kind,command_json,payload_hash,
                    status,expires_at,created_at,updated_at
                 ) VALUES(?1,?2,'help','{"action":"help"}',?3,'dead_letter',?5,?4,?4)"#,
            )
            .bind(format!("inbound-{index}"))
            .bind(format!("wx:{index:064x}"))
            .bind("a".repeat(64))
            .bind(old)
            .bind(now + 1)
            .execute(&mut *transaction)
            .await
            .expect("inbound");
            let fingerprint = format!("wx:{:064x}", index + count);
            sqlx::query(
                "INSERT INTO selection_contexts(sender_fingerprint,expires_at) VALUES(?1,?2)",
            )
            .bind(&fingerprint)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .expect("selection context");
            sqlx::query(
                "INSERT INTO selection_entries(sender_fingerprint,slot,device_id,item_kind)
                 VALUES(?1,1,?2,'device')",
            )
            .bind(&fingerprint)
            .bind(&device)
            .execute(&mut *transaction)
            .await
            .expect("selection entry");
        }
        transaction.commit().await.expect("commit");
    }

    #[tokio::test]
    async fn concurrent_runs_are_serialized_and_report_all_deleted_rows() {
        let (_directory, service) = service().await;
        let seeded_at = unix_timestamp_ms().expect("clock");
        seed_round_at(&service, 501, seeded_at).await;
        let first_service = service.clone();
        let first_cancellation = CancellationToken::new();
        let second_cancellation = CancellationToken::new();

        let (first, second) = tokio::join!(
            first_service.run_once(&first_cancellation),
            service.run_once(&second_cancellation),
        );
        let first = first.expect("first run");
        let second = second.expect("second run");

        assert_eq!(
            (
                first.outbox_deleted,
                first.inbound_deleted,
                first.selection_deleted
            ),
            (501, 501, 501)
        );
        assert_eq!(
            (
                second.outbox_deleted,
                second.inbound_deleted,
                second.selection_deleted
            ),
            (0, 0, 0)
        );
        assert!(first.started_at >= seeded_at);
        assert!(first.completed_at >= first.started_at);
        assert!(second.completed_at >= second.started_at);
        assert!(first.is_complete());
        assert!(second.is_complete());
        assert!(format!("{first:?}").contains("RetentionRunReport"));
        service.pool.close().await;
    }

    #[tokio::test]
    async fn cancellation_while_waiting_for_a_run_lock_does_not_clean_rows() {
        let (_directory, service) = service().await;
        seed_round_at(&service, 1, unix_timestamp_ms().expect("clock")).await;
        let run_guard = service.run_lock.lock().await;
        let waiting_service = service.clone();
        let cancellation = CancellationToken::new();
        let waiting_cancellation = cancellation.clone();
        let mut waiting =
            tokio::spawn(async move { waiting_service.run_once(&waiting_cancellation).await });

        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut waiting)
                .await
                .is_err()
        );
        cancellation.cancel();
        let report = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("cancelled wait completes")
            .expect("run task")
            .expect("report");
        assert_eq!(
            (
                report.outbox_deleted,
                report.inbound_deleted,
                report.selection_deleted
            ),
            (0, 0, 0)
        );
        assert!(report.started_at >= 0);
        assert!(report.completed_at >= report.started_at);
        assert!(!report.is_complete());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM notification_outbox")
                .fetch_one(&service.pool)
                .await
                .expect("outbox remains"),
            1
        );
        drop(run_guard);
        service.pool.close().await;
    }

    #[tokio::test]
    async fn automatic_progress_reports_only_complete_passes() {
        let (_directory, service) = service().await;
        let cancellation = CancellationToken::new();
        let callback_cancellation = cancellation.clone();
        let ticks = Arc::new(AtomicUsize::new(0));
        let callback_ticks = Arc::clone(&ticks);
        service
            .clone()
            .run_with_progress(cancellation, move || {
                callback_ticks.fetch_add(1, Ordering::AcqRel);
                callback_cancellation.cancel();
            })
            .await
            .expect("complete retention pass");
        assert_eq!(ticks.load(Ordering::Acquire), 1);

        let guard = service.run_lock.lock().await;
        let cancellation = CancellationToken::new();
        let run_cancellation = cancellation.clone();
        let interrupted_ticks = Arc::new(AtomicUsize::new(0));
        let callback_ticks = Arc::clone(&interrupted_ticks);
        let waiting_service = service.clone();
        let waiting = tokio::spawn(async move {
            waiting_service
                .run_with_progress(run_cancellation, move || {
                    callback_ticks.fetch_add(1, Ordering::AcqRel);
                })
                .await
        });
        tokio::task::yield_now().await;
        cancellation.cancel();
        waiting
            .await
            .expect("interrupted task")
            .expect("interrupted pass");
        assert_eq!(interrupted_ticks.load(Ordering::Acquire), 0);
        drop(guard);

        service.pool.close().await;
        let failure_ticks = Arc::new(AtomicUsize::new(0));
        let callback_ticks = Arc::clone(&failure_ticks);
        service
            .run_with_progress(CancellationToken::new(), move || {
                callback_ticks.fetch_add(1, Ordering::AcqRel);
            })
            .await
            .expect_err("closed database must fail");
        assert_eq!(failure_ticks.load(Ordering::Acquire), 0);
    }

    #[test]
    fn reports_are_debuggable_and_errors_are_generic() {
        let report = RetentionRunReport {
            outbox_deleted: 1,
            inbound_deleted: 2,
            selection_deleted: 3,
            started_at: 4,
            completed_at: 5,
            completed: true,
        };
        assert!(format!("{report:?}").contains("RetentionRunReport"));
        assert_eq!(
            serde_json::to_value(report).expect("report JSON"),
            serde_json::json!({
                "outboxDeleted": 1,
                "inboundDeleted": 2,
                "selectionDeleted": 3,
                "startedAt": 4,
                "completedAt": 5,
            })
        );
        assert_eq!(
            RetentionError::Database.to_string(),
            "retention database operation failed"
        );
        assert_eq!(RetentionError::Clock.to_string(), "system clock is invalid");
    }

    #[tokio::test]
    async fn one_round_is_fair_batched_and_selection_cascades() {
        let (_directory, service) = service().await;
        seed_round(&service, 501).await;
        let result = service
            .run_round_at(NOW, &CancellationToken::new())
            .await
            .expect("round");
        assert_eq!(
            result,
            RoundResult {
                outbox: 500,
                inbound: 500,
                selections: 500,
                confirmations: 0
            }
        );
        let remaining = sqlx::query(
            "SELECT
                (SELECT COUNT(*) FROM notification_outbox) AS outbox,
                (SELECT COUNT(*) FROM inbound_commands) AS inbound,
                (SELECT COUNT(*) FROM selection_contexts) AS contexts,
                (SELECT COUNT(*) FROM selection_entries) AS entries",
        )
        .fetch_one(&service.pool)
        .await
        .expect("remaining counts");
        for column in ["outbox", "inbound", "contexts", "entries"] {
            assert_eq!(
                remaining.get::<i64, _>(column),
                1,
                "unexpected {column} count"
            );
        }
        service.pool.close().await;
    }

    #[tokio::test]
    async fn confirmation_batch_is_counted_separately_from_selections() {
        let (_directory, service) = service().await;
        seed_round_at(&service, 501, NOW).await;
        let device: String = sqlx::query_scalar("SELECT id FROM devices LIMIT 1")
            .fetch_one(&service.pool)
            .await
            .expect("device");
        sqlx::query(
            "INSERT INTO control_confirmations(
                 sender_fingerprint, confirmation_id, confirmation_digest,
                 confirmation_nonce, device_id, action_kind, run_handle,
                 intent_id, expires_at, dispatch_state
             ) VALUES(?1, ?2, ?3, ?4, ?5, 'cancel_run', ?6, ?7, ?8, 'cancelled')",
        )
        .bind(format!("wx:{}", "f".repeat(64)))
        .bind("a".repeat(64))
        .bind("b".repeat(64))
        .bind("c".repeat(32))
        .bind(device)
        .bind("run_0123456789abcdef0123456789abcdef")
        .bind("intent_0123456789abcdef0123456789abcdef")
        .bind(NOW - 1)
        .execute(&service.pool)
        .await
        .expect("expired confirmation");

        let result = service
            .run_round_at(NOW, &CancellationToken::new())
            .await
            .expect("round");
        assert_eq!(result.selections, 500);
        assert_eq!(result.confirmations, 1);
        assert!(result.may_have_more());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM selection_contexts")
                .fetch_one(&service.pool)
                .await
                .expect("remaining selections"),
            1
        );
        service.pool.close().await;
    }

    #[tokio::test]
    async fn exact_age_boundaries_are_preserved_and_selection_expiry_is_immediate() {
        let (_directory, service) = service().await;
        let device: String = sqlx::query_scalar("SELECT id FROM devices LIMIT 1")
            .fetch_one(&service.pool)
            .await
            .expect("device");
        let accepted_boundary = NOW - days_to_milliseconds(30);
        sqlx::query(
            "INSERT INTO notification_outbox(
                id,origin_kind,origin_key,origin_device_id,notification_id,dedupe_key,
                payload_hash,kind,title,body,priority,status,not_before,expires_at,
                created_at,updated_at
             ) VALUES('boundary','device','device:'||?1,?1,'boundary','boundary','hash',
                      'test','t','b',1,'provider_accepted',?2,?3,?2,?2)",
        )
        .bind(&device)
        .bind(accepted_boundary)
        .bind(NOW + 1)
        .execute(&service.pool)
        .await
        .expect("outbox boundary");
        let expired_boundary = NOW - days_to_milliseconds(2);
        sqlx::query(
            r#"INSERT INTO inbound_commands(
                message_key,sender_fingerprint,command_kind,command_json,payload_hash,
                status,expires_at,created_at,updated_at
             ) VALUES('boundary',?1,'help','{"action":"help"}',?2,'expired',?3,?4,?4)"#,
        )
        .bind(format!("wx:{}", "b".repeat(64)))
        .bind("a".repeat(64))
        .bind(NOW + 1)
        .bind(expired_boundary)
        .execute(&service.pool)
        .await
        .expect("inbound boundary");
        let selection = format!("wx:{}", "c".repeat(64));
        sqlx::query("INSERT INTO selection_contexts(sender_fingerprint,expires_at) VALUES(?1,?2)")
            .bind(&selection)
            .bind(NOW)
            .execute(&service.pool)
            .await
            .expect("selection boundary");
        sqlx::query(
            "INSERT INTO selection_entries(sender_fingerprint,slot,device_id,item_kind)
             VALUES(?1,1,?2,'device')",
        )
        .bind(&selection)
        .bind(&device)
        .execute(&service.pool)
        .await
        .expect("selection entry");

        assert_eq!(
            service
                .run_round_at(NOW, &CancellationToken::new())
                .await
                .expect("boundary round"),
            RoundResult {
                outbox: 0,
                inbound: 0,
                selections: 1,
                confirmations: 0
            }
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM notification_outbox")
                .fetch_one(&service.pool)
                .await
                .expect("outbox retained"),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM inbound_commands")
                .fetch_one(&service.pool)
                .await
                .expect("inbound retained"),
            1
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM selection_entries")
                .fetch_one(&service.pool)
                .await
                .expect("entry cascade"),
            0
        );
        service.pool.close().await;
    }

    #[tokio::test]
    async fn cancellation_before_round_preserves_eligible_rows() {
        let (_directory, service) = service().await;
        seed_round(&service, 1).await;
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert_eq!(
            service
                .run_round_at(NOW, &cancellation)
                .await
                .expect("cancelled"),
            RoundResult::default()
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_outbox")
            .fetch_one(&service.pool)
            .await
            .expect("preserved");
        assert_eq!(count, 1);
        service.pool.close().await;
    }
}
