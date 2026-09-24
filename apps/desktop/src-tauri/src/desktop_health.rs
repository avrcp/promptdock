//! Explicit local health checks. No installation, network requests, or config writes.
use crate::{
    db::{now_ms, Db},
    desktop_runtime::DesktopRuntime,
    diagnostics::*,
    error::AppError,
};
use rusqlite::{params, OptionalExtension};
use std::path::Path;

pub(crate) fn test_status(db: &Db) -> Result<Option<TestProbe>, AppError> {
    db.with_connection(|conn| conn.query_row(
        "SELECT substr(dedupe_key,13),id,status,remote_status,last_error_code FROM notification_outbox WHERE event_kind='test' AND dedupe_key LIKE 'health-test:%' ORDER BY created_at DESC,id DESC LIMIT 1",[],
        |r|Ok(TestProbe{probe_id:r.get(0)?,outbox_id:r.get(1)?,status:r.get(2)?,remote_status:r.get(3)?,last_error_code:r.get(4)?})
    ).optional().map_err(AppError::from))
}

pub(crate) fn start_test(
    runtime: &DesktopRuntime,
    request_id: &str,
) -> Result<TestProbe, AppError> {
    let probe = enqueue_test(&runtime.db, &runtime.outbox, request_id, now_ms()?)?;
    let _ = runtime.delivery.wake()?;
    // This boundary has only created or replayed a durable outbox row. Relay
    // acceptance is recorded later by the delivery worker, never inferred here.
    runtime
        .diagnostics
        .push(Component::Storage, Stage::Append, SafeCode::Ok, 0, 1);
    Ok(probe)
}

fn enqueue_test(
    db: &Db,
    outbox: &crate::notification::outbox::OutboxRepository,
    request_id: &str,
    now: i64,
) -> Result<TestProbe, AppError> {
    let id = uuid::Uuid::parse_str(request_id)
        .map_err(|_| AppError::new("INVALID_TEST_REQUEST", "测试请求标识无效"))?;
    if id.get_version_num() != 4 {
        return Err(AppError::new(
            "INVALID_TEST_REQUEST",
            "测试请求标识必须为UUIDv4",
        ));
    }
    let key = format!("health-test:{id}");
    outbox.hold_controller().require_not_held(now)?;
    outbox.enqueue_control(
        crate::notification::outbox::NotificationEventKind::Test,
        &key,
        &crate::relay::state::relay_test_payload(),
        now,
    )?;
    db.with_connection(|conn|conn.query_row(
        "SELECT id,status,remote_status,last_error_code FROM notification_outbox WHERE dedupe_key=?",params![key],
        |r|Ok(TestProbe{probe_id:id.to_string(),outbox_id:r.get(0)?,status:r.get(1)?,remote_status:r.get(2)?,last_error_code:r.get(3)?})
    ).map_err(AppError::from))
}

