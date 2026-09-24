use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures_util::{SinkExt as _, StreamExt as _};
use sqlx::{Row as _, SqlitePool};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        Message as WsMessage, client::IntoClientRequest as _, http::HeaderValue,
        http::header::AUTHORIZATION,
    },
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use zeroize::Zeroizing;

use super::parser::parse_confirmation_code;
use super::*;
use crate::{
    api,
    auth::{DeviceAuthService, DeviceScope},
    config::{DatabaseConfig, ServerConfig},
    db,
    gateway::{
        GatewayRuntime, RemoteDesiredStateV5, RemoteRunConditionV5, RemoteRunDetailV5,
        RemoteRunFilterV5, RemoteRunOutcomeV5, RemoteRunPageV5, RemoteRunPhaseV5,
        RemoteRunSummaryV5,
        protocol::{
            GATEWAY_PROTOCOL_VERSION, GatewayCapabilityV5, GatewayClientFrameV5,
            GatewayServerFrameV5, HelloV5, RemoteResponseResultV5, RemoteResponseV5, RequestAckV5,
            decode_server_frame, encode_client_frame,
        },
    },
    outbox::{InteractiveReplyV1, OutboxService},
    shutdown::TaskSupervisor,
    state::AppState,
};

const NOW: i64 = 1_800_000_000_000;
const SENDER_FINGERPRINT: &str =
    "wx:0000000000000000000000000000000000000000000000000000000000000000";
const OTHER_FINGERPRINT: &str =
    "wx:1111111111111111111111111111111111111111111111111111111111111111";

async fn sink() -> (TempDir, SqlitePool, DurableInboundMessageSink) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let config = DatabaseConfig {
        path: directory.path().join("relay.db"),
        ..DatabaseConfig::default()
    };
    let pool = db::open(&config).await.expect("database");
    let sink = DurableInboundMessageSink::new(InboundCommandService::new(pool.clone()));
    (directory, pool, sink)
}

async fn runtime() -> (
    TempDir,
    SqlitePool,
    InboundCommandService,
    DurableInboundMessageSink,
    OutboxService,
    DeviceAuthService,
) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let config = DatabaseConfig {
        path: directory.path().join("relay.db"),
        ..DatabaseConfig::default()
    };
    let pool = db::open(&config).await.expect("database");
    let service = InboundCommandService::new(pool.clone());
    let sink = DurableInboundMessageSink::new(service.clone());
    let outbox = OutboxService::new(pool.clone());
    let devices = DeviceAuthService::new(pool.clone());
    (directory, pool, service, sink, outbox, devices)
}

fn message(message_key: &str, text: String) -> EphemeralInboundText {
    EphemeralInboundText {
        message_key: message_key.to_owned(),
        sender_fingerprint: SENDER_FINGERPRINT.to_owned(),
        text: Zeroizing::new(text),
        received_at: NOW,
    }
}

fn message_at(message_key: &str, text: &str, received_at: i64) -> EphemeralInboundText {
    let mut message = message(message_key, text.to_owned());
    message.received_at = received_at;
    message
}

fn reply_for_claim(
    claim: &super::model::ClaimedInboundCommand,
    notification_id: &str,
    now: i64,
) -> InteractiveReplyV1 {
    InteractiveReplyV1 {
        schema_version: 1,
        notification_id: notification_id.to_owned(),
        dedupe_key: format!("dedupe-{notification_id}"),
        priority: 200,
        target_account_fingerprint: claim.sender_fingerprint.clone(),
        title: "PromptDock".to_owned(),
        body: "safe closed reply".to_owned(),
        sensitive_body: false,
        correlation_key: Some(format!("correlation-{notification_id}")),
        created_at: now,
        expires_at: claim.expires_at,
    }
}

fn gateway(devices: &DeviceAuthService) -> GatewayRuntime {
    GatewayRuntime::new(devices.clone(), TaskSupervisor::new())
}

#[test]
fn parser_is_closed_deterministic_and_normalizes_full_width_spaces() {
    for input in ["帮助", " help ", "\u{3000}/help\u{3000}", "HELP"] {
        assert_eq!(parse_inbound_command(input).command, InboundCommandV5::Help);
    }
    for input in ["设备", "devices", "list_devices", "\u{3000}device\u{3000}"] {
        assert_eq!(
            parse_inbound_command(input).command,
            InboundCommandV5::ListDevices
        );
    }
    for input in ["帮我执行任意 SQL", "0 状态", "1000 详情", ""] {
        assert_eq!(
            parse_inbound_command(input).command,
            InboundCommandV5::Unknown
        );
    }
    assert_eq!(
        parse_inbound_command("任务").command,
        InboundCommandV5::ListRuns {
            filter: RunFilter::All
        }
    );
    assert_eq!(
        parse_inbound_command("最近").command,
        InboundCommandV5::ListRuns {
            filter: RunFilter::Recent
        }
    );
    assert_eq!(
        parse_inbound_command("失败").command,
        InboundCommandV5::ListRuns {
            filter: RunFilter::Failed
        }
    );
    for input in ["下一页", "next", "next_page", "/next"] {
        assert_eq!(
            parse_inbound_command(input).command,
            InboundCommandV5::NextPage
        );
    }
    assert_eq!(
        parse_inbound_command("1\u{3000}状态").command,
        InboundCommandV5::GetRunStatus { slot: 1 }
    );
    assert_eq!(
        parse_inbound_command("999 详情").command,
        InboundCommandV5::GetRunDetail { slot: 999 }
    );
    assert_eq!(
        parse_inbound_command("9 子运行").command,
        InboundCommandV5::GetRunTree { slot: 9 }
    );
    assert_eq!(
        parse_inbound_command("选择设备 2").command,
        InboundCommandV5::SelectDevice { slot: 2 }
    );
    assert_eq!(
        parse_inbound_command("选择工作区 1").command,
        InboundCommandV5::SelectWorkspace { slot: 1 }
    );
    assert_eq!(
        parse_inbound_command("选择配置 1").command,
        InboundCommandV5::SelectHarnessProfile { slot: 1 }
    );
    assert_eq!(
        parse_inbound_command("启动 1").command,
        InboundCommandV5::StartRun { slot: 1 }
    );
    assert_eq!(
        parse_inbound_command("确认 123456").command,
        InboundCommandV5::Unknown
    );
    assert_eq!(parse_confirmation_code("确认 123456"), Some("123456"));
    assert_eq!(
        parse_inbound_command("取消").command,
        InboundCommandV5::CancelConfirmation
    );
    assert_eq!(
        parse_inbound_command("停止 1").command,
        InboundCommandV5::CancelRun { slot: 1 }
    );
    assert_eq!(
        parse_inbound_command("确认 12345").command,
        InboundCommandV5::Unknown
    );
}

#[test]
fn oversized_parser_input_is_closed_without_partial_command_execution() {
    let oversized = format!("帮助{}", "界".repeat(MAX_INBOUND_TEXT_CHARS - 1));
    assert_eq!(oversized.chars().count(), MAX_INBOUND_TEXT_CHARS + 1);
    let parsed = parse_inbound_command(&oversized);
    assert_eq!(parsed.command, InboundCommandV5::Unknown);
    assert_eq!(parsed.error_code, Some("INPUT_TOO_LARGE"));

    let boundary = "界".repeat(MAX_INBOUND_TEXT_CHARS);
    let parsed = parse_inbound_command(&boundary);
    assert_eq!(parsed.command, InboundCommandV5::Unknown);
    assert_eq!(parsed.error_code, None);
}

