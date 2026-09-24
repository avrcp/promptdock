// Historical bundle persistence is deliberately retained for clean-break
// database inspection only. No production route or worker consumes it.
#[allow(dead_code)]
mod bundles;
mod claim;
mod crypto;
mod ingress;
mod model;
mod repository;
mod retry;
mod status;
mod validation;
mod worker;

pub use crypto::BundleContentCipher;
pub use model::{
    AcceptedNotification, AdminTest, ChannelMessage, ChannelOutboxStatus, ChannelOutcome,
    ClaimedBundleSegment, ClaimedNotification, InteractiveReplyV1, NotificationBundleReceipt,
    NotificationBundleStatus, NotificationBundleV1, NotificationStatus, OutboxError,
    RelayNotificationV1, RetryClass,
};
pub use repository::OutboxService;
pub use worker::{FakeChannel, NotificationChannel, OutboxWorker};

#[cfg(test)]
use async_trait::async_trait;
#[cfg(test)]
use claim::STALE_CLAIM_MS;
#[cfg(test)]
use ingress::payload_hash;
#[cfg(test)]
use repository::unix_timestamp_ms;
#[cfg(test)]
use retry::{NETWORK_RETRY_MS, RATE_LIMIT_RETRY_MS, failure_transition};
#[cfg(test)]
use tokio::sync::Notify;
#[cfg(test)]
use tokio_util::sync::CancellationToken;
#[cfg(test)]
use uuid::Uuid;
#[cfg(test)]
use validation::{MAX_TTL_MS, validate_notification};
#[cfg(test)]
use worker::stable_client_id;

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use sha2::{Digest as _, Sha256};
    use sqlx::{Row as _, SqlitePool};
    use tempfile::TempDir;

    use super::*;
    use crate::{config::DatabaseConfig, db};

    const NOW: i64 = 1_800_000_000_000;

    async fn service() -> (TempDir, DatabaseConfig, OutboxService, Uuid) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let config = DatabaseConfig {
            path: directory.path().join("relay.db"),
            ..DatabaseConfig::default()
        };
        let pool = db::open(&config).await.expect("database");
        let device_id = insert_device(&pool, "DEVICE").await;
        (
            directory,
            config,
            OutboxService::new(pool).with_bundle_cipher(BundleContentCipher::new([42; 32])),
            device_id,
        )
    }

    async fn insert_device(pool: &SqlitePool, name: &str) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO devices (id, name, token_hash, enabled, created_at)
             VALUES (?1, ?2, ?3, 1, ?4)",
        )
        .bind(id.to_string())
        .bind(name)
        .bind(vec![0_u8; 32])
        .bind(NOW)
        .execute(pool)
        .await
        .expect("device");
        id
    }

    fn notification(id: &str, dedupe: &str) -> RelayNotificationV1 {
        RelayNotificationV1 {
            schema_version: 1,
            notification_id: id.to_owned(),
            dedupe_key: dedupe.to_owned(),
            kind: "run_completed".to_owned(),
            priority: 80,
            title: "任务完成".to_owned(),
            body: "safe rendered body".to_owned(),
            correlation_key: Some("opaque-correlation".to_owned()),
            created_at: NOW - 1_000,
            expires_at: NOW + 24 * 60 * 60 * 1_000,
        }
    }

    fn interactive_reply(id: &str, dedupe: &str) -> InteractiveReplyV1 {
        InteractiveReplyV1 {
            schema_version: 1,
            notification_id: id.to_owned(),
            dedupe_key: dedupe.to_owned(),
            priority: 80,
            target_account_fingerprint: format!("wx:{}", "a".repeat(64)),
            title: "PromptDock".to_owned(),
            body: "safe interactive reply".to_owned(),
            sensitive_body: false,
            correlation_key: Some("opaque-message-key".to_owned()),
            created_at: NOW - 1_000,
            expires_at: NOW + 24 * 60 * 60 * 1_000,
        }
    }

    fn bundle(id: &str, dedupe: &str, body: String) -> NotificationBundleV1 {
        NotificationBundleV1 {
            schema_version: 1,
            bundle_id: id.to_owned(),
            dedupe_key: dedupe.to_owned(),
            kind: "run_completed".to_owned(),
            content_mode: "full_final".to_owned(),
            source: "codex_stop".to_owned(),
            correlation_key: format!("run:{id}"),
            result_revision: 1,
            title: "Codex 本轮输出".to_owned(),
            source_hash: format!("{:x}", Sha256::digest(body.as_bytes())),
            body,
            created_at: NOW - 1_000,
            expires_at: NOW + 24 * 60 * 60 * 1_000,
        }
    }

    fn target() -> String {
        format!("wx:{}", "a".repeat(64))
    }

    #[tokio::test]
    async fn bundle_is_encrypted_idempotent_ordered_and_byte_exact() {
        let (directory, config, service, device_id) = service().await;
        let sentinel = "BUNDLE-PLAINTEXT-SENTINEL-";
        let body = format!(
            "目录:C:\\虚构\\项目\r\n```rust\n\tfn main() {{}}\n```\n{}{}",
            sentinel,
            "中文🙂e\u{301}\n".repeat(1_400)
        );
        let request = bundle("bundle-1", "bundle-dedupe-1", body.clone());
        let accepted = service
            .enqueue_bundle_at(device_id, target(), request.clone(), NOW)
            .await
            .expect("enqueue bundle");
        assert_eq!(accepted.status, NotificationBundleStatus::Queued);
        assert!(accepted.segment_count > 1);

        let stored = sqlx::query(
            "SELECT body_ciphertext, body_nonce, request_digest FROM notification_bundles
             WHERE bundle_id = 'bundle-1'",
        )
        .fetch_one(&service.pool)
        .await
        .expect("stored bundle");
        let ciphertext = stored.get::<Vec<u8>, _>("body_ciphertext");
        assert!(
            !ciphertext
                .windows(sentinel.len())
                .any(|window| window == sentinel.as_bytes())
        );
        assert_eq!(stored.get::<Vec<u8>, _>("body_nonce").len(), 24);
        assert_eq!(stored.get::<Vec<u8>, _>("request_digest").len(), 32);
        sqlx::query("PRAGMA wal_checkpoint(PASSIVE)")
            .execute(&service.pool)
            .await
            .expect("checkpoint encrypted bundle");
        fs::copy(&config.path, directory.path().join("relay-backup.db"))
            .expect("copy checkpointed database");
        for entry in fs::read_dir(directory.path()).expect("database artifacts") {
            let path = entry.expect("artifact entry").path();
            if path.is_file() {
                let bytes = fs::read(&path).expect("read database artifact");
                assert!(
                    !bytes
                        .windows(sentinel.len())
                        .any(|window| window == sentinel.as_bytes()),
                    "plaintext bundle leaked to {}",
                    path.display()
                );
            }
        }

        let blocked_successor = service
            .claim_next_bundle_segment_at(NOW)
            .await
            .expect("claim first")
            .expect("first segment");
        assert!(
            service
                .claim_next_bundle_segment_at(NOW + 1)
                .await
                .expect("blocked successor")
                .is_none()
        );

        let mut reconstructed = blocked_successor
            .body
            .split_once("\n\n")
            .expect("header")
            .1
            .as_bytes()
            .to_vec();
        service
            .finish_bundle_claim_at(
                &blocked_successor,
                ChannelOutcome::Accepted {
                    provider_message_id: Some("provider-0".to_owned()),
                },
                NOW + 2,
            )
            .await
            .expect("accept first");
        let mut now = NOW + 2 + 300;
        while let Some(claim) = service
            .claim_next_bundle_segment_at(now)
            .await
            .expect("claim successor")
        {
            reconstructed
                .extend_from_slice(claim.body.split_once("\n\n").expect("header").1.as_bytes());
            service
                .finish_bundle_claim_at(
                    &claim,
                    ChannelOutcome::Accepted {
                        provider_message_id: Some(format!("provider-{now}")),
                    },
                    now + 1,
                )
                .await
                .expect("accept segment");
            now += 301;
        }
        assert_eq!(reconstructed, body.as_bytes());
        let complete = service
            .bundle_status(device_id, "bundle-1")
            .await
            .expect("bundle status");
        assert_eq!(complete.status, NotificationBundleStatus::ProviderAccepted);
        assert_eq!(complete.accepted_segments, complete.segment_count);

        let replay = service
            .enqueue_bundle_at(device_id, target(), request.clone(), NOW + 10_000)
            .await
            .expect("replay after response loss");
        assert_eq!(replay.accepted_at, accepted.accepted_at);
        assert_eq!(replay.status, NotificationBundleStatus::ProviderAccepted);

        let mut conflict = request;
        conflict.body.push('!');
        conflict.source_hash = format!("{:x}", Sha256::digest(conflict.body.as_bytes()));
        assert_eq!(
            service
                .enqueue_bundle_at(device_id, target(), conflict, NOW + 10_001)
                .await
                .expect_err("content conflict"),
            OutboxError::IdempotencyConflict
        );
        service.pool.close().await;
    }

    #[tokio::test]
    async fn atomic_bundle_creation_rolls_back_parent_when_any_child_insert_fails() {
        let (_directory, _config, service, device_id) = service().await;
        sqlx::query(
            "CREATE TRIGGER fail_bundle_second_segment
             BEFORE INSERT ON notification_bundle_segments
             WHEN NEW.segment_index = 1
             BEGIN SELECT RAISE(ABORT, 'injected child failure'); END",
        )
        .execute(&service.pool)
        .await
        .expect("failure trigger");
        let request = bundle(
            "bundle-atomic",
            "bundle-atomic-dedupe",
            "中文🙂\n".repeat(2_000),
        );
        assert_eq!(
            service
                .enqueue_bundle_at(device_id, target(), request, NOW)
                .await
                .expect_err("atomic failure"),
            OutboxError::Database
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM notification_bundles WHERE bundle_id = 'bundle-atomic'",
            )
            .fetch_one(&service.pool)
            .await
            .expect("parent count"),
            0
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM notification_bundle_segments")
                .fetch_one(&service.pool)
                .await
                .expect("segment count"),
            0
        );
        service.pool.close().await;
    }

    #[tokio::test]
    async fn bundle_parent_updated_at_strictly_increases_with_same_millisecond_transitions() {
        let (_directory, _config, service, device_id) = service().await;
        let accepted = service
            .enqueue_bundle_at(
                device_id,
                target(),
                bundle(
                    "bundle-monotonic",
                    "bundle-monotonic-dedupe",
                    "one segment".to_owned(),
                ),
                NOW,
            )
            .await
            .expect("enqueue");
        let claim = service
            .claim_next_bundle_segment_at(NOW)
            .await
            .expect("claim")
            .expect("segment");
        let delivering = service
            .bundle_status(device_id, "bundle-monotonic")
            .await
            .expect("delivering");
        service
            .finish_bundle_claim_at(
                &claim,
                ChannelOutcome::Accepted {
                    provider_message_id: Some("provider-monotonic".to_owned()),
                },
                NOW,
            )
            .await
            .expect("finish");
        let complete = service
            .bundle_status(device_id, "bundle-monotonic")
            .await
            .expect("complete");
        assert_eq!(accepted.updated_at, NOW);
        assert!(delivering.updated_at > accepted.updated_at);
        assert!(complete.updated_at > delivering.updated_at);
        assert_eq!(complete.status, NotificationBundleStatus::ProviderAccepted);
        assert_eq!(complete.accepted_segments, 1);
        service.pool.close().await;
    }

    #[tokio::test]
    async fn corrupt_bundle_is_dead_lettered_without_poisoning_restart_or_other_work() {
        let (_directory, config, service, device_id) = service().await;
        let base = unix_timestamp_ms().expect("clock") - 1_000;
        let mut corrupt = bundle(
            "bundle-corrupt",
            "bundle-corrupt-dedupe",
            "corrupt me".to_owned(),
        );
        corrupt.created_at = base - 1_000;
        corrupt.expires_at = base + 86_400_000;
        service
            .enqueue_bundle_at(device_id, target(), corrupt, base)
            .await
            .expect("corrupt candidate");
        let mut healthy = bundle(
            "bundle-healthy",
            "bundle-healthy-dedupe",
            "healthy body".to_owned(),
        );
        healthy.created_at = base - 1_000;
        healthy.expires_at = base + 86_400_000;
        service
            .enqueue_bundle_at(device_id, target(), healthy, base + 1)
            .await
            .expect("healthy bundle");
        sqlx::query(
            "UPDATE notification_bundles
             SET body_ciphertext = zeroblob(length(body_ciphertext))
             WHERE bundle_id = 'bundle-corrupt'",
        )
        .execute(&service.pool)
        .await
        .expect("tamper ciphertext");

        assert!(
            service
                .claim_next_bundle_segment_at(base + 2)
                .await
                .expect("quarantine corrupt bundle")
                .is_none()
        );
        let corrupt_status = service
            .bundle_status(device_id, "bundle-corrupt")
            .await
            .expect("corrupt status");
        assert_eq!(
            corrupt_status.status,
            NotificationBundleStatus::PartialFailed
        );
        let corrupt_segment: (String, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT segment.status, segment.claim_token, segment.last_error_code
             FROM notification_bundle_segments AS segment
             JOIN notification_bundles AS bundle ON bundle.id = segment.bundle_row_id
             WHERE bundle.bundle_id = 'bundle-corrupt'",
        )
        .fetch_one(&service.pool)
        .await
        .expect("corrupt segment");
        assert_eq!(corrupt_segment.0, "dead_letter");
        assert!(corrupt_segment.1.is_none());
        assert_eq!(corrupt_segment.2.as_deref(), Some("CONTENT_AUTH_FAILED"));
        service.pool.close().await;

        let reopened = OutboxService::new(db::open(&config).await.expect("reopen"))
            .with_bundle_cipher(BundleContentCipher::new([42; 32]));
        let mut short = notification("short-after-corrupt", "short-after-corrupt-dedupe");
        short.created_at = base - 1_000;
        short.expires_at = base + 86_400_000;
        reopened
            .enqueue_device_at(device_id, short, base + 3)
            .await
            .expect("enqueue short notification");
        let channel = Arc::new(FakeChannel::default());
        let worker = OutboxWorker::new(reopened.clone(), channel.clone());
        assert!(
            worker
                .run_once(CancellationToken::new())
                .await
                .expect("short pass")
        );
        assert!(
            !worker
                .run_once(CancellationToken::new())
                .await
                .expect("retired bundle scheduler is idle")
        );
        assert_eq!(channel.sent_count(), 1);
        assert_eq!(
            reopened
                .status(device_id, "short-after-corrupt")
                .await
                .expect("short status")
                .status,
            "provider_accepted"
        );
        assert_eq!(
            reopened
                .bundle_status(device_id, "bundle-healthy")
                .await
                .expect("healthy bundle status")
                .status,
            NotificationBundleStatus::Queued
        );
        reopened.pool.close().await;
    }

    #[tokio::test]
    async fn bundle_restart_partial_failure_target_fence_and_cleanup_keep_tombstone() {
        let (_directory, config, service, device_id) = service().await;
        let body = "🙂中文/code\\path\n".repeat(1_000);
        let request = bundle("bundle-restart", "bundle-restart-dedupe", body);
        service
            .enqueue_bundle_at(device_id, target(), request.clone(), NOW)
            .await
            .expect("enqueue");
        let first = service
            .claim_next_bundle_segment_at(NOW)
            .await
            .expect("claim")
            .expect("first");
        service
            .finish_bundle_claim_at(
                &first,
                ChannelOutcome::Accepted {
                    provider_message_id: Some("provider-first".to_owned()),
                },
                NOW + 1,
            )
            .await
            .expect("finish first");
        service.pool.close().await;

        let reopened = OutboxService::new(db::open(&config).await.expect("reopen"))
            .with_bundle_cipher(BundleContentCipher::new([42; 32]));
        let second = reopened
            .claim_next_bundle_segment_at(NOW + 301)
            .await
            .expect("claim after restart")
            .expect("remaining segment");
        reopened
            .finish_bundle_claim_at(
                &second,
                ChannelOutcome::Accepted {
                    provider_message_id: Some("provider-second".to_owned()),
                },
                NOW + 302,
            )
            .await
            .expect("second accepted");
        let third = reopened
            .claim_next_bundle_segment_at(NOW + 602)
            .await
            .expect("third claim")
            .expect("third segment");
        reopened
            .finish_bundle_claim_at(&third, ChannelOutcome::PermanentFailure, NOW + 603)
            .await
            .expect("third partial failure");
        let failed = reopened
            .bundle_status(device_id, "bundle-restart")
            .await
            .expect("partial status");
        assert_eq!(failed.status, NotificationBundleStatus::PartialFailed);
        assert_eq!(failed.accepted_segments, 2);

        let unknown_request = bundle(
            "bundle-unknown",
            "bundle-unknown-dedupe",
            "ambiguous body".to_owned(),
        );
        reopened
            .enqueue_bundle_at(device_id, target(), unknown_request, NOW + 700)
            .await
            .expect("unknown bundle");
        let unknown_claim = reopened
            .claim_next_bundle_segment_at(NOW + 700)
            .await
            .expect("unknown claim")
            .expect("unknown segment");
        let stable_client_id = unknown_claim.client_id.clone();
        reopened
            .finish_bundle_claim_at(
                &unknown_claim,
                ChannelOutcome::Retryable {
                    class: RetryClass::AmbiguousMinusTwo,
                },
                NOW + 701,
            )
            .await
            .expect("unknown result");
        assert_eq!(
            reopened
                .bundle_status(device_id, "bundle-unknown")
                .await
                .expect("unknown status")
                .status,
            NotificationBundleStatus::DeliveryUnknown
        );
        assert!(
            reopened
                .claim_next_bundle_segment_at(NOW + 701 + NETWORK_RETRY_MS[0])
                .await
                .expect("unknown is terminal")
                .is_none()
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>(
                "SELECT segment.client_id
                 FROM notification_bundle_segments AS segment
                 JOIN notification_bundles AS bundle ON bundle.id = segment.bundle_row_id
                 WHERE bundle.bundle_id = 'bundle-unknown'",
            )
            .fetch_one(&reopened.pool)
            .await
            .expect("stable unknown client id"),
            stable_client_id
        );

        let target_request = bundle(
            "bundle-target",
            "bundle-target-dedupe",
            "target bound body".to_owned(),
        );
        reopened
            .enqueue_bundle_at(device_id, target(), target_request, NOW + 400)
            .await
            .expect("target bundle");
        let target_claim = reopened
            .claim_next_bundle_segment_at(NOW + 400)
            .await
            .expect("target claim")
            .expect("target segment");
        reopened
            .finish_bundle_claim_at(
                &target_claim,
                ChannelOutcome::BlockedTargetChanged,
                NOW + 401,
            )
            .await
            .expect("target fence");
        assert_eq!(
            reopened
                .bundle_status(device_id, "bundle-target")
                .await
                .expect("target status")
                .status,
            NotificationBundleStatus::BlockedTargetChanged
        );

        sqlx::query(
            "UPDATE notification_bundles
             SET body_retain_until = ?1 WHERE bundle_id = 'bundle-restart'",
        )
        .bind(NOW + 500)
        .execute(&reopened.pool)
        .await
        .expect("age body");
        reopened
            .purge_bundle_content_at(NOW + 500, 500)
            .await
            .expect("retention cleanup");
        let purged: (Option<Vec<u8>>, Option<Vec<u8>>, Option<i64>) = sqlx::query_as(
            "SELECT body_ciphertext, body_nonce, body_purged_at
             FROM notification_bundles WHERE bundle_id = 'bundle-restart'",
        )
        .fetch_one(&reopened.pool)
        .await
        .expect("purged tombstone");
        assert!(purged.0.is_none() && purged.1.is_none() && purged.2.is_some());
        let replay = reopened
            .enqueue_bundle_at(device_id, target(), request, NOW + 501)
            .await
            .expect("replay after cleanup");
        assert_eq!(replay.status, NotificationBundleStatus::PartialFailed);
        reopened.pool.close().await;
    }

    #[test]
    fn validation_and_payload_hash_contract_is_stable() {
        let base = notification("notification-1", "dedupe-1");
        validate_notification(&base, NOW).expect("valid notification");
        assert_eq!(
            payload_hash(&base),
            "0ce5d34285a696728bca0b2b73bd4c018c6403d99442db839a5ba269a60bbb10"
        );

        let mut changed_identifier = base.clone();
        changed_identifier.notification_id = "notification-2".to_owned();
        changed_identifier.dedupe_key = "dedupe-2".to_owned();
        assert_eq!(payload_hash(&base), payload_hash(&changed_identifier));

        let mut changed_body = base.clone();
        changed_body.body.push('!');
        assert_ne!(payload_hash(&base), payload_hash(&changed_body));
        let mut no_correlation = base.clone();
        no_correlation.correlation_key = None;
        assert_ne!(payload_hash(&base), payload_hash(&no_correlation));

        let mut invalid_values = Vec::new();
        let mut invalid = base.clone();
        invalid.schema_version = 2;
        invalid_values.push(invalid);
        let mut invalid = base.clone();
        invalid.priority = 256;
        invalid_values.push(invalid);
        let mut invalid = base.clone();
        invalid.body.clear();
        invalid_values.push(invalid);
        let mut invalid = base.clone();
        invalid.expires_at = NOW;
        invalid_values.push(invalid);
        let mut invalid = base.clone();
        invalid.expires_at = NOW + MAX_TTL_MS + 1;
        invalid_values.push(invalid);
        let mut invalid = base.clone();
        invalid.notification_id = "界".repeat(43);
        invalid_values.push(invalid);
        for invalid in invalid_values {
            assert_eq!(
                validate_notification(&invalid, NOW),
                Err(OutboxError::Validation)
            );
        }
    }

    #[tokio::test]
    async fn idempotency_matrix_is_strict_and_device_scoped() {
        let (_directory, _config, service, first_device) = service().await;
        let second_device = insert_device(&service.pool, "SECOND").await;
        let base = notification("notification-1", "dedupe-1");
        let inserted = service
            .enqueue_device_at(first_device, base.clone(), NOW)
            .await
            .expect("insert");
        assert!(!inserted.existing);

        let replay = service
            .enqueue_device_at(first_device, base.clone(), NOW + 5_000)
            .await
            .expect("replay");
        assert!(replay.existing);
        assert_eq!(replay.accepted_at, NOW);

        let mut different_payload = base.clone();
        different_payload.body.push('!');
        assert_eq!(
            service
                .enqueue_device_at(first_device, different_payload, NOW + 1)
                .await
                .expect_err("payload conflict"),
            OutboxError::IdempotencyConflict
        );
        let mut different_dedupe = base.clone();
        different_dedupe.dedupe_key = "dedupe-other".to_owned();
        assert_eq!(
            service
                .enqueue_device_at(first_device, different_dedupe, NOW + 1)
                .await
                .expect_err("dedupe conflict"),
            OutboxError::IdempotencyConflict
        );
        let mut different_id = base.clone();
        different_id.notification_id = "notification-other".to_owned();
        assert_eq!(
            service
                .enqueue_device_at(first_device, different_id, NOW + 1)
                .await
                .expect_err("ID conflict"),
            OutboxError::IdempotencyConflict
        );

        service
            .enqueue_device_at(
                first_device,
                notification("notification-2", "dedupe-2"),
                NOW + 1,
            )
            .await
            .expect("second row");
        assert_eq!(
            service
                .enqueue_device_at(
                    first_device,
                    notification("notification-1", "dedupe-2"),
                    NOW + 2,
                )
                .await
                .expect_err("split collision"),
            OutboxError::IdempotencyConflict
        );

        let independent = service
            .enqueue_device_at(second_device, base, NOW + 1)
            .await
            .expect("second device");
        assert!(!independent.existing);
        service.pool.close().await;
    }

    #[tokio::test]
    async fn system_reply_replay_is_exact_and_namespaced_from_device_ingress() {
        let (_directory, _config, service, device_id) = service().await;
        let device = notification("shared-id", "shared-dedupe");
        let reply = interactive_reply("shared-id", "shared-dedupe");

        assert!(
            !service
                .enqueue_device_at(device_id, device.clone(), NOW)
                .await
                .expect("device insert")
                .existing
        );
        assert!(
            !service
                .enqueue_system_reply_at(reply.clone(), NOW)
                .await
                .expect("system insert")
                .existing
        );
        let replay = service
            .enqueue_system_reply_at(reply.clone(), NOW + 5_000)
            .await
            .expect("system replay");
        assert!(replay.existing);
        assert_eq!(replay.accepted_at, NOW);

        let rows = sqlx::query(
            "SELECT origin_kind, origin_key, origin_device_id, kind,
                    target_account_fingerprint
             FROM notification_outbox ORDER BY origin_kind",
        )
        .fetch_all(&service.pool)
        .await
        .expect("origin rows");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get::<String, _>("origin_kind"), "device");
        assert_eq!(
            rows[0].get::<String, _>("origin_key"),
            format!("device:{device_id}")
        );
        assert_eq!(
            rows[0].get::<Option<String>, _>("origin_device_id"),
            Some(device_id.to_string())
        );
        assert_eq!(
            rows[0].get::<Option<String>, _>("target_account_fingerprint"),
            None
        );
        assert_eq!(rows[1].get::<String, _>("origin_kind"), "system");
        assert_eq!(rows[1].get::<String, _>("origin_key"), "system:interactive");
        assert_eq!(rows[1].get::<Option<String>, _>("origin_device_id"), None);
        assert_eq!(rows[1].get::<String, _>("kind"), "interactive_reply");
        assert_eq!(
            rows[1].get::<String, _>("target_account_fingerprint"),
            format!("wx:{}", "a".repeat(64))
        );

        let mut retargeted = reply.clone();
        retargeted.target_account_fingerprint = format!("wx:{}", "b".repeat(64));
        assert_eq!(
            service
                .enqueue_system_reply_at(retargeted, NOW + 1)
                .await
                .expect_err("target conflict"),
            OutboxError::IdempotencyConflict
        );
        let mut conflict = reply;
        conflict.body.push('!');
        assert_eq!(
            service
                .enqueue_system_reply_at(conflict, NOW + 1)
                .await
                .expect_err("system conflict"),
            OutboxError::IdempotencyConflict
        );
        assert_eq!(
            service
                .status(device_id, "shared-id")
                .await
                .expect("device row")
                .status,
            "pending_channel"
        );
        service
            .enqueue_system_reply_at(interactive_reply("system-only", "system-only"), NOW)
            .await
            .expect("system-only row");
        assert!(matches!(
            service.status(device_id, "system-only").await,
            Err(OutboxError::NotFound)
        ));
        let device_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM devices")
            .fetch_one(&service.pool)
            .await
            .expect("no fake system device");
        assert_eq!(device_count, 1);
        service.pool.close().await;
    }

    #[tokio::test]
    async fn admin_test_is_closed_durable_and_has_no_device_or_target() {
        let (_directory, _config, service, _device_id) = service().await;
        let accepted = service
            .enqueue_admin_test_at(NOW)
            .await
            .expect("Admin test enqueue");
        assert!(!accepted.existing);
        assert_eq!(accepted.relay_status, "accepted");
        assert_eq!(accepted.accepted_at, NOW);

        let row = sqlx::query(
            "SELECT origin_kind, origin_key, origin_device_id, notification_id,
                    dedupe_key, kind, target_account_fingerprint, title, body,
                    correlation_key, priority, status, created_at, expires_at
             FROM notification_outbox WHERE notification_id = ?1",
        )
        .bind(&accepted.notification_id)
        .fetch_one(&service.pool)
        .await
        .expect("Admin outbox row");
        assert_eq!(row.get::<String, _>("origin_kind"), "admin");
        assert_eq!(row.get::<String, _>("origin_key"), "admin:singleton");
        assert_eq!(row.get::<Option<String>, _>("origin_device_id"), None);
        assert_eq!(
            row.get::<String, _>("notification_id"),
            row.get::<String, _>("dedupe_key")
        );
        assert_eq!(row.get::<String, _>("kind"), "test");
        assert_eq!(
            row.get::<Option<String>, _>("target_account_fingerprint"),
            None
        );
        assert_eq!(row.get::<String, _>("title"), "PromptDock Relay 测试");
        assert_eq!(row.get::<String, _>("body"), "✅ PromptDock Relay 测试通知");
        assert_eq!(row.get::<Option<String>, _>("correlation_key"), None);
        assert_eq!(row.get::<i64, _>("priority"), 100);
        assert_eq!(row.get::<String, _>("status"), "pending_channel");
        assert_eq!(row.get::<i64, _>("created_at"), NOW);
        assert_eq!(row.get::<i64, _>("expires_at"), NOW + 5 * 60 * 1_000);
        service.pool.close().await;
    }

    #[tokio::test]
    async fn typed_enqueue_rejects_cross_origin_kind_and_invalid_target_fingerprint() {
        let (_directory, _config, service, device_id) = service().await;
        let mut device_interactive = notification("device-interactive", "device-interactive");
        device_interactive.kind = "interactive_reply".to_owned();
        assert_eq!(
            service
                .enqueue_device_at(device_id, device_interactive, NOW)
                .await
                .expect_err("device interactive kind"),
            OutboxError::Validation
        );

        let mut invalid_target = interactive_reply("invalid-target", "invalid-target");
        invalid_target.target_account_fingerprint = "raw-user-id".to_owned();
        assert_eq!(
            service
                .enqueue_system_reply_at(invalid_target, NOW)
                .await
                .expect_err("invalid target"),
            OutboxError::Validation
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_outbox")
            .fetch_one(&service.pool)
            .await
            .expect("no invalid rows");
        assert_eq!(count, 0);
        service.pool.close().await;
    }

    #[tokio::test]
    async fn staged_system_reply_obeys_caller_transaction_and_explicit_wake_boundary() {
        let (_directory, _config, service, _device_id) = service().await;
        let reply = interactive_reply("staged", "staged");

        let mut rolled_back = service.pool.begin().await.expect("rollback transaction");
        let staged = service
            .stage_system_reply_in(&mut rolled_back, reply.clone(), NOW)
            .await
            .expect("stage rollback row");
        assert!(!staged.existing);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(5), service.wake.notified())
                .await
                .is_err(),
            "staging must not wake the worker"
        );
        rolled_back.rollback().await.expect("rollback staged row");
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_outbox")
            .fetch_one(&service.pool)
            .await
            .expect("rollback row count");
        assert_eq!(count, 0);

        let mut committed = service.pool.begin().await.expect("commit transaction");
        service
            .stage_system_reply_in(&mut committed, reply, NOW)
            .await
            .expect("stage committed row");
        committed.commit().await.expect("commit staged row");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(5), service.wake.notified())
                .await
                .is_err(),
            "commit does not implicitly wake outside the service wrapper"
        );
        service.notify_worker();
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            service.wake.notified(),
        )
        .await
        .expect("explicit wake");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(5), service.wake.notified())
                .await
                .is_err(),
            "one explicit wake must produce one permit"
        );
        service.pool.close().await;
    }

    #[tokio::test]
    async fn concurrent_exact_ingress_creates_one_durable_row() {
        let (_directory, _config, service, device_id) = service().await;
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..16 {
            let service = service.clone();
            tasks.spawn(async move {
                service
                    .enqueue_device_at(device_id, notification("notification-1", "dedupe-1"), NOW)
                    .await
            });
        }
        let mut inserted = 0;
        let mut replayed = 0;
        while let Some(result) = tasks.join_next().await {
            let accepted = result.expect("task").expect("accepted");
            if accepted.existing {
                replayed += 1;
            } else {
                inserted += 1;
            }
            assert_eq!(accepted.accepted_at, NOW);
        }
        assert_eq!((inserted, replayed), (1, 15));
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_outbox")
            .fetch_one(&service.pool)
            .await
            .expect("count");
        assert_eq!(count, 1);
        service.pool.close().await;
    }

    #[tokio::test]
    async fn concurrent_conflicting_ingress_has_one_winner_without_overwrite() {
        let (_directory, _config, service, device_id) = service().await;
        let first = notification("notification-1", "dedupe-1");
        let mut second = first.clone();
        second.body = "different body".to_owned();
        let left_service = service.clone();
        let right_service = service.clone();
        let (left, right) = tokio::join!(
            left_service.enqueue_device_at(device_id, first, NOW),
            right_service.enqueue_device_at(device_id, second, NOW)
        );
        let results = [left, right];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(OutboxError::IdempotencyConflict)))
                .count(),
            1
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_outbox")
            .fetch_one(&service.pool)
            .await
            .expect("count");
        assert_eq!(count, 1);
        service.pool.close().await;
    }

    #[tokio::test]
    async fn committed_ingress_survives_reopen_and_replays_original_acceptance() {
        let (_directory, config, service, device_id) = service().await;
        let accepted = service
            .enqueue_device_at(device_id, notification("notification-1", "dedupe-1"), NOW)
            .await
            .expect("commit");
        service.pool.close().await;

        let reopened = OutboxService::new(db::open(&config).await.expect("reopen"));
        let status = reopened
            .status(device_id, "notification-1")
            .await
            .expect("durable status");
        assert_eq!(status.status, "pending_channel");
        let replay = reopened
            .enqueue_device_at(
                device_id,
                notification("notification-1", "dedupe-1"),
                NOW + 10_000,
            )
            .await
            .expect("replay after reopen");
        assert!(replay.existing);
        assert_eq!(replay.accepted_at, accepted.accepted_at);
        reopened.pool.close().await;
    }

    #[tokio::test]
    async fn atomic_claim_orders_work_and_rejects_stale_completion() {
        let (_directory, _config, service, device_id) = service().await;
        let mut low = notification("low", "low");
        low.priority = 1;
        let mut high = notification("high", "high");
        high.priority = 255;
        service
            .enqueue_device_at(device_id, low, NOW)
            .await
            .expect("low");
        service
            .enqueue_device_at(device_id, high, NOW + 1)
            .await
            .expect("high");
        let first = service
            .claim_next_at(NOW + 2)
            .await
            .expect("claim")
            .expect("row");
        assert_eq!(first.notification_id, "high");
        assert_eq!(first.attempt_count, 1);

        assert_eq!(
            service
                .recover_stale_claims_at(NOW + 2 + STALE_CLAIM_MS)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            service
                .recover_stale_claims_at(NOW + 3 + STALE_CLAIM_MS)
                .await
                .unwrap(),
            1
        );
        let reclaimed = service
            .claim_next_at(NOW + 3 + STALE_CLAIM_MS)
            .await
            .expect("reclaim")
            .expect("row");
        assert_eq!(reclaimed.notification_id, "high");
        assert_eq!(reclaimed.attempt_count, 2);
        assert!(
            !service
                .finish_claim_at(
                    &first,
                    ChannelOutcome::Accepted {
                        provider_message_id: Some("stale".to_owned()),
                    },
                    NOW + 4 + STALE_CLAIM_MS,
                )
                .await
                .expect("stale completion")
        );
        assert!(
            service
                .finish_claim_at(
                    &reclaimed,
                    ChannelOutcome::Accepted {
                        provider_message_id: Some("provider-1".to_owned()),
                    },
                    NOW + 4 + STALE_CLAIM_MS,
                )
                .await
                .expect("completion")
        );
        let status = service.status(device_id, "high").await.expect("status");
        assert_eq!(status.status, "provider_accepted");
        assert_eq!(status.provider_message_id.as_deref(), Some("provider-1"));
        service.pool.close().await;
    }

    #[tokio::test]
    async fn concurrent_claimers_never_claim_the_same_row() {
        let (_directory, _config, service, device_id) = service().await;
        service
            .enqueue_device_at(device_id, notification("notification-1", "dedupe-1"), NOW)
            .await
            .expect("insert");
        let left_service = service.clone();
        let right_service = service.clone();
        let (left, right) = tokio::join!(
            left_service.claim_next_at(NOW + 1),
            right_service.claim_next_at(NOW + 1)
        );
        let claimed = [left.expect("left"), right.expect("right")]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        assert_eq!(claimed.len(), 1);
        service.pool.close().await;
    }

    #[test]
    fn retry_schedules_and_stable_client_ids_are_deterministic() {
        for (attempt, expected) in NETWORK_RETRY_MS.into_iter().enumerate() {
            let outcome = ChannelOutcome::Retryable {
                class: RetryClass::Network,
            };
            let transition = failure_transition(&outcome, attempt as i64 + 1, NOW);
            assert_eq!(transition.0, "retry_wait");
            assert_eq!(transition.1, Some(NOW + expected));
        }
        let exhausted = ChannelOutcome::Retryable {
            class: RetryClass::Network,
        };
        assert_eq!(failure_transition(&exhausted, 6, NOW).0, "dead_letter");
        for (attempt, expected) in RATE_LIMIT_RETRY_MS.into_iter().enumerate() {
            let outcome = ChannelOutcome::Retryable {
                class: RetryClass::RateLimited,
            };
            assert_eq!(
                failure_transition(&outcome, attempt as i64 + 1, NOW).1,
                Some(NOW + expected)
            );
        }
        let context_after_an_earlier_attempt = ChannelOutcome::Retryable {
            class: RetryClass::ContextChanged,
        };
        assert_eq!(
            failure_transition(&context_after_an_earlier_attempt, 2, NOW).0,
            "blocked_activation"
        );
        let id = stable_client_id("device:00000000-0000-4000-8000-000000000001", "dedupe");
        assert_eq!(
            id,
            stable_client_id("device:00000000-0000-4000-8000-000000000001", "dedupe")
        );
        assert_ne!(id, stable_client_id("system:interactive", "dedupe"));
        assert_ne!(
            id,
            stable_client_id("device:00000000-0000-4000-8000-000000000001", "dedupe-2")
        );
        assert!(id.starts_with("promptdock-relay-"));
    }

    #[tokio::test]
    async fn retry_beyond_ttl_expires_and_blocked_rows_can_be_unblocked() {
        let (_directory, _config, service, device_id) = service().await;
        let mut short = notification("short", "short");
        short.expires_at = NOW + 1_000;
        service
            .enqueue_device_at(device_id, short, NOW)
            .await
            .expect("short");
        let claim = service
            .claim_next_at(NOW)
            .await
            .expect("claim")
            .expect("short claim");
        service
            .finish_claim_at(
                &claim,
                ChannelOutcome::Retryable {
                    class: RetryClass::Network,
                },
                NOW,
            )
            .await
            .expect("finish");
        assert_eq!(
            service.status(device_id, "short").await.unwrap().status,
            "expired"
        );

        for (id, blocked) in [
            ("activation", "blocked_activation"),
            ("reconnect", "blocked_reconnect"),
        ] {
            service
                .enqueue_device_at(device_id, notification(id, id), NOW)
                .await
                .expect("blocked seed");
            sqlx::query(
                "UPDATE notification_outbox SET status = ?1
                 WHERE origin_kind = 'device' AND origin_device_id = ?2 AND notification_id = ?3",
            )
            .bind(blocked)
            .bind(device_id.to_string())
            .bind(id)
            .execute(&service.pool)
            .await
            .expect("set blocked");
        }
        assert_eq!(
            service
                .unblock_at("blocked_activation", NOW + 1)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            service
                .unblock_at("blocked_reconnect", NOW + 1)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            service
                .status(device_id, "activation")
                .await
                .unwrap()
                .status,
            "retry_wait"
        );
        assert_eq!(
            service.status(device_id, "reconnect").await.unwrap().status,
            "retry_wait"
        );
        service.pool.close().await;
    }

    #[tokio::test]
    async fn provider_acceptance_wins_when_send_finishes_after_ttl() {
        let (_directory, _config, service, device_id) = service().await;
        let mut short = notification("short", "short");
        short.expires_at = NOW + 1_000;
        service
            .enqueue_device_at(device_id, short, NOW)
            .await
            .expect("short");
        let claim = service
            .claim_next_at(NOW)
            .await
            .expect("claim")
            .expect("row");
        service
            .finish_claim_at(
                &claim,
                ChannelOutcome::Accepted {
                    provider_message_id: None,
                },
                NOW + 1_000,
            )
            .await
            .expect("accepted completion");
        let status = service.status(device_id, "short").await.expect("status");
        assert_eq!(status.status, "provider_accepted");
        assert!(status.provider_message_id.is_none());
        service.pool.close().await;
    }

    struct DatabaseWritingChannel {
        pool: SqlitePool,
    }

    #[async_trait]
    impl NotificationChannel for DatabaseWritingChannel {
        async fn send(
            &self,
            _message: &ChannelMessage,
            _cancellation: CancellationToken,
        ) -> ChannelOutcome {
            let mut connection = self.pool.acquire().await.expect("connection during send");
            sqlx::query("BEGIN IMMEDIATE")
                .execute(&mut *connection)
                .await
                .expect("write transaction is not held during channel send");
            sqlx::query("ROLLBACK")
                .execute(&mut *connection)
                .await
                .expect("rollback probe");
            ChannelOutcome::Accepted {
                provider_message_id: Some("probe".to_owned()),
            }
        }
    }

    #[tokio::test]
    async fn worker_sends_outside_transactions_and_fake_acceptance_is_persisted() {
        let (_directory, _config, service, device_id) = service().await;
        let mut current = notification("notification-1", "dedupe-1");
        let now = unix_timestamp_ms().expect("now");
        current.created_at = now - 1;
        current.expires_at = now + 60_000;
        service
            .enqueue_device(device_id, current)
            .await
            .expect("enqueue");
        let worker = OutboxWorker::new(
            service.clone(),
            Arc::new(DatabaseWritingChannel {
                pool: service.pool.clone(),
            }),
        );
        assert!(
            worker
                .run_once(CancellationToken::new())
                .await
                .expect("worker")
        );
        let status = service.status(device_id, "notification-1").await.unwrap();
        assert_eq!(status.status, "provider_accepted");
        assert_eq!(status.provider_message_id.as_deref(), Some("probe"));
        service.pool.close().await;
    }

    #[tokio::test]
    async fn worker_startup_recovers_after_a_real_sqlite_writer_lock() {
        let _directory = tempfile::tempdir().expect("temporary directory");
        let config = DatabaseConfig {
            path: _directory.path().join("startup-lock.db"),
            busy_timeout_ms: 0,
            ..DatabaseConfig::default()
        };
        let pool = db::open(&config).await.expect("database");
        let device_id = insert_device(&pool, "STARTUP LOCK").await;
        let service =
            OutboxService::new(pool).with_bundle_cipher(BundleContentCipher::new([42; 32]));
        let now = unix_timestamp_ms().expect("now");
        let mut current = notification("startup-lock", "startup-lock");
        current.created_at = now - 1;
        current.expires_at = now + 60_000;
        service
            .enqueue_device(device_id, current)
            .await
            .expect("enqueue");

        let mut lock = service.pool.acquire().await.expect("writer connection");
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *lock)
            .await
            .expect("writer lock");
        let channel = Arc::new(FakeChannel::default());
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(
            OutboxWorker::new(service.clone(), channel.clone()).run(cancellation.clone()),
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        sqlx::query("ROLLBACK")
            .execute(&mut *lock)
            .await
            .expect("release writer lock");

        let delivered = tokio::time::timeout(Duration::from_secs(2), async {
            while channel.sent_count() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        cancellation.cancel();
        task.await
            .expect("worker task")
            .expect("worker remains healthy");
        delivered.expect("worker retries recovery and delivers after lock release");
        assert_eq!(
            service
                .status(device_id, "startup-lock")
                .await
                .expect("status")
                .status,
            "provider_accepted"
        );
    }

    #[tokio::test]
    async fn worker_claims_and_sends_system_interactive_reply() {
        let (_directory, _config, service, _device_id) = service().await;
        let now = unix_timestamp_ms().expect("now");
        let mut reply = interactive_reply("system-worker", "system-worker");
        reply.created_at = now - 1;
        reply.expires_at = now + 60_000;
        service
            .enqueue_system_reply(reply)
            .await
            .expect("system enqueue");
        let channel = Arc::new(FakeChannel::default());
        let worker = OutboxWorker::new(service.clone(), channel.clone());
        assert!(
            worker
                .run_once(CancellationToken::new())
                .await
                .expect("system worker")
        );
        assert_eq!(channel.sent_count(), 1);
        let row = sqlx::query(
            "SELECT status, provider_message_id, target_account_fingerprint
             FROM notification_outbox
             WHERE origin_key = 'system:interactive' AND notification_id = 'system-worker'",
        )
        .fetch_one(&service.pool)
        .await
        .expect("sent system row");
        assert_eq!(row.get::<String, _>("status"), "provider_accepted");
        assert!(
            row.get::<String, _>("provider_message_id")
                .starts_with("fake:promptdock-relay-")
        );
        assert_eq!(
            row.get::<String, _>("target_account_fingerprint"),
            format!("wx:{}", "a".repeat(64))
        );
        service.pool.close().await;
    }

    #[tokio::test]
    async fn stale_recovery_and_retry_apply_to_system_rows() {
        let (_directory, _config, service, _device_id) = service().await;
        service
            .enqueue_system_reply_at(
                interactive_reply("system-lifecycle", "system-lifecycle"),
                NOW,
            )
            .await
            .expect("system enqueue");
        let first = service
            .claim_next_at(NOW + 1)
            .await
            .expect("first claim")
            .expect("system row");
        assert_eq!(first.origin_key, "system:interactive");
        assert_eq!(
            first.target_account_fingerprint,
            Some(format!("wx:{}", "a".repeat(64)))
        );
        assert_eq!(
            service
                .recover_stale_claims_at(NOW + 2 + STALE_CLAIM_MS)
                .await
                .expect("stale recovery"),
            1
        );
        let reclaimed = service
            .claim_next_at(NOW + 2 + STALE_CLAIM_MS)
            .await
            .expect("reclaim")
            .expect("recovered system row");
        assert_eq!(reclaimed.attempt_count, 2);
        assert!(
            service
                .finish_claim_at(
                    &reclaimed,
                    ChannelOutcome::Retryable {
                        class: RetryClass::Network,
                    },
                    NOW + 2 + STALE_CLAIM_MS,
                )
                .await
                .expect("schedule retry")
        );
        let status: String = sqlx::query_scalar(
            "SELECT status FROM notification_outbox
             WHERE origin_key = 'system:interactive' AND notification_id = 'system-lifecycle'",
        )
        .fetch_one(&service.pool)
        .await
        .expect("retry status");
        assert_eq!(status, "retry_wait");

        service.pool.close().await;
    }

    #[tokio::test]
    async fn exact_replay_never_sends_a_second_fake_message() {
        let (_directory, _config, service, device_id) = service().await;
        let now = unix_timestamp_ms().expect("now");
        let mut current = notification("notification-1", "dedupe-1");
        current.created_at = now - 1;
        current.expires_at = now + 60_000;
        service
            .enqueue_device(device_id, current.clone())
            .await
            .expect("enqueue");
        let channel = Arc::new(FakeChannel::default());
        let worker = OutboxWorker::new(service.clone(), channel.clone());
        assert!(
            worker
                .run_once(CancellationToken::new())
                .await
                .expect("first send")
        );
        assert!(
            service
                .enqueue_device(device_id, current)
                .await
                .expect("replay")
                .existing
        );
        assert!(
            !worker
                .run_once(CancellationToken::new())
                .await
                .expect("no second send")
        );
        assert_eq!(channel.sent_count(), 1);
        service.pool.close().await;
    }

    struct BlockingChannel {
        entered: Arc<Notify>,
    }

    struct CancelAndAcceptChannel;

    struct CancelledChannel;

    #[async_trait]
    impl NotificationChannel for CancelAndAcceptChannel {
        async fn send(
            &self,
            _message: &ChannelMessage,
            cancellation: CancellationToken,
        ) -> ChannelOutcome {
            cancellation.cancel();
            ChannelOutcome::Accepted {
                provider_message_id: Some("accepted-before-shutdown".to_owned()),
            }
        }
    }

    #[async_trait]
    impl NotificationChannel for CancelledChannel {
        async fn send(
            &self,
            _message: &ChannelMessage,
            _cancellation: CancellationToken,
        ) -> ChannelOutcome {
            ChannelOutcome::Cancelled
        }
    }

    #[tokio::test]
    async fn ready_provider_acceptance_wins_over_simultaneous_cancellation() {
        let (_directory, _config, service, device_id) = service().await;
        let now = unix_timestamp_ms().expect("now");
        let mut current = notification("notification-1", "dedupe-1");
        current.created_at = now - 1;
        current.expires_at = now + 60_000;
        service
            .enqueue_device(device_id, current)
            .await
            .expect("enqueue");
        let worker = OutboxWorker::new(service.clone(), Arc::new(CancelAndAcceptChannel));

        assert!(
            worker
                .run_once(CancellationToken::new())
                .await
                .expect("worker")
        );
        let status = service
            .status(device_id, "notification-1")
            .await
            .expect("status");
        assert_eq!(status.status, "provider_accepted");
        assert_eq!(
            status.provider_message_id.as_deref(),
            Some("accepted-before-shutdown")
        );
        service.pool.close().await;
    }

    #[async_trait]
    impl NotificationChannel for BlockingChannel {
        async fn send(
            &self,
            _message: &ChannelMessage,
            _cancellation: CancellationToken,
        ) -> ChannelOutcome {
            self.entered.notify_one();
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn cancellation_releases_an_inflight_claim_for_restart() {
        let (_directory, _config, service, device_id) = service().await;
        let now = unix_timestamp_ms().expect("now");
        let mut current = notification("notification-1", "dedupe-1");
        current.created_at = now - 1;
        current.expires_at = now + 60_000;
        service
            .enqueue_device(device_id, current)
            .await
            .expect("enqueue");
        let entered = Arc::new(Notify::new());
        let worker = OutboxWorker::new(
            service.clone(),
            Arc::new(BlockingChannel {
                entered: Arc::clone(&entered),
            }),
        );
        let cancellation = CancellationToken::new();
        let run_cancellation = cancellation.clone();
        let task = tokio::spawn(async move { worker.run_once(run_cancellation).await });
        entered.notified().await;
        assert_eq!(
            service
                .status(device_id, "notification-1")
                .await
                .expect("sending status")
                .status,
            "sending_channel"
        );
        cancellation.cancel();
        assert!(!task.await.expect("worker task").expect("worker result"));
        let status = service
            .status(device_id, "notification-1")
            .await
            .expect("released status");
        assert_eq!(status.status, "retry_wait");
        assert_eq!(status.attempt_count, 0);
        assert_eq!(status.last_error_code.as_deref(), Some("SEND_CANCELLED"));
        service.pool.close().await;
    }

    #[tokio::test]
    async fn automatic_progress_reports_complete_idle_pass_but_not_cancellation_or_error() {
        let (_directory, _config, service, device_id) = service().await;
        let idle_cancellation = CancellationToken::new();
        let idle_ticks = Arc::new(AtomicUsize::new(0));
        let callback_ticks = Arc::clone(&idle_ticks);
        let callback_cancellation = idle_cancellation.clone();
        OutboxWorker::new(service.clone(), Arc::new(FakeChannel::default()))
            .run_with_progress(idle_cancellation, move || {
                callback_ticks.fetch_add(1, Ordering::AcqRel);
                callback_cancellation.cancel();
            })
            .await
            .expect("idle worker");
        assert_eq!(idle_ticks.load(Ordering::Acquire), 1);

        let now = unix_timestamp_ms().expect("now");
        let mut current = notification("progress-cancelled", "progress-cancelled");
        current.created_at = now - 1;
        current.expires_at = now + 60_000;
        service
            .enqueue_device(device_id, current)
            .await
            .expect("enqueue");
        let entered = Arc::new(Notify::new());
        let cancellation = CancellationToken::new();
        let run_cancellation = cancellation.clone();
        let interrupted_ticks = Arc::new(AtomicUsize::new(0));
        let callback_ticks = Arc::clone(&interrupted_ticks);
        let worker = OutboxWorker::new(
            service.clone(),
            Arc::new(BlockingChannel {
                entered: Arc::clone(&entered),
            }),
        );
        let task = tokio::spawn(async move {
            worker
                .run_with_progress(run_cancellation, move || {
                    callback_ticks.fetch_add(1, Ordering::AcqRel);
                })
                .await
        });
        entered.notified().await;
        cancellation.cancel();
        task.await
            .expect("interrupted worker task")
            .expect("interrupted worker result");
        assert_eq!(interrupted_ticks.load(Ordering::Acquire), 0);

        service.pool.close().await;
        let failure_ticks = Arc::new(AtomicUsize::new(0));
        let callback_ticks = Arc::clone(&failure_ticks);
        let error = OutboxWorker::new(service, Arc::new(FakeChannel::default()))
            .run_with_progress(CancellationToken::new(), move || {
                callback_ticks.fetch_add(1, Ordering::AcqRel);
            })
            .await
            .expect_err("closed database must fail");
        assert_eq!(error, OutboxError::Database);
        assert_eq!(failure_ticks.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn channel_cancellation_releases_claim_without_consuming_an_attempt() {
        let (_directory, _config, service, device_id) = service().await;
        let now = unix_timestamp_ms().expect("now");
        let mut current = notification("notification-1", "dedupe-1");
        current.created_at = now - 1;
        current.expires_at = now + 60_000;
        service
            .enqueue_device(device_id, current)
            .await
            .expect("enqueue");
        let worker = OutboxWorker::new(service.clone(), Arc::new(CancelledChannel));

        assert!(
            !worker
                .run_once(CancellationToken::new())
                .await
                .expect("worker")
        );
        let status = service
            .status(device_id, "notification-1")
            .await
            .expect("status");
        assert_eq!(status.status, "retry_wait");
        assert_eq!(status.attempt_count, 0);
        assert_eq!(status.last_error_code.as_deref(), Some("SEND_CANCELLED"));
        service.pool.close().await;
    }

    #[tokio::test]
    async fn channel_status_aggregates_only_actionable_and_provider_rows() {
        let (_directory, _config, service, device_id) = service().await;
        for (index, status, accepted_at) in [
            ("pending", "pending_channel", None),
            ("retry", "retry_wait", None),
            ("activation", "blocked_activation", None),
            ("reconnect", "blocked_reconnect", None),
            ("accepted-old", "provider_accepted", Some(NOW - 10)),
            ("accepted-new", "provider_accepted", Some(NOW)),
            ("expired", "expired", None),
        ] {
            sqlx::query(
                "INSERT INTO notification_outbox (
                    id, origin_kind, origin_key, origin_device_id,
                    notification_id, dedupe_key, payload_hash,
                    kind, title, body, priority, status, not_before, expires_at,
                    provider_accepted_at, created_at, updated_at
                 ) VALUES (?1, 'device', 'device:' || ?2, ?2, ?1, ?1,
                           'hash', 'test', 'title', 'body',
                           1, ?3, ?4, ?5, ?6, ?4, ?4)",
            )
            .bind(index)
            .bind(device_id.to_string())
            .bind(status)
            .bind(NOW - 100)
            .bind(NOW + 100)
            .bind(accepted_at)
            .execute(&service.pool)
            .await
            .expect("status row");
        }

        assert_eq!(
            service.channel_status().await.expect("channel status"),
            ChannelOutboxStatus {
                pending_notifications: 2,
                blocked_notifications: 2,
                last_provider_accepted_at: Some(NOW),
            }
        );
        service.pool.close().await;
    }

    #[tokio::test]
    async fn terminal_delivery_redacts_confirmation_body_but_keeps_stable_tombstone_digest() {
        let (_directory, _config, service, _device_id) = service().await;
        let secret = "确认码：123456";
        let mut reply = interactive_reply("confirmation-redaction", "confirmation-redaction");
        reply.body = secret.to_owned();
        reply.sensitive_body = true;
        let replay = reply.clone();
        service
            .enqueue_system_reply_at(reply, NOW)
            .await
            .expect("enqueue confirmation");
        let initial_hash: String = sqlx::query_scalar(
            "SELECT payload_hash FROM notification_outbox WHERE notification_id = ?1",
        )
        .bind("confirmation-redaction")
        .fetch_one(&service.pool)
        .await
        .expect("initial digest");
        let claim = service
            .claim_next_at(NOW + 1)
            .await
            .expect("claim")
            .expect("pending confirmation");
        assert_eq!(claim.body, secret);
        assert!(
            service
                .finish_claim_at(
                    &claim,
                    ChannelOutcome::Accepted {
                        provider_message_id: None
                    },
                    NOW + 2,
                )
                .await
                .expect("finish")
        );
        let row = sqlx::query("SELECT body, payload_hash FROM notification_outbox WHERE id = ?1")
            .bind(&claim.row_id)
            .fetch_one(&service.pool)
            .await
            .expect("redacted row");
        assert_eq!(row.get::<String, _>("body"), "[REDACTED]");
        assert_eq!(row.get::<String, _>("payload_hash"), initial_hash);
        assert!(
            service
                .enqueue_system_reply_at(replay, NOW + 3)
                .await
                .expect("replay after cleanup")
                .existing
        );
        service.pool.close().await;
    }

    fn sqlite_sidecar(path: &Path, suffix: &str) -> PathBuf {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        PathBuf::from(sidecar)
    }

    fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    #[tokio::test]
    async fn sensitive_terminal_redaction_scans_database_wal_and_shm() {
        let (_directory, config, service, _device_id) = service().await;
        let secret = "F6-CONFIRMATION-SECRET-923841";
        let mut reply = interactive_reply("privacy-scan", "privacy-scan");
        reply.body = secret.to_owned();
        reply.sensitive_body = true;
        service
            .enqueue_system_reply_at(reply, NOW)
            .await
            .expect("enqueue confirmation");
        let claim = service
            .claim_next_at(NOW + 1)
            .await
            .expect("claim")
            .expect("pending confirmation");
        assert!(
            service
                .finish_claim_at(
                    &claim,
                    ChannelOutcome::Accepted {
                        provider_message_id: None
                    },
                    NOW + 2,
                )
                .await
                .expect("finish")
        );

        let checkpoint = db::checkpoint_wal(&service.pool)
            .await
            .expect("privacy checkpoint");
        assert_eq!(checkpoint.busy, 0, "test owns every database reader");
        for path in [
            config.path.clone(),
            sqlite_sidecar(&config.path, "-wal"),
            sqlite_sidecar(&config.path, "-shm"),
        ] {
            if tokio::fs::try_exists(&path)
                .await
                .expect("privacy artifact existence")
            {
                let bytes = tokio::fs::read(&path)
                    .await
                    .expect("privacy artifact bytes");
                assert!(
                    !contains_bytes(&bytes, secret.as_bytes()),
                    "sensitive confirmation body remained in {}",
                    path.display()
                );
            }
        }
        service.pool.close().await;
    }
}