pub(crate) fn snapshot(
    dir: &Path,
    runtime: &DesktopRuntime,
    hook: Result<crate::hook_health::HookHealthSnapshot, String>,
) -> Result<HealthSnapshot, String> {
    let started = std::time::Instant::now();
    let mut steps = Vec::new();
    let mut checks = Vec::new();
    let mut step = |id, label, status, code: SafeCode, component, detail: String, next_action| {
        let code_text = serde_json::to_value(code)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_else(|| "UNAVAILABLE".into());
        // Codes are closed enum values. UI DTO uses owned code so no leak-prone interning.
        steps.push(HealthStep {
            id,
            label,
            status,
            code: code_text,
            detail,
            next_action,
        });
        checks.push(
            CheckResult::new(
                component,
                Stage::Check,
                match status {
                    "pass" => CheckStatus::Pass,
                    "not_run" => CheckStatus::NotRun,
                    _ => CheckStatus::Fail,
                },
                code,
                0,
            )
            .expect("zero duration is bounded"),
        );
    };
    let build = option_env!("PROMPTDOCK_BUILD_COMMIT").unwrap_or("unknown");
    let manifest = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.join("RELEASE-MANIFEST.json")))
        .and_then(|p| crate::hook_health::bounded_read(&p).ok())
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    let manifest_commit = manifest
        .as_ref()
        .and_then(|v| v.get("client"))
        .and_then(|v| v.get("commit"))
        .and_then(|v| v.as_str());
    let identity = match manifest_commit {
        Some(v) if v == build => ("pass", SafeCode::Ok),
        Some(_) => ("attention", SafeCode::ManifestMismatch),
        None => ("not_run", SafeCode::ManifestUnavailable),
    };
    step(
        "identity",
        "运行身份",
        identity.0,
        identity.1,
        Component::Runtime,
        format!(
            "运行构建 {build}；包清单{}。固定数据根已由启动守卫确认。",
            if manifest_commit.is_some() {
                "已读取"
            } else {
                "未取得（源码运行可没有包清单）"
            }
        ),
        "none",
    );
    let policy = crate::desktop_policy::read_authority(&dir.join("agent-events.jsonl"));
    let hold = runtime
        .delivery
        .user_hold_status(now_ms().map_err(|e| e.code.to_owned())?)
        .map_err(|e| e.code.to_owned())?;
    let (status, code, detail) = if dir.join(".reset-pending").exists() {
        (
            "attention",
            SafeCode::ResetPending,
            "本地运行数据清空未完成。请退出并重新执行包内清空入口。".into(),
        )
    } else if let Ok(policy) = policy {
        if runtime.settings.pending() {
            (
                "attention",
                SafeCode::PolicyPending,
                format!(
                    "策略修订 {} 已保存，等待安全应用；后台会重试。",
                    policy.revision
                ),
            )
        } else {
            match hold.state {
                crate::notification::hold::UserHoldState::Active => (
                    "attention",
                    SafeCode::HoldActive,
                    format!(
                        "策略修订 {} 已应用；用户暂缓至 {}，原文继续保存在本机。",
                        policy.revision,
                        hold.until.unwrap_or_default()
                    ),
                ),
                crate::notification::hold::UserHoldState::Uncertain => (
                    "attention",
                    SafeCode::HoldUncertain,
                    "暂缓控制文档或系统时间不可确认，请明确恢复推送。".into(),
                ),
                _ => (
                    "pass",
                    SafeCode::Ok,
                    format!("策略修订 {} 已应用；未暂缓推送。", policy.revision),
                ),
            }
        }
    } else {
        (
            "unavailable",
            SafeCode::Unavailable,
            "策略文件不可读取；不会用默认权限替代。".into(),
        )
    };
    step(
        "policy",
        "配置与发送控制",
        status,
        code,
        Component::Policy,
        detail,
        "open_notifications",
    );
    match hook {
        Ok(hook) => {
            let configured = hook.fresh && hook.installation == "current";
            let required = hook
                .handlers
                .iter()
                .filter(|h| h.required)
                .collect::<Vec<_>>();
            let trusted = required
                .iter()
                .all(|h| matches!(h.trust, crate::hook_health::TrustEvidence::MatchingRecord));
            let observed = required.iter().filter(|h| h.observation_current).count();
            let (status, code) = if !hook.observation_enabled {
                ("not_run", SafeCode::Disabled)
            } else if !configured {
                ("attention", SafeCode::HookMissing)
            } else if !trusted {
                ("attention", SafeCode::TrustUnconfirmed)
            } else {
                ("pass", SafeCode::Ok)
            };
            step("hook","Hook 配置、信任与接管",status,code,Component::Hook,format!("必需事件 {} 项，当前持久接管证据 {} 项。信任记录、组件自检和实际桌面验证分别判断；缺少新事件不代表损坏。",required.len(),observed),"open_integration");
        }
        Err(_) => step(
            "hook",
            "Hook 配置、信任与接管",
            "unavailable",
            SafeCode::Unavailable,
            Component::Hook,
            "本机 Hook 状态暂不可读取；未尝试安装或修复。".into(),
            "open_integration",
        ),
    }
    let outbox = runtime.outbox.stats().map_err(|e| e.code.to_owned())?;
    let (retention, checkpoint_health) = runtime
        .db
        .with_transaction(|tx| {
            Ok((
                crate::storage::retention::summary(tx)?,
                tx.query_row(
                    "SELECT
                COUNT(*),
                COALESCE(SUM(CASE WHEN status = 'active' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN status = 'degraded' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN status = 'unsupported' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN status = 'configured' THEN 1 ELSE 0 END), 0)
             FROM source_checkpoints",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                        ))
                    },
                )?,
            ))
        })
        .map_err(|e| e.code.to_owned())?;
    let inbox_bytes = match std::fs::metadata(dir.join("agent-events.jsonl")) {
        Ok(m) => Some(m.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(0),
        Err(_) => None,
    };
    let (storage_status, storage_code) = storage_health(
        inbox_bytes.is_some(),
        outbox.blocked_notifications,
        checkpoint_health,
    );
    step("storage","接收、处理与本地待发",storage_status,storage_code,Component::Storage,
        format!("接收文件 {} 字节；未消费字节未知（共享 source checkpoint 在轮转后不能安全合并为全局偏移）。处理器 checkpoint：总计 {}，活跃 {}、降级 {}、不支持 {}、待配置 {}。待发 {}、重试 {}、阻塞 {}。",inbox_bytes.map(|n|n.to_string()).unwrap_or_else(||"未知".into()),checkpoint_health.0,checkpoint_health.1,checkpoint_health.2,checkpoint_health.3,checkpoint_health.4,outbox.pending_count,outbox.retry_count,outbox.blocked_notifications),"view_deliveries");
    let relay = runtime.relay.status();
    let (status, code) = if !relay.configured {
        ("attention", SafeCode::RelayUnconfigured)
    } else if !relay.reachable {
        ("attention", SafeCode::RelayUnreachable)
    } else if !relay.authenticated
        || relay.can_submit == Some(false)
        || relay.can_read_own == Some(false)
    {
        ("attention", SafeCode::RelayUnauthorized)
    } else if !relay.features.iter().any(|f| f == "result_pages_v1") {
        ("attention", SafeCode::ResultCapabilityMissing)
    } else if runtime.relay.notification_delivery_ready() {
        ("pass", SafeCode::Ok)
    } else {
        ("not_run", SafeCode::NotRun)
    };
    step("relay","最近服务器检查",status,code,Component::Relay,format!("只展示已有 probe；最近检查时间 {}。结果页可用、Relay 接管、微信服务接收和手机阅读是不同事实。",relay.last_checked_at.map(|n|n.to_string()).unwrap_or_else(||"尚未检查".into())),"probe_relay");
    step(
        "acceptance",
        "真实桌面与手机确认",
        "not_run",
        SafeCode::NotRun,
        Component::Capture,
        "需在桌面 Codex 明确提交验证轮次，并由人在手机核对；本机体检不执行任务、不发送微信。"
            .into(),
        "open_integration",
    );
    runtime.diagnostics.push(
        Component::Runtime,
        Stage::Check,
        SafeCode::Ok,
        started.elapsed().as_millis().min(u32::MAX as u128) as u32,
        steps.len() as u32,
    );
    let report = HealthReport::from_safe_input(HealthReportInput {
        app_build: SafeBuild::parse(build).ok_or("HEALTH_BUILD_INVALID")?,
        runtime_epoch: runtime.epoch,
        check_results: checks,
        queue_counts: QueueCounts {
            pending: outbox.pending_count,
            sending: outbox.sending_count,
            retry: outbox.retry_count,
            blocked: outbox.blocked_notifications,
            dead_letter: outbox.dead_letter_count,
        },
        retention_pressure: RetentionPressure {
            retained_records: (retention.event_count
                + retention.output_count
                + retention.outbox_count) as u32,
            record_limit: retention.max_records as u32,
            retained_content_bytes: retention.content_bytes as u64,
            content_byte_limit: retention.max_content_bytes as u64,
        },
        // Hook configuration/trust was checked above. Capture is the separate
        // real desktop acceptance step, which this local report never runs.
        explicit_not_run: vec![Component::Capture],
        ring_snapshot: runtime.diagnostics.snapshot(),
    })
    .ok_or("HEALTH_REPORT_INVALID")?;
    Ok(HealthSnapshot {
        steps,
        report,
        test: test_status(&runtime.db).map_err(|e| e.code.to_owned())?,
    })
}