#[tokio::test]
async fn fresh_accept_is_durable_with_safe_closed_command_fields() {
    let (_directory, pool, sink) = sink().await;
    let outcome = sink
        .accept(
            message("message-1", "帮助".to_owned()),
            &CancellationToken::new(),
        )
        .await
        .expect("fresh accept");
    assert_eq!(outcome, InboundAcceptOutcome::Fresh);

    let row = sqlx::query("SELECT * FROM inbound_commands WHERE message_key = 'message-1'")
        .fetch_one(&pool)
        .await
        .expect("durable command");
    assert_eq!(
        row.get::<String, _>("sender_fingerprint"),
        SENDER_FINGERPRINT
    );
    assert_eq!(row.get::<String, _>("command_kind"), "help");
    assert_eq!(row.get::<String, _>("command_json"), r#"{"action":"help"}"#);
    assert_eq!(row.get::<String, _>("status"), "received");
    assert_eq!(row.get::<i64, _>("expires_at"), NOW + 86_400_000);
    assert_eq!(row.get::<i64, _>("attempt_count"), 0);
    assert_eq!(row.get::<i64, _>("created_at"), NOW);
    assert_eq!(row.get::<i64, _>("updated_at"), NOW);
    assert_eq!(row.get::<Option<String>, _>("claim_token"), None);
    assert_eq!(row.get::<Option<i64>, _>("claimed_at"), None);
    assert_eq!(row.get::<Option<String>, _>("reply_notification_id"), None);
    assert_eq!(row.get::<Option<String>, _>("last_error_code"), None);
    pool.close().await;
}

#[tokio::test]
async fn exact_replay_preserves_the_original_row() {
    let (_directory, pool, sink) = sink().await;
    let cancellation = CancellationToken::new();
    assert_eq!(
        sink.accept(message("message-1", "设备".to_owned()), &cancellation)
            .await,
        Ok(InboundAcceptOutcome::Fresh)
    );
    let mut replay = message("message-1", "设备".to_owned());
    replay.received_at = NOW + 50_000;
    assert_eq!(
        sink.accept(replay, &cancellation).await,
        Ok(InboundAcceptOutcome::ExactReplay)
    );
    let row = sqlx::query(
        "SELECT COUNT(*) AS row_count, MIN(created_at) AS created_at,
                MIN(expires_at) AS expires_at FROM inbound_commands",
    )
    .fetch_one(&pool)
    .await
    .expect("one original row");
    assert_eq!(row.get::<i64, _>("row_count"), 1);
    assert_eq!(row.get::<i64, _>("created_at"), NOW);
    assert_eq!(row.get::<i64, _>("expires_at"), NOW + 86_400_000);
    pool.close().await;
}

#[tokio::test]
async fn reused_message_key_with_different_payload_or_sender_conflicts() {
    let (_directory, pool, sink) = sink().await;
    let cancellation = CancellationToken::new();
    sink.accept(message("message-1", "帮助".to_owned()), &cancellation)
        .await
        .expect("winner");

    assert_eq!(
        sink.accept(message("message-1", "设备".to_owned()), &cancellation)
            .await,
        Err(InboundMessageError::IdempotencyConflict)
    );
    let mut foreign = message("message-1", "帮助".to_owned());
    foreign.sender_fingerprint = OTHER_FINGERPRINT.to_owned();
    assert_eq!(
        sink.accept(foreign, &cancellation).await,
        Err(InboundMessageError::IdempotencyConflict)
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inbound_commands")
        .fetch_one(&pool)
        .await
        .expect("row count");
    assert_eq!(count, 1);
    pool.close().await;
}

#[tokio::test]
async fn cancellation_before_the_durability_boundary_writes_nothing() {
    let (_directory, pool, sink) = sink().await;
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        sink.accept(message("message-1", "帮助".to_owned()), &cancellation)
            .await,
        Err(InboundMessageError::Cancelled)
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inbound_commands")
        .fetch_one(&pool)
        .await
        .expect("row count");
    assert_eq!(count, 0);
    pool.close().await;
}

#[tokio::test]
async fn cancellation_interrupts_acceptance_waiting_for_the_sqlite_writer() {
    let (_directory, pool, sink) = sink().await;
    let mut blocker = pool.begin().await.expect("writer transaction");
    sqlx::query("UPDATE app_metadata SET value = value WHERE key = 'schema_identity'")
        .execute(&mut *blocker)
        .await
        .expect("hold sqlite writer");

    let cancellation = CancellationToken::new();
    let acceptance = sink.accept(message("blocked-message", "帮助".to_owned()), &cancellation);
    tokio::pin!(acceptance);
    tokio::select! {
        biased;
        result = &mut acceptance => panic!("acceptance unexpectedly crossed writer fence: {result:?}"),
        () = tokio::task::yield_now() => {}
    }
    cancellation.cancel();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), acceptance)
            .await
            .expect("cancellation must not wait for busy timeout"),
        Err(InboundMessageError::Cancelled)
    );

    blocker.rollback().await.expect("release sqlite writer");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inbound_commands")
        .fetch_one(&pool)
        .await
        .expect("row count");
    assert_eq!(count, 0);
    pool.close().await;
}

#[tokio::test]
async fn oversized_input_is_durable_only_as_closed_unknown_metadata() {
    let (_directory, pool, sink) = sink().await;
    let raw = format!(
        "top-secret-oversized-body-{}",
        "密".repeat(MAX_INBOUND_TEXT_CHARS)
    );
    let cancellation = CancellationToken::new();
    sink.accept(message("oversized", raw), &cancellation)
        .await
        .expect("oversized unknown");
    let row = sqlx::query(
        "SELECT command_kind, command_json, last_error_code FROM inbound_commands
         WHERE message_key = 'oversized'",
    )
    .fetch_one(&pool)
    .await
    .expect("unknown row");
    assert_eq!(row.get::<String, _>("command_kind"), "unknown");
    assert_eq!(
        row.get::<String, _>("command_json"),
        r#"{"action":"unknown"}"#
    );
    assert_eq!(row.get::<String, _>("last_error_code"), "INPUT_TOO_LARGE");
    assert_database_does_not_contain(&pool, "top-secret-oversized-body").await;

    let different_oversized = format!(
        "different-oversized-body-{}",
        "密".repeat(MAX_INBOUND_TEXT_CHARS)
    );
    assert_eq!(
        sink.accept(message("oversized", different_oversized), &cancellation)
            .await,
        Err(InboundMessageError::IdempotencyConflict)
    );
    pool.close().await;
}

#[tokio::test]
async fn confirmation_code_is_absent_from_durable_inbound_acceptance() {
    let (_directory, pool, sink) = sink().await;
    sink.accept(
        message("confirm-hash", "确认 123456".to_owned()),
        &CancellationToken::new(),
    )
    .await
    .expect("confirmation command");
    let command_json: String = sqlx::query_scalar(
        "SELECT command_json FROM inbound_commands WHERE message_key = 'confirm-hash'",
    )
    .fetch_one(&pool)
    .await
    .expect("command json");
    assert!(!command_json.contains("123456"));
    assert!(!command_json.contains("confirmationId"));
    pool.close().await;
}

#[tokio::test]
async fn raw_text_is_absent_from_database_debug_and_errors() {
    let (_directory, pool, sink) = sink().await;
    let secret = "UNIQUE-RAW-INBOUND-BODY-不要落盘";
    let debug = format!("{:?}", message("privacy", secret.to_owned()));
    assert!(!debug.contains(secret));
    assert!(debug.contains("[REDACTED]"));

    sink.accept(
        message("privacy", secret.to_owned()),
        &CancellationToken::new(),
    )
    .await
    .expect("privacy row");
    let error = sink
        .accept(
            message("privacy", "different secret".to_owned()),
            &CancellationToken::new(),
        )
        .await
        .expect_err("conflict");
    assert!(!format!("{error:?} {error}").contains(secret));
    assert_database_does_not_contain(&pool, secret).await;
    pool.close().await;
}

#[tokio::test]
async fn migration_exposes_the_frozen_table_columns_indexes_and_constraints() {
    let (_directory, pool, _sink) = sink().await;
    let columns = sqlx::query("PRAGMA table_info(inbound_commands)")
        .fetch_all(&pool)
        .await
        .expect("columns")
        .into_iter()
        .map(|row| row.get::<String, _>("name"))
        .collect::<BTreeSet<_>>();
    let expected = [
        "message_key",
        "sender_fingerprint",
        "command_kind",
        "command_json",
        "payload_hash",
        "status",
        "claim_token",
        "claimed_at",
        "reply_notification_id",
        "next_attempt_at",
        "expires_at",
        "attempt_count",
        "last_error_code",
        "created_at",
        "updated_at",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    assert_eq!(columns, expected);

    let indexes = sqlx::query(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'inbound_commands'",
    )
    .fetch_all(&pool)
    .await
    .expect("indexes")
    .into_iter()
    .map(|row| row.get::<String, _>("name"))
    .collect::<BTreeSet<_>>();
    assert!(indexes.contains("idx_inbound_commands_ready"));
    assert!(indexes.contains("idx_inbound_commands_claim"));

    let invalid_status = sqlx::query(
        r#"INSERT INTO inbound_commands (
            message_key, sender_fingerprint, command_kind, command_json, payload_hash,
            status, expires_at, created_at, updated_at
         ) VALUES ('invalid', ?1, 'help', '{"action":"help"}', ?2, 'not-a-status', 2, 1, 1)"#,
    )
    .bind(SENDER_FINGERPRINT)
    .bind("a".repeat(64))
    .execute(&pool)
    .await;
    assert!(invalid_status.is_err());
    let invalid_json = sqlx::query(
        "INSERT INTO inbound_commands (
            message_key, sender_fingerprint, command_kind, command_json, payload_hash,
            status, expires_at, created_at, updated_at
         ) VALUES ('invalid', ?1, 'help', 'not-json', ?2, 'received', 2, 1, 1)",
    )
    .bind(SENDER_FINGERPRINT)
    .bind("a".repeat(64))
    .execute(&pool)
    .await;
    assert!(invalid_json.is_err());

    let mismatched_kind = sqlx::query(
        r#"INSERT INTO inbound_commands (
            message_key, sender_fingerprint, command_kind, command_json, payload_hash,
            status, expires_at, created_at, updated_at
         ) VALUES ('mismatch', ?1, 'help', '{"action":"list_devices"}', ?2,
                   'received', 2, 1, 1)"#,
    )
    .bind(SENDER_FINGERPRINT)
    .bind("b".repeat(64))
    .execute(&pool)
    .await;
    assert!(mismatched_kind.is_err());

    let invalid_claim_state = sqlx::query(
        r#"INSERT INTO inbound_commands (
            message_key, sender_fingerprint, command_kind, command_json, payload_hash,
            status, claim_token, expires_at, created_at, updated_at
         ) VALUES ('invalid-claim', ?1, 'help', '{"action":"help"}', ?2,
                   'received', 'unexpected', 2, 1, 1)"#,
    )
    .bind(SENDER_FINGERPRINT)
    .bind("c".repeat(64))
    .execute(&pool)
    .await;
    assert!(invalid_claim_state.is_err());

    let invalid_reply_state = sqlx::query(
        r#"INSERT INTO inbound_commands (
            message_key, sender_fingerprint, command_kind, command_json, payload_hash,
            status, expires_at, created_at, updated_at
         ) VALUES ('invalid-reply', ?1, 'help', '{"action":"help"}', ?2,
                   'reply_queued', 2, 1, 1)"#,
    )
    .bind(SENDER_FINGERPRINT)
    .bind("d".repeat(64))
    .execute(&pool)
    .await;
    assert!(invalid_reply_state.is_err());
    pool.close().await;
}

#[tokio::test]
async fn concurrent_claim_has_one_fenced_winner_and_one_attempt() {
    let (_directory, pool, service, sink, _outbox, _devices) = runtime().await;
    sink.accept(
        message("concurrent-claim", "帮助".to_owned()),
        &CancellationToken::new(),
    )
    .await
    .expect("durable command");

    let left_service = service.clone();
    let right_service = service.clone();
    let left_cancellation = CancellationToken::new();
    let right_cancellation = CancellationToken::new();
    let (left, right) = tokio::join!(
        left_service.claim_next_at(NOW + 1, &left_cancellation),
        right_service.claim_next_at(NOW + 1, &right_cancellation),
    );
    let claims = [left.expect("left claim"), right.expect("right claim")];
    assert_eq!(claims.iter().filter(|claim| claim.is_some()).count(), 1);
    let row = sqlx::query(
        "SELECT status, attempt_count, claim_token, claimed_at
         FROM inbound_commands WHERE message_key = 'concurrent-claim'",
    )
    .fetch_one(&pool)
    .await
    .expect("claimed row");
    assert_eq!(row.get::<String, _>("status"), "dispatching");
    assert_eq!(row.get::<i64, _>("attempt_count"), 1);
    assert!(row.get::<Option<String>, _>("claim_token").is_some());
    assert_eq!(row.get::<Option<i64>, _>("claimed_at"), Some(NOW + 1));
    pool.close().await;
}

#[tokio::test]
async fn fencing_rolls_back_staged_reply_and_only_current_claim_can_commit() {
    let (_directory, pool, service, sink, outbox, _devices) = runtime().await;
    sink.accept(
        message("fenced-command", "帮助".to_owned()),
        &CancellationToken::new(),
    )
    .await
    .expect("durable command");
    let claim = service
        .claim_next_at(NOW + 1, &CancellationToken::new())
        .await
        .expect("claim query")
        .expect("claim");
    let stale = super::model::ClaimedInboundCommand {
        message_key: claim.message_key.clone(),
        claim_token: "stale-claim-token".to_owned(),
        sender_fingerprint: claim.sender_fingerprint.clone(),
        command_kind: claim.command_kind.clone(),
        command_json: claim.command_json.clone(),
        last_error_code: claim.last_error_code.clone(),
        expires_at: claim.expires_at,
    };
    let device_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO devices(id, name, token_hash, enabled, created_at)
         VALUES(?1, 'ATOMIC DEVICE', ?2, 1, ?3)",
    )
    .bind(device_id.to_string())
    .bind(vec![7_u8; 32])
    .bind(NOW)
    .execute(&pool)
    .await
    .expect("atomic selection device");
    let selection = || super::model::PendingJobPageSelection {
        selected_device: device_id,
        filter: RunFilter::Recent,
        next_cursor: Some("Opaque_Cursor-atomic-page-000000000".to_owned()),
        entries: vec![super::model::SelectionEntry {
            slot: 1,
            device_id,
            client_opaque_handle: Some("Opaque_Handle-atomic-job-0000000000".to_owned()),
            item_kind: super::model::SelectionItemKind::Run,
        }],
    };

    assert!(
        !service
            .queue_reply_with_job_page_at(
                &outbox,
                &stale,
                reply_for_claim(&stale, "stale-reply", NOW + 2),
                selection(),
                NOW + 2,
            )
            .await
            .expect("stale queue rolls back")
    );
    assert!(
        !service
            .release_claim_at(&stale, NOW + 2)
            .await
            .expect("stale release")
    );
    assert!(
        !service
            .dead_letter_at(&stale, "STALE", NOW + 2)
            .await
            .expect("stale dead letter")
    );
    let outbox_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_outbox")
        .fetch_one(&pool)
        .await
        .expect("rolled back outbox count");
    assert_eq!(outbox_count, 0);
    let selection_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM selection_contexts")
        .fetch_one(&pool)
        .await
        .expect("rolled back selection count");
    assert_eq!(selection_count, 0);

    let reply = reply_for_claim(&claim, "committed-reply", NOW + 3);
    assert!(
        service
            .queue_reply_with_job_page_at(&outbox, &claim, reply, selection(), NOW + 3)
            .await
            .expect("current claim commit")
    );
    let row = sqlx::query(
        "SELECT c.status, c.reply_notification_id, c.claim_token,
                o.origin_kind, o.target_account_fingerprint
         FROM inbound_commands c
         JOIN notification_outbox o ON o.notification_id = c.reply_notification_id
         WHERE c.message_key = 'fenced-command'",
    )
    .fetch_one(&pool)
    .await
    .expect("atomic command and reply");
    assert_eq!(row.get::<String, _>("status"), "reply_queued");
    assert_eq!(
        row.get::<String, _>("reply_notification_id"),
        "committed-reply"
    );
    assert_eq!(row.get::<Option<String>, _>("claim_token"), None);
    assert_eq!(row.get::<String, _>("origin_kind"), "system");
    assert_eq!(
        row.get::<String, _>("target_account_fingerprint"),
        SENDER_FINGERPRINT
    );
    let selection_row = sqlx::query(
        "SELECT c.selected_device, c.query_kind, c.next_cursor, e.slot,
                e.client_opaque_handle
         FROM selection_contexts c
         JOIN selection_entries e USING(sender_fingerprint)
         WHERE c.sender_fingerprint = ?1",
    )
    .bind(SENDER_FINGERPRINT)
    .fetch_one(&pool)
    .await
    .expect("selection committed with reply and terminal state");
    assert_eq!(
        selection_row.get::<String, _>("selected_device"),
        device_id.to_string()
    );
    assert_eq!(selection_row.get::<String, _>("query_kind"), "runs_recent");
    assert_eq!(selection_row.get::<i64, _>("slot"), 1);
    pool.close().await;
}