fn storage_health(
    readable: bool,
    blocked: u32,
    checkpoints: (i64, i64, i64, i64, i64),
) -> (&'static str, SafeCode) {
    if !readable {
        ("unavailable", SafeCode::Unavailable)
    } else if blocked > 0 {
        ("attention", SafeCode::QueueBlocked)
    } else if checkpoints.2 > 0 || checkpoints.3 > 0 {
        ("attention", SafeCode::Unavailable)
    } else if checkpoints.0 == 0 || checkpoints.1 == 0 {
        ("not_run", SafeCode::NotRun)
    } else {
        ("pass", SafeCode::Ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn processor_health_never_claims_unobserved_or_unsupported_checkpoints_pass() {
        for (counts, status) in [
            ((0, 0, 0, 0, 0), "not_run"),
            ((1, 0, 0, 0, 1), "not_run"),
            ((1, 0, 0, 1, 0), "attention"),
            ((1, 0, 1, 0, 0), "attention"),
            ((1, 1, 0, 0, 0), "pass"),
        ] {
            assert_eq!(storage_health(true, 0, counts).0, status);
        }
        assert_eq!(storage_health(false, 0, (1, 1, 0, 0, 0)).0, "unavailable");
        assert_eq!(storage_health(true, 1, (1, 1, 0, 0, 0)).0, "attention");
    }
    #[test]
    fn test_probe_replay_and_reopen_only_return_the_same_durable_delivery() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let outbox = crate::notification::outbox::OutboxRepository::new(db.clone());
        let id = uuid::Uuid::new_v4().to_string();
        let first = enqueue_test(&db, &outbox, &id, 1_000).unwrap();
        let again = enqueue_test(&db, &outbox, &id, 2_000).unwrap();
        assert_eq!(first, again);
        assert_eq!(first.status, "pending");
        assert_eq!(first.remote_status, None);
        assert_eq!(first.last_error_code, None);
        assert_eq!(test_status(&db).unwrap(), Some(first.clone()));
        db.with_connection(|conn| {
            assert_eq!(
                conn.query_row("SELECT COUNT(*) FROM notification_outbox", [], |r| r
                    .get::<_, i64>(0))?,
                1
            );
            assert_eq!(
                conn.query_row("SELECT attempt_count FROM notification_outbox", [], |r| r
                    .get::<_, i64>(
                    0
                ))?,
                0
            );
            Ok(())
        })
        .unwrap();
        // Re-reading and exporting status never needs the protected payload.
        db.with_connection(|conn| {
            conn.execute(
                "UPDATE notification_outbox SET payload_json='broken-protected-body'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        assert_eq!(test_status(&db).unwrap(), Some(first));
    }

    #[test]
    fn test_probe_replay_preserves_blocked_or_accepted_metadata_without_new_delivery() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let outbox = crate::notification::outbox::OutboxRepository::new(db.clone());

        let blocked_id = uuid::Uuid::new_v4().to_string();
        let blocked = enqueue_test(&db, &outbox, &blocked_id, 1_000).unwrap();
        db.with_connection(|conn|{conn.execute(
            "UPDATE notification_outbox SET status='blocked_activation', last_error_code='RELAY_UNREACHABLE' WHERE id=?",
            params![blocked.outbox_id],
        )?;Ok(())}).unwrap();
        let blocked_again = enqueue_test(&db, &outbox, &blocked_id, 2_000).unwrap();
        assert_eq!(blocked_again.status, "blocked_activation");
        assert_eq!(
            blocked_again.last_error_code.as_deref(),
            Some("RELAY_UNREACHABLE")
        );

        let accepted_id = uuid::Uuid::new_v4().to_string();
        let accepted = enqueue_test(&db, &outbox, &accepted_id, 3_000).unwrap();
        db.with_connection(|conn|{conn.execute(
            "UPDATE notification_outbox SET status='delivered', remote_status='provider_accepted' WHERE id=?",
            params![accepted.outbox_id],
        )?;Ok(())}).unwrap();
        let accepted_again = enqueue_test(&db, &outbox, &accepted_id, 4_000).unwrap();
        assert_eq!(accepted_again.status, "delivered");
        assert_eq!(
            accepted_again.remote_status.as_deref(),
            Some("provider_accepted")
        );
        db.with_connection(|conn| {
            assert_eq!(
                conn.query_row("SELECT COUNT(*) FROM notification_outbox", [], |r| r
                    .get::<_, i64>(0))?,
                2
            );
            assert_eq!(
                conn.query_row(
                    "SELECT SUM(attempt_count) FROM notification_outbox",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                0
            );
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn held_test_is_rejected_without_enqueuing_and_status_is_read_only() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let outbox = crate::notification::outbox::OutboxRepository::new(db.clone());
        outbox
            .hold_controller()
            .hold(0, crate::notification::hold::HoldDuration::Minutes15, 1000)
            .unwrap();
        let revision = db.revision();
        assert!(test_status(&db).unwrap().is_none());
        assert_eq!(revision, db.revision());
        assert_eq!(
            enqueue_test(&db, &outbox, &uuid::Uuid::new_v4().to_string(), 1001)
                .unwrap_err()
                .code,
            "NOTIFICATION_HOLD_ACTIVE"
        );
        assert!(test_status(&db).unwrap().is_none());
    }
}