#[tokio::test]
async fn reply_target_and_expiry_guards_fail_before_staging_outbox() {
    let (_directory, pool, service, sink, outbox, _devices) = runtime().await;
    sink.accept(
        message("guarded-command", "帮助".to_owned()),
        &CancellationToken::new(),
    )
    .await
    .expect("durable command");
    let claim = service
        .claim_next_at(NOW + 1, &CancellationToken::new())
        .await
        .expect("claim query")
        .expect("claim");

    let mut wrong_target = reply_for_claim(&claim, "wrong-target", NOW + 2);
    wrong_target.target_account_fingerprint = OTHER_FINGERPRINT.to_owned();
    assert_eq!(
        service
            .queue_reply_at(&outbox, &claim, wrong_target, NOW + 2)
            .await,
        Err(InboundWorkerError::Outbox)
    );
    let mut overlong = reply_for_claim(&claim, "overlong", NOW + 2);
    overlong.expires_at = claim.expires_at + 1;
    assert_eq!(
        service
            .queue_reply_at(&outbox, &claim, overlong, NOW + 2)
            .await,
        Err(InboundWorkerError::Outbox)
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_outbox")
        .fetch_one(&pool)
        .await
        .expect("guarded outbox count");
    assert_eq!(count, 0);
    let status: String = sqlx::query_scalar(
        "SELECT status FROM inbound_commands WHERE message_key = 'guarded-command'",
    )
    .fetch_one(&pool)
    .await
    .expect("claim remains fenced");
    assert_eq!(status, "dispatching");
    pool.close().await;
}

#[tokio::test]
async fn stale_recovery_expires_due_rows_recovers_only_stale_claims() {
    let (_directory, pool, service, sink, _outbox, _devices) = runtime().await;
    let cancellation = CancellationToken::new();
    sink.accept(
        message_at(
            "expired-command",
            "帮助",
            NOW - super::repository::INBOUND_COMMAND_TTL_MS,
        ),
        &cancellation,
    )
    .await
    .expect("expired candidate");
    for key in ["fresh-claim", "stale-claim"] {
        sink.accept(message(key, "帮助".to_owned()), &cancellation)
            .await
            .expect("claim candidate");
    }
    let recovery_at = NOW + super::repository::STALE_COMMAND_CLAIM_MS + 10;
    sqlx::query(
        "UPDATE inbound_commands
         SET status = 'dispatching', claim_token = message_key,
             claimed_at = CASE message_key
                 WHEN 'stale-claim' THEN ?1 ELSE ?2 END,
             attempt_count = 1, updated_at = ?3
         WHERE message_key IN ('fresh-claim', 'stale-claim')",
    )
    .bind(NOW)
    .bind(recovery_at - super::repository::STALE_COMMAND_CLAIM_MS + 1)
    .bind(recovery_at)
    .execute(&pool)
    .await
    .expect("seed claim ages");

    assert_eq!(
        service
            .recover_stale_claims_at(recovery_at)
            .await
            .expect("recover"),
        2
    );
    let rows = sqlx::query(
        "SELECT message_key, status, claim_token, claimed_at, last_error_code
         FROM inbound_commands ORDER BY message_key",
    )
    .fetch_all(&pool)
    .await
    .expect("recovery rows");
    let state = rows
        .into_iter()
        .map(|row| {
            (
                row.get::<String, _>("message_key"),
                (
                    row.get::<String, _>("status"),
                    row.get::<Option<String>, _>("claim_token"),
                    row.get::<Option<i64>, _>("claimed_at"),
                    row.get::<Option<String>, _>("last_error_code"),
                ),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(state["expired-command"].0, "expired");
    assert_eq!(state["stale-claim"].0, "received");
    assert_eq!(state["stale-claim"].1, None);
    assert_eq!(state["stale-claim"].2, None);
    assert_eq!(
        state["stale-claim"].3.as_deref(),
        Some("STALE_CLAIM_RECOVERED")
    );
    assert_eq!(state["fresh-claim"].0, "dispatching");
    assert_eq!(state["fresh-claim"].1.as_deref(), Some("fresh-claim"));
    pool.close().await;
}

#[tokio::test]
async fn cancellation_before_run_touches_no_claim_and_post_claim_release_is_immediate() {
    let (_directory, pool, service, sink, outbox, devices) = runtime().await;
    sink.accept(
        message("cancelled-start", "帮助".to_owned()),
        &CancellationToken::new(),
    )
    .await
    .expect("durable command");
    sqlx::query(
        "UPDATE inbound_commands
         SET status = 'dispatching', claim_token = 'startup-stale', claimed_at = ?1,
             attempt_count = 1, updated_at = ?2
         WHERE message_key = 'cancelled-start'",
    )
    .bind(NOW)
    .bind(NOW + super::repository::STALE_COMMAND_CLAIM_MS + 1)
    .execute(&pool)
    .await
    .expect("stale startup claim");
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    InboundCommandWorker::new(service.clone(), outbox, devices.clone(), gateway(&devices))
        .run(cancellation)
        .await
        .expect("cancelled worker exits");
    let row = sqlx::query(
        "SELECT status, claim_token, attempt_count
         FROM inbound_commands WHERE message_key = 'cancelled-start'",
    )
    .fetch_one(&pool)
    .await
    .expect("untouched startup row");
    assert_eq!(row.get::<String, _>("status"), "dispatching");
    assert_eq!(
        row.get::<Option<String>, _>("claim_token").as_deref(),
        Some("startup-stale")
    );
    assert_eq!(row.get::<i64, _>("attempt_count"), 1);

    sqlx::query(
        "UPDATE inbound_commands
         SET status = 'received', claim_token = NULL, claimed_at = NULL,
             attempt_count = 0, updated_at = ?1
         WHERE message_key = 'cancelled-start'",
    )
    .bind(NOW + 1)
    .execute(&pool)
    .await
    .expect("prepare claim boundary");
    let boundary_cancellation = CancellationToken::new();
    let claim = service
        .claim_next_at(NOW + 2, &boundary_cancellation)
        .await
        .expect("claim boundary query")
        .expect("claim boundary row");
    boundary_cancellation.cancel();
    assert!(
        service
            .release_claim_at(&claim, NOW + 3)
            .await
            .expect("post-claim cancellation release")
    );
    let released = sqlx::query(
        "SELECT status, claim_token, claimed_at, attempt_count
         FROM inbound_commands WHERE message_key = 'cancelled-start'",
    )
    .fetch_one(&pool)
    .await
    .expect("released boundary row");
    assert_eq!(released.get::<String, _>("status"), "received");
    assert_eq!(released.get::<Option<String>, _>("claim_token"), None);
    assert_eq!(released.get::<Option<i64>, _>("claimed_at"), None);
    assert_eq!(released.get::<i64, _>("attempt_count"), 0);
    pool.close().await;
}

#[tokio::test]
async fn automatic_progress_reports_complete_idle_pass_but_not_cancellation_or_error() {
    let (_directory, pool, service, _sink, outbox, devices) = runtime().await;
    let cancellation = CancellationToken::new();
    let callback_cancellation = cancellation.clone();
    let ticks = Arc::new(AtomicUsize::new(0));
    let callback_ticks = Arc::clone(&ticks);
    InboundCommandWorker::new(
        service.clone(),
        outbox.clone(),
        devices.clone(),
        gateway(&devices),
    )
    .run_with_progress(cancellation, move || {
        callback_ticks.fetch_add(1, Ordering::AcqRel);
        callback_cancellation.cancel();
    })
    .await
    .expect("idle worker");
    assert_eq!(ticks.load(Ordering::Acquire), 1);

    let pre_cancelled = CancellationToken::new();
    pre_cancelled.cancel();
    let interrupted_ticks = Arc::new(AtomicUsize::new(0));
    let callback_ticks = Arc::clone(&interrupted_ticks);
    InboundCommandWorker::new(
        service.clone(),
        outbox.clone(),
        devices.clone(),
        gateway(&devices),
    )
    .run_with_progress(pre_cancelled, move || {
        callback_ticks.fetch_add(1, Ordering::AcqRel);
    })
    .await
    .expect("pre-cancelled worker");
    assert_eq!(interrupted_ticks.load(Ordering::Acquire), 0);

    pool.close().await;
    let failure_ticks = Arc::new(AtomicUsize::new(0));
    let callback_ticks = Arc::clone(&failure_ticks);
    InboundCommandWorker::new(service, outbox, devices.clone(), gateway(&devices))
        .run_with_progress(CancellationToken::new(), move || {
            callback_ticks.fetch_add(1, Ordering::AcqRel);
        })
        .await
        .expect_err("closed database must fail");
    assert_eq!(failure_ticks.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn fresh_accept_wakes_once_exact_replay_does_not_and_notify_is_bounded() {
    let (_directory, pool, service, sink, _outbox, _devices) = runtime().await;
    let cancellation = CancellationToken::new();
    assert_eq!(
        sink.accept(message("wake-1", "帮助".to_owned()), &cancellation)
            .await,
        Ok(InboundAcceptOutcome::Fresh)
    );
    assert_eq!(
        sink.accept(message("wake-2", "设备".to_owned()), &cancellation)
            .await,
        Ok(InboundAcceptOutcome::Fresh)
    );
    tokio::time::timeout(Duration::from_millis(100), service.wake().notified())
        .await
        .expect("fresh commit wake");
    assert!(
        tokio::time::timeout(Duration::from_millis(10), service.wake().notified())
            .await
            .is_err(),
        "Notify coalesces a burst into one bounded permit"
    );
    assert_eq!(
        sink.accept(message("wake-1", "帮助".to_owned()), &cancellation)
            .await,
        Ok(InboundAcceptOutcome::ExactReplay)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(10), service.wake().notified())
            .await
            .is_err(),
        "exact replay must not wake the worker"
    );
    pool.close().await;
}

#[tokio::test]
async fn worker_queues_closed_replies_once_with_safe_bounded_device_rendering() {
    let (_directory, pool, service, sink, outbox, devices) = runtime().await;
    let now = super::repository::unix_timestamp_ms().expect("current time");
    for index in 0..21_i64 {
        let name = if index == 0 {
            "OFFICE\n\u{202e}\u{2066}PC".to_owned()
        } else {
            format!("DEVICE-{index:02}")
        };
        sqlx::query(
            "INSERT INTO devices (id, name, token_hash, enabled, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(name)
        .bind(vec![index as u8; 32])
        .bind(if index == 1 { 0 } else { 1 })
        .bind(now + index)
        .execute(&pool)
        .await
        .expect("device fixture");
    }

    let secret = "RAW-UNKNOWN-COMMAND-MUST-NOT-SURVIVE";
    let commands = [
        ("worker-help", "帮助"),
        ("worker-devices", "设备"),
        ("worker-jobs", "任务"),
        ("worker-status", "7 状态"),
        ("worker-detail", "8 详情"),
        ("worker-unknown", secret),
    ];
    for (offset, (key, text)) in commands.iter().enumerate() {
        sink.accept(
            message_at(key, text, now + i64::try_from(offset).expect("offset")),
            &CancellationToken::new(),
        )
        .await
        .expect("closed command acceptance");
    }
    let worker =
        InboundCommandWorker::new(service.clone(), outbox, devices.clone(), gateway(&devices));
    for _ in &commands {
        assert!(
            worker
                .run_once(CancellationToken::new())
                .await
                .expect("worker reply")
        );
    }
    assert!(
        !worker
            .run_once(CancellationToken::new())
            .await
            .expect("drained worker")
    );

    let command_rows = sqlx::query(
        "SELECT message_key, status, reply_notification_id, claim_token
         FROM inbound_commands ORDER BY message_key",
    )
    .fetch_all(&pool)
    .await
    .expect("processed commands");
    assert_eq!(command_rows.len(), commands.len());
    for row in command_rows {
        assert_eq!(row.get::<String, _>("status"), "reply_queued");
        assert!(
            row.get::<Option<String>, _>("reply_notification_id")
                .is_some()
        );
        assert_eq!(row.get::<Option<String>, _>("claim_token"), None);
    }
    let replies = sqlx::query(
        "SELECT notification_id, origin_kind, kind, title, body,
                target_account_fingerprint
         FROM notification_outbox ORDER BY created_at, notification_id",
    )
    .fetch_all(&pool)
    .await
    .expect("durable replies");
    assert_eq!(replies.len(), commands.len());
    assert!(replies.iter().all(|row| {
        row.get::<String, _>("origin_kind") == "system"
            && row.get::<String, _>("kind") == "interactive_reply"
            && row.get::<String, _>("title") == "PromptDock"
            && row.get::<String, _>("target_account_fingerprint") == SENDER_FINGERPRINT
    }));
    let bodies = replies
        .iter()
        .map(|row| row.get::<String, _>("body"))
        .collect::<Vec<_>>();
    assert_eq!(
        bodies
            .iter()
            .filter(|body| body.contains("目标设备当前离线"))
            .count(),
        1
    );
    let device_body = bodies
        .iter()
        .find(|body| body.contains("当前没有在线且具备只读任务查询权限"))
        .expect("device reply");
    assert!(!device_body.contains(['\r', '\u{202e}', '\u{2066}'].as_slice()));
    assert!(device_body.chars().count() <= 6_000);
    assert!(bodies.iter().any(|body| body.contains("未识别该命令")));
    assert!(bodies.iter().any(|body| body.contains("PromptDock 命令")));
    assert!(bodies.iter().all(|body| !body.contains(secret)));

    assert_eq!(
        sink.accept(
            message_at("worker-help", "帮助", now + 10_000),
            &CancellationToken::new(),
        )
        .await,
        Ok(InboundAcceptOutcome::ExactReplay)
    );
    assert!(
        !worker
            .run_once(CancellationToken::new())
            .await
            .expect("exact replay is not redispatched")
    );
    let reply_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_outbox")
        .fetch_one(&pool)
        .await
        .expect("single reply per command");
    assert_eq!(reply_count, commands.len() as i64);
    assert_database_does_not_contain(&pool, secret).await;
    pool.close().await;
}

#[tokio::test]
async fn invalid_closed_json_is_dead_lettered_without_outbox_side_effect() {
    let (_directory, pool, service, _sink, outbox, devices) = runtime().await;
    let now = super::repository::unix_timestamp_ms().expect("current time");
    sqlx::query(
        r#"INSERT INTO inbound_commands (
            message_key, sender_fingerprint, command_kind, command_json, payload_hash,
            status, expires_at, created_at, updated_at
         ) VALUES ('invalid-semantic', ?1, 'get_run_status',
                   '{"action":"get_run_status"}', ?2, 'received', ?3, ?4, ?4)"#,
    )
    .bind(SENDER_FINGERPRINT)
    .bind("e".repeat(64))
    .bind(now + super::repository::INBOUND_COMMAND_TTL_MS)
    .bind(now)
    .execute(&pool)
    .await
    .expect("schema-valid but semantically invalid closed JSON");
    assert!(
        InboundCommandWorker::new(service, outbox, devices.clone(), gateway(&devices))
            .run_once(CancellationToken::new())
            .await
            .expect("dead-letter worker")
    );
    let row = sqlx::query(
        "SELECT status, claim_token, last_error_code
         FROM inbound_commands WHERE message_key = 'invalid-semantic'",
    )
    .fetch_one(&pool)
    .await
    .expect("dead-letter row");
    assert_eq!(row.get::<String, _>("status"), "dead_letter");
    assert_eq!(row.get::<Option<String>, _>("claim_token"), None);
    assert_eq!(
        row.get::<Option<String>, _>("last_error_code").as_deref(),
        Some("INVALID_CLOSED_COMMAND")
    );
    let outbox_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_outbox")
        .fetch_one(&pool)
        .await
        .expect("no invalid reply");
    assert_eq!(outbox_count, 0);
    pool.close().await;
}

#[tokio::test]
async fn selection_context_is_bounded_replaced_expired_and_restart_durable() {
    let (_directory, pool, service, _sink, _outbox, _devices) = runtime().await;
    let first_device = uuid::Uuid::new_v4();
    let second_device = uuid::Uuid::new_v4();
    for (index, device) in [first_device, second_device].into_iter().enumerate() {
        sqlx::query(
            "INSERT INTO devices(id, name, token_hash, enabled, created_at)
             VALUES(?1, ?2, ?3, 1, ?4)",
        )
        .bind(device.to_string())
        .bind(format!("DEVICE-{index}"))
        .bind(vec![u8::try_from(index).expect("index"); 32])
        .bind(NOW + i64::try_from(index).expect("index"))
        .execute(&pool)
        .await
        .expect("device");
    }
    let entries = vec![
        super::model::SelectionEntry {
            slot: 1,
            device_id: first_device,
            client_opaque_handle: Some("0123456789abcdef0123456789abcdef".to_owned()),
            item_kind: super::model::SelectionItemKind::Run,
        },
        super::model::SelectionEntry {
            slot: 2,
            device_id: first_device,
            client_opaque_handle: Some("11111111111111111111111111111111".to_owned()),
            item_kind: super::model::SelectionItemKind::Run,
        },
    ];
    assert_eq!(
        service
            .next_page_context_at(SENDER_FINGERPRINT, NOW)
            .await
            .expect("missing page context"),
        Err(super::model::SelectionLookupError::Missing)
    );
    service
        .replace_job_page_at(
            SENDER_FINGERPRINT,
            first_device,
            RunFilter::Failed,
            Some("Opaque_Cursor-selection-page-000000"),
            &entries,
            NOW,
        )
        .await
        .expect("page selection");
    let page_context = service
        .next_page_context_at(SENDER_FINGERPRINT, NOW + 1)
        .await
        .expect("page context query")
        .expect("page context");
    assert_eq!(page_context.selected_device, first_device);
    assert_eq!(page_context.filter, RunFilter::Failed);
    assert_eq!(
        page_context.next_cursor,
        "Opaque_Cursor-selection-page-000000"
    );
    service
        .replace_job_page_at(
            SENDER_FINGERPRINT,
            first_device,
            RunFilter::Failed,
            None,
            &entries,
            NOW + 2,
        )
        .await
        .expect("last page selection");
    assert_eq!(
        service
            .next_page_context_at(SENDER_FINGERPRINT, NOW + 3)
            .await
            .expect("last page context"),
        Err(super::model::SelectionLookupError::Missing)
    );
    service
        .replace_selection_at(SENDER_FINGERPRINT, Some(first_device), &entries, NOW)
        .await
        .expect("selection");

    let restarted = InboundCommandService::new(pool.clone());
    let resolved = restarted
        .resolve_job_slot_at(SENDER_FINGERPRINT, 2, NOW + 1)
        .await
        .expect("lookup")
        .expect("durable after restart");
    assert_eq!(resolved.device_id, first_device);
    assert_eq!(
        resolved.client_opaque_handle.as_deref(),
        Some("11111111111111111111111111111111")
    );

    let replacement = [super::model::SelectionEntry {
        slot: 1,
        device_id: second_device,
        client_opaque_handle: None,
        item_kind: super::model::SelectionItemKind::Device,
    }];
    restarted
        .replace_selection_at(SENDER_FINGERPRINT, None, &replacement, NOW + 2)
        .await
        .expect("replacement");
    assert_eq!(
        restarted
            .resolve_job_slot_at(SENDER_FINGERPRINT, 2, NOW + 3)
            .await
            .expect("old lookup"),
        Err(super::model::SelectionLookupError::Missing)
    );
    assert_eq!(
        restarted
            .select_device_slot_at(SENDER_FINGERPRINT, 1, NOW + 3)
            .await
            .expect("device lookup"),
        Ok(second_device)
    );

    assert_eq!(
        restarted
            .resolve_job_slot_at(
                SENDER_FINGERPRINT,
                1,
                NOW + 2 + super::repository::SELECTION_TTL_MS,
            )
            .await
            .expect("expired lookup"),
        Err(super::model::SelectionLookupError::Expired)
    );
    let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM selection_contexts")
        .fetch_one(&pool)
        .await
        .expect("selection cleanup");
    assert_eq!(remaining, 0);

    let workspace_entry = [super::model::SelectionEntry {
        slot: 1,
        device_id: first_device,
        client_opaque_handle: Some("workspace_0123456789abcdef0123456789abcdef".to_owned()),
        item_kind: super::model::SelectionItemKind::Workspace,
    }];
    restarted
        .replace_catalog_selection_at(
            SENDER_FINGERPRINT,
            first_device,
            "workspaces",
            &workspace_entry,
            NOW + 4,
        )
        .await
        .expect("catalog selection");
    assert_eq!(
        restarted
            .select_catalog_slot_at(
                SENDER_FINGERPRINT,
                1,
                super::model::SelectionItemKind::Workspace,
                NOW + 5,
            )
            .await
            .expect("catalog selection refresh"),
        Ok(workspace_entry[0].clone())
    );

    let too_many = (1..=super::repository::MAX_SELECTION_ENTRIES + 1)
        .map(|index| super::model::SelectionEntry {
            slot: u16::try_from(index).expect("slot"),
            device_id: first_device,
            client_opaque_handle: Some(format!("{index:032x}")),
            item_kind: super::model::SelectionItemKind::Run,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        restarted
            .replace_selection_at(SENDER_FINGERPRINT, Some(first_device), &too_many, NOW)
            .await,
        Err(InboundWorkerError::Database)
    );
    pool.close().await;
}

#[tokio::test]
async fn waiting_gateway_restart_queues_safe_reply_without_redispatch() {
    let (_directory, pool, service, sink, outbox, devices) = runtime().await;
    let now = super::repository::unix_timestamp_ms().expect("current time");
    sink.accept(
        message_at("restart-query", "任务", now),
        &CancellationToken::new(),
    )
    .await
    .expect("query accepted");
    let claim = service
        .claim_next_at(now + 1, &CancellationToken::new())
        .await
        .expect("claim")
        .expect("query claim");
    assert!(
        service
            .mark_waiting_gateway_at(&claim, now + 2)
            .await
            .expect("waiting route")
    );

    let restarted = InboundCommandService::new(pool.clone());
    assert_eq!(
        restarted
            .recover_stale_claims_at(now + 3)
            .await
            .expect("restart recovery"),
        1
    );
    let worker = InboundCommandWorker::new(restarted, outbox, devices.clone(), gateway(&devices));
    assert!(
        worker
            .run_once(CancellationToken::new())
            .await
            .expect("safe restart reply")
    );
    let row = sqlx::query(
        "SELECT status, attempt_count, last_error_code FROM inbound_commands
         WHERE message_key = 'restart-query'",
    )
    .fetch_one(&pool)
    .await
    .expect("route row");
    assert_eq!(row.get::<String, _>("status"), "reply_queued");
    assert_eq!(row.get::<i64, _>("attempt_count"), 2);
    assert_eq!(
        row.get::<String, _>("last_error_code"),
        "GATEWAY_INTERRUPTED_RESTART"
    );
    let body: String = sqlx::query_scalar(
        "SELECT body FROM notification_outbox WHERE notification_id LIKE 'interactive:%'",
    )
    .fetch_one(&pool)
    .await
    .expect("safe outbox reply");
    assert!(body.contains("未重复执行"));
    pool.close().await;
}

#[tokio::test]
async fn device_list_job_list_and_detail_cross_the_real_gateway_without_content_leakage() {
    let directory = tempfile::tempdir().expect("temporary console database");
    let pool = db::open(&DatabaseConfig {
        path: directory.path().join("relay.db"),
        ..DatabaseConfig::default()
    })
    .await
    .expect("database");
    let supervisor = TaskSupervisor::new();
    let state = AppState::new(pool.clone(), supervisor.clone());
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("console listener");
    let address = listener.local_addr().expect("listener address");
    let app = api::router(state.clone(), &ServerConfig::default());
    let server_shutdown = supervisor.cancellation_token();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(server_shutdown.cancelled_owned())
            .await;
    });
    let device = state
        .device_auth
        .create_device(
            "HOME-PC",
            &[DeviceScope::GatewayConnect, DeviceScope::RunQuery],
        )
        .await
        .expect("query device");
    let mut request = format!("ws://{address}/v5/gateway/ws")
        .into_client_request()
        .expect("websocket request");
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", device.token.expose())).expect("authorization"),
    );
    let (mut socket, _) = connect_async(request).await.expect("gateway connect");
    send_gateway_client(
        &mut socket,
        GatewayClientFrameV5::Hello(HelloV5 {
            protocol_version: GATEWAY_PROTOCOL_VERSION,
            client_version: "console-test/1".to_owned(),
            capabilities: vec![GatewayCapabilityV5::RemoteRunsReadV2],
        }),
    )
    .await;
    let GatewayServerFrameV5::Welcome(welcome) = receive_gateway_server(&mut socket).await else {
        panic!("expected gateway welcome")
    };

    let service = InboundCommandService::new(pool.clone());
    let sink = DurableInboundMessageSink::new(service.clone());
    let worker = std::sync::Arc::new(InboundCommandWorker::new(
        service,
        state.outbox.clone(),
        state.device_auth.clone(),
        state.gateway.clone(),
    ));
    let now = super::repository::unix_timestamp_ms().expect("current time");
    sink.accept(
        message_at("console-devices", "设备", now),
        &CancellationToken::new(),
    )
    .await
    .expect("devices inbound");
    assert!(
        worker
            .run_once(CancellationToken::new())
            .await
            .expect("device reply")
    );

    sink.accept(
        message_at("console-list", "任务", now + 1),
        &CancellationToken::new(),
    )
    .await
    .expect("list inbound");
    let list_worker = std::sync::Arc::clone(&worker);
    let list = tokio::spawn(async move { list_worker.run_once(CancellationToken::new()).await });
    let GatewayServerFrameV5::Request(list_request) = receive_gateway_server(&mut socket).await
    else {
        panic!("expected list request")
    };
    send_gateway_client(
        &mut socket,
        GatewayClientFrameV5::RequestAck(RequestAckV5 {
            request_id: list_request.request_id.clone(),
            generation: welcome.generation,
            accepted: true,
        }),
    )
    .await;
    let first_run_handle = "run_0123456789abcdef0123456789abcdef";
    let first_summary = RemoteRunSummaryV5 {
        run_handle: first_run_handle.to_owned(),
        title: "安全任务标题".to_owned(),
        runtime_label: "Codex".to_owned(),
        workspace_label: "PromptDock".to_owned(),
        desired_state: RemoteDesiredStateV5::Running,
        phase: RemoteRunPhaseV5::Active,
        outcome: RemoteRunOutcomeV5::None,
        conditions: vec![
            RemoteRunConditionV5::Accepted,
            RemoteRunConditionV5::Running,
        ],
        attention_count: 0,
        started_at: now,
        updated_at: now + 1,
        child_count: 0,
        active_child_count: 0,
    };
    send_gateway_client(
        &mut socket,
        GatewayClientFrameV5::Response(Box::new(RemoteResponseV5 {
            request_id: list_request.request_id,
            generation: welcome.generation,
            result: RemoteResponseResultV5::ListRuns(RemoteRunPageV5 {
                items: vec![first_summary],
                next_cursor: Some("cursor_0123456789abcdef0123456789abcdef".to_owned()),
            }),
        })),
    )
    .await;
    assert!(list.await.expect("list worker join").expect("list worker"));

    sink.accept(
        message_at("console-next", "下一页", now + 2),
        &CancellationToken::new(),
    )
    .await
    .expect("next page inbound");
    let next_worker = std::sync::Arc::clone(&worker);
    let next = tokio::spawn(async move { next_worker.run_once(CancellationToken::new()).await });
    let GatewayServerFrameV5::Request(next_request) = receive_gateway_server(&mut socket).await
    else {
        panic!("expected next page request")
    };
    assert!(matches!(
        &next_request.action,
        crate::gateway::protocol::RemoteRequestActionV5::ListRuns {
            filter: RemoteRunFilterV5::All,
            page_size: 10,
            cursor: Some(cursor)
        } if cursor == "cursor_0123456789abcdef0123456789abcdef"
    ));
    send_gateway_client(
        &mut socket,
        GatewayClientFrameV5::RequestAck(RequestAckV5 {
            request_id: next_request.request_id.clone(),
            generation: welcome.generation,
            accepted: true,
        }),
    )
    .await;
    let run_handle = "run_fedcba9876543210fedcba9876543210";
    let summary = RemoteRunSummaryV5 {
        run_handle: run_handle.to_owned(),
        title: "第二页安全标题".to_owned(),
        runtime_label: "Codex".to_owned(),
        workspace_label: "PromptDock".to_owned(),
        desired_state: RemoteDesiredStateV5::Running,
        phase: RemoteRunPhaseV5::WaitingInput,
        outcome: RemoteRunOutcomeV5::None,
        conditions: vec![
            RemoteRunConditionV5::Accepted,
            RemoteRunConditionV5::WaitingForInput,
        ],
        attention_count: 1,
        started_at: now,
        updated_at: now + 2,
        child_count: 1,
        active_child_count: 1,
    };
    send_gateway_client(
        &mut socket,
        GatewayClientFrameV5::Response(Box::new(RemoteResponseV5 {
            request_id: next_request.request_id,
            generation: welcome.generation,
            result: RemoteResponseResultV5::ListRuns(RemoteRunPageV5 {
                items: vec![summary.clone()],
                next_cursor: None,
            }),
        })),
    )
    .await;
    assert!(next.await.expect("next worker join").expect("next worker"));

    sink.accept(
        message_at("console-detail", "1 详情", now + 3),
        &CancellationToken::new(),
    )
    .await
    .expect("detail inbound");
    let detail_worker = std::sync::Arc::clone(&worker);
    let detail =
        tokio::spawn(async move { detail_worker.run_once(CancellationToken::new()).await });
    let GatewayServerFrameV5::Request(detail_request) = receive_gateway_server(&mut socket).await
    else {
        panic!("expected detail request")
    };
    send_gateway_client(
        &mut socket,
        GatewayClientFrameV5::RequestAck(RequestAckV5 {
            request_id: detail_request.request_id.clone(),
            generation: welcome.generation,
            accepted: true,
        }),
    )
    .await;
    send_gateway_client(
        &mut socket,
        GatewayClientFrameV5::Response(Box::new(RemoteResponseV5 {
            request_id: detail_request.request_id,
            generation: welcome.generation,
            result: RemoteResponseResultV5::GetRunDetail(RemoteRunDetailV5 { summary }),
        })),
    )
    .await;
    assert!(
        detail
            .await
            .expect("detail worker join")
            .expect("detail worker")
    );

    let bodies = sqlx::query_scalar::<_, String>(
        "SELECT body FROM notification_outbox ORDER BY created_at, notification_id",
    )
    .fetch_all(&pool)
    .await
    .expect("console replies");
    assert_eq!(bodies.len(), 4);
    assert!(bodies.iter().any(|body| body.contains("已自动选择")));
    assert!(bodies.iter().any(|body| body.contains("安全任务标题")));
    assert!(bodies.iter().any(|body| body.contains("第二页安全标题")));
    assert!(bodies.iter().any(|body| body.contains("为保护隐私")));
    assert!(bodies.iter().all(|body| !body.contains(run_handle)));
    assert!(bodies.iter().all(|body| !body.contains(first_run_handle)));
    for handle in [first_run_handle, run_handle] {
        let leaked: i64 = sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM inbound_commands
                WHERE command_json LIKE '%' || ?1 || '%'
                   OR COALESCE(last_error_code, '') LIKE '%' || ?1 || '%'
                UNION ALL
                SELECT 1 FROM notification_outbox
                WHERE title LIKE '%' || ?1 || '%' OR body LIKE '%' || ?1 || '%'
                   OR notification_id LIKE '%' || ?1 || '%'
                   OR dedupe_key LIKE '%' || ?1 || '%'
                   OR COALESCE(correlation_key, '') LIKE '%' || ?1 || '%'
            )",
        )
        .bind(handle)
        .fetch_one(&pool)
        .await
        .expect("long-lived privacy sentinel");
        assert_eq!(
            leaked, 0,
            "opaque handle escaped the short-lived selection tables"
        );
    }

    supervisor.begin_shutdown();
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("server shutdown deadline")
        .expect("server join");
    pool.close().await;
}

async fn send_gateway_client<S>(socket: &mut S, frame: GatewayClientFrameV5)
where
    S: futures_util::Sink<WsMessage> + Unpin,
    S::Error: std::fmt::Debug,
{
    let encoded = encode_client_frame(&frame).expect("encode client frame");
    socket
        .send(WsMessage::Text(encoded.into()))
        .await
        .expect("send client frame");
}

async fn receive_gateway_server<S>(socket: &mut S) -> GatewayServerFrameV5
where
    S: futures_util::Stream<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
{
    let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("gateway response deadline")
        .expect("gateway response")
        .expect("gateway frame");
    let WsMessage::Text(text) = message else {
        panic!("expected text gateway frame")
    };
    decode_server_frame(text.as_str()).expect("decode server frame")
}

async fn assert_database_does_not_contain(pool: &SqlitePool, secret: &str) {
    let schema: String = sqlx::query_scalar(
        "SELECT COALESCE(group_concat(sql, ' '), '') FROM sqlite_master WHERE sql IS NOT NULL",
    )
    .fetch_one(pool)
    .await
    .expect("schema text");
    assert!(!schema.contains(secret));

    let rows = sqlx::query(
        "SELECT message_key, sender_fingerprint, command_kind, command_json, payload_hash,
                status, COALESCE(claim_token, '') AS claim_token,
                COALESCE(reply_notification_id, '') AS reply_notification_id,
                COALESCE(last_error_code, '') AS last_error_code
         FROM inbound_commands",
    )
    .fetch_all(pool)
    .await
    .expect("stored text columns");
    for row in rows {
        for column in [
            "message_key",
            "sender_fingerprint",
            "command_kind",
            "command_json",
            "payload_hash",
            "status",
            "claim_token",
            "reply_notification_id",
            "last_error_code",
        ] {
            assert!(!row.get::<String, _>(column).contains(secret));
        }
    }

    let outbox_rows = sqlx::query(
        "SELECT id, origin_kind, origin_key, COALESCE(origin_device_id, '') AS origin_device_id,
                notification_id, dedupe_key, payload_hash, kind, title, body,
                COALESCE(correlation_key, '') AS correlation_key,
                COALESCE(target_account_fingerprint, '') AS target_account_fingerprint,
                status, COALESCE(claim_token, '') AS claim_token,
                COALESCE(last_error_code, '') AS last_error_code,
                COALESCE(provider_message_id, '') AS provider_message_id
         FROM notification_outbox",
    )
    .fetch_all(pool)
    .await
    .expect("outbox text columns");
    for row in outbox_rows {
        for column in [
            "id",
            "origin_kind",
            "origin_key",
            "origin_device_id",
            "notification_id",
            "dedupe_key",
            "payload_hash",
            "kind",
            "title",
            "body",
            "correlation_key",
            "target_account_fingerprint",
            "status",
            "claim_token",
            "last_error_code",
            "provider_message_id",
        ] {
            assert!(!row.get::<String, _>(column).contains(secret));
        }
    }
}

#[tokio::test]
#[ignore = "superseded by keyed context-bound confirmation verification"]
async fn confirmation_consume_is_one_time_and_fenced_to_device_and_action() {
    let (_directory, pool, service, _sink, _outbox, _devices) = runtime().await;
    let device_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO devices(id, name, token_hash, created_at) VALUES(?1, 'confirm-device', ?2, ?3)",
    )
    .bind(device_id.to_string())
    .bind(vec![1_u8; 32])
    .bind(NOW)
    .execute(&pool)
    .await
    .expect("device");
    let hash = "confirm_0123456789abcdef0123456789abcdef".to_owned();
    sqlx::query(
        "INSERT INTO control_confirmations(
             sender_fingerprint, confirmation_hash, device_id, action_kind,
             workspace_handle, profile_handle, preset_handle, run_handle,
             intent_id, expires_at
         ) VALUES(?1, ?2, ?3, 'cancel_run', NULL, NULL, NULL, ?4, ?5, ?6)",
    )
    .bind(SENDER_FINGERPRINT)
    .bind(&hash)
    .bind(device_id.to_string())
    .bind("job_abcdefghijklmnopqrstuvwxyz123456")
    .bind("intent_abcdefghijklmnopqrstuvwxyz123456")
    .bind(NOW + 120_000)
    .execute(&pool)
    .await
    .expect("confirmation");
    sqlx::query(
        "INSERT INTO control_confirmations(
             sender_fingerprint, confirmation_hash, device_id, action_kind,
             workspace_handle, profile_handle, preset_handle, run_handle,
             intent_id, expires_at
         ) VALUES(?1, ?2, ?3, 'cancel_run', NULL, NULL, NULL, ?4, ?5, ?6)",
    )
    .bind(OTHER_FINGERPRINT)
    .bind(&hash)
    .bind(device_id.to_string())
    .bind("job_11111111111111111111111111111111")
    .bind("intent_11111111111111111111111111111111")
    .bind(NOW + 120_000)
    .execute(&pool)
    .await
    .expect("same hash from another sender is isolated");

    assert_eq!(
        service
            .consume_confirmation_at(
                SENDER_FINGERPRINT,
                &hash,
                Uuid::new_v4(),
                super::model::ConfirmationActionKind::CancelRun,
                NOW,
            )
            .await,
        Err(super::repository::ConfirmationConsumeError::WrongScope)
    );
    let consumed = service
        .consume_confirmation_at(
            SENDER_FINGERPRINT,
            &hash,
            device_id,
            super::model::ConfirmationActionKind::CancelRun,
            NOW,
        )
        .await
        .expect("consume");
    assert_eq!(consumed.device_id, device_id);
    assert_eq!(
        service
            .consume_confirmation_at(
                SENDER_FINGERPRINT,
                &hash,
                device_id,
                super::model::ConfirmationActionKind::CancelRun,
                NOW,
            )
            .await,
        Err(super::repository::ConfirmationConsumeError::Replay)
    );
    pool.close().await;
}

#[tokio::test]
async fn control_dispatch_binding_and_outcome_survive_restart_with_one_intent() {
    let (_directory, pool, service, sink, _outbox, _devices) = runtime().await;
    let device_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO devices(id, name, token_hash, created_at) VALUES(?1, 'dispatch-device', ?2, ?3)",
    )
    .bind(device_id.to_string())
    .bind(vec![2_u8; 32])
    .bind(NOW)
    .execute(&pool)
    .await
    .expect("device");
    sink.accept(
        message_at("control-confirm", "确认 123456", NOW),
        &CancellationToken::new(),
    )
    .await
    .expect("confirmation command");
    let confirmation_id = "confirm_fedcba98765432100123456789abcdef".to_owned();
    sqlx::query(
        "INSERT INTO control_confirmations(
             confirmation_id, sender_fingerprint, confirmation_digest, confirmation_nonce,
             device_id, action_kind, runtime_handle, workspace_handle,
             harness_profile_handle, task_preset_handle, run_handle, intent_id, expires_at
         ) VALUES(?1, ?2, ?3, ?4, ?5, 'cancel_run', NULL, NULL, NULL, NULL, ?6, ?7, ?8)",
    )
    .bind(&confirmation_id)
    .bind(SENDER_FINGERPRINT)
    .bind("a".repeat(64))
    .bind("b".repeat(32))
    .bind(device_id.to_string())
    .bind("run_0123456789abcdef0123456789abcdef")
    .bind("intent_0123456789abcdef0123456789abcdef")
    .bind(NOW + super::repository::CONTROL_CONFIRMATION_TTL_MS)
    .execute(&pool)
    .await
    .expect("confirmation");

    let claim = service
        .claim_next_at(NOW + 1, &CancellationToken::new())
        .await
        .expect("claim")
        .expect("confirmation claim");
    let first = service
        .prepare_control_dispatch_at(&claim, &confirmation_id, NOW + 2)
        .await
        .expect("bind dispatch");
    assert_eq!(
        first.confirmation.intent_id,
        "intent_0123456789abcdef0123456789abcdef"
    );
    assert!(first.outcome.is_none());
    assert!(first.error_code.is_none());

    let restarted = InboundCommandService::new(pool.clone());
    assert_eq!(
        restarted
            .recover_stale_claims_at(NOW + 3)
            .await
            .expect("pending dispatch recovery"),
        1
    );
    let retry_claim = restarted
        .claim_next_at(NOW + 4, &CancellationToken::new())
        .await
        .expect("retry claim")
        .expect("retry confirmation claim");
    let pending = restarted
        .prepare_control_dispatch_at(&retry_claim, &confirmation_id, NOW + 5)
        .await
        .expect("resume pending dispatch");
    assert_eq!(pending.confirmation.intent_id, first.confirmation.intent_id);
    assert!(pending.outcome.is_none());

    let outcome = super::model::ControlDispatchOutcome {
        accepted: true,
        run_handle: "run_fedcba9876543210fedcba9876543210".to_owned(),
        status: "requested".to_owned(),
    };
    assert!(
        restarted
            .persist_control_outcome_at(&retry_claim, &confirmation_id, &outcome, NOW + 6)
            .await
            .expect("persist outcome")
    );
    let after_response_crash = InboundCommandService::new(pool.clone());
    assert_eq!(
        after_response_crash
            .recover_stale_claims_at(NOW + 7)
            .await
            .expect("outcome recovery"),
        1
    );
    let outcome_claim = after_response_crash
        .claim_next_at(NOW + 8, &CancellationToken::new())
        .await
        .expect("outcome claim")
        .expect("outcome confirmation claim");
    let recovered = after_response_crash
        .prepare_control_dispatch_at(&outcome_claim, &confirmation_id, NOW + 9)
        .await
        .expect("resume outcome");
    assert_eq!(
        recovered.confirmation.intent_id,
        first.confirmation.intent_id
    );
    assert_eq!(recovered.outcome, Some(outcome));
    pool.close().await;
}
