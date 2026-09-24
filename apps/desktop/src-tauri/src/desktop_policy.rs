//! A single durable policy commit; SQLite and Settings are recoverable projections.
use crate::{
    capture_policy::{
        try_read_policy_state, write_policy_state, CapturePolicy, CapturePolicyState,
    },
    db::{now_ms, Db},
    error::AppError,
    model::{NotificationMode, SettingsSnapshot},
    settings::SettingsStore,
};
use fs2::FileExt;
use serde::Serialize;
use std::{
    fs::OpenOptions,
    path::Path,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PolicySaveStatus {
    Saved,
    Unchanged,
    SavedPendingApply,
    Conflict,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PolicySaveReceipt {
    pub status: PolicySaveStatus,
    pub state: CapturePolicyState,
    pub error_code: Option<String>,
}

pub(crate) fn read_authority(inbox: &Path) -> Result<CapturePolicyState, AppError> {
    try_read_policy_state(inbox).map_err(|_| {
        AppError::new(
            "CAPTURE_POLICY_UNAVAILABLE",
            "采集策略缺失、损坏或版本不兼容，请检查 capture-policy.json；未恢复默认权限",
        )
    })
}

pub(crate) fn initialize(
    db: &Db,
    settings: &SettingsStore,
    inbox: &Path,
) -> Result<CapturePolicyState, AppError> {
    let path = inbox
        .parent()
        .ok_or_else(|| AppError::new("CAPTURE_POLICY_PATH", "采集目录无效"))?
        .join("capture-policy.json");
    match path.try_exists() {
        Ok(false) => {
            let untouched = db.with_transaction(|tx| Ok(tx.query_row(
                "SELECT policy_revision=0 AND content_epoch=0 AND capture_mode='status_only' AND task_input_enabled=0 FROM desktop_privacy WHERE singleton=1", [], |row| row.get::<_,bool>(0)
            )?))?;
            if !untouched {
                return Err(AppError::new(
                    "CAPTURE_POLICY_UNAVAILABLE",
                    "已保存的采集策略丢失；未恢复默认权限，请检查 capture-policy.json",
                ));
            }
            write_policy_state(inbox, &CapturePolicyState::default())?;
        }
        Ok(true) => {}
        Err(_) => {
            return Err(AppError::new(
                "CAPTURE_POLICY_UNAVAILABLE",
                "无法读取采集策略",
            ))
        }
    }
    let state = read_authority(inbox)?;
    settings.begin_apply()?;
    apply_projection(db, settings, &state)?;
    Ok(state)
}

pub(crate) fn commit(
    inbox: &Path,
    policy: &CapturePolicy,
    expected_revision: i64,
) -> Result<PolicySaveReceipt, AppError> {
    commit_with_publish(inbox, policy, expected_revision, write_policy_state)
}

fn commit_with_publish(
    inbox: &Path,
    policy: &CapturePolicy,
    expected_revision: i64,
    publish: impl FnOnce(&Path, &CapturePolicyState) -> Result<(), AppError>,
) -> Result<PolicySaveReceipt, AppError> {
    if !policy.validate() {
        return Err(AppError::new(
            "INVALID_CAPTURE_POLICY",
            "结束观察窗口必须在 500–20000 毫秒之间",
        ));
    }
    let parent = inbox
        .parent()
        .ok_or_else(|| AppError::new("CAPTURE_POLICY_PATH", "采集目录无效"))?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(parent.join("capture-policy.lock"))
        .map_err(|_| AppError::new("CAPTURE_POLICY_LOCK", "策略暂时无法锁定"))?;
    let deadline = Instant::now() + Duration::from_millis(400);
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => break,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(_) => {
                return Err(AppError::new(
                    "CAPTURE_POLICY_LOCK",
                    "策略正在由其他操作保存，请重试",
                ))
            }
        }
    }
    let previous = read_authority(inbox)?;
    if previous.revision != expected_revision {
        return Ok(PolicySaveReceipt {
            status: PolicySaveStatus::Conflict,
            state: previous,
            error_code: Some("CONFIG_REVISION_CONFLICT".into()),
        });
    }
    if previous.policy == *policy {
        return Ok(PolicySaveReceipt {
            status: PolicySaveStatus::Unchanged,
            state: previous,
            error_code: None,
        });
    }
    let revision = previous
        .revision
        .checked_add(1)
        .filter(|v| *v <= 9_007_199_254_740_991)
        .ok_or_else(|| AppError::new("POLICY_REVISION_EXHAUSTED", "策略修订号已耗尽"))?;
    let narrowed = policy.result_content_mode.rank() < previous.policy.result_content_mode.rank();
    let generation = if narrowed {
        previous
            .capture_generation
            .checked_add(1)
            .ok_or_else(|| AppError::new("CAPTURE_GENERATION_EXHAUSTED", "采集内容代次已耗尽"))?
    } else {
        previous.capture_generation
    };
    let state = CapturePolicyState {
        policy: policy.clone(),
        revision,
        capture_generation: generation,
        ..previous
    };
    // This successful atomic replace, and only this, means the user has saved.
    publish(inbox, &state)?;
    Ok(PolicySaveReceipt {
        status: PolicySaveStatus::Saved,
        state,
        error_code: None,
    })
}

pub(crate) fn apply_projection(
    db: &Db,
    settings: &SettingsStore,
    state: &CapturePolicyState,
) -> Result<(), AppError> {
    let mode = crate::desktop_runtime::content_mode_name(state.policy.result_content_mode);
    db.with_transaction(|tx| {
        let (revision, generation, old_mode, task_input): (i64,i64,String,bool) = tx.query_row(
            "SELECT policy_revision,content_epoch,capture_mode,task_input_enabled FROM desktop_privacy WHERE singleton=1", [],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        )?;
        if revision > state.revision || generation > state.capture_generation {
            return Err(AppError::new("POLICY_REVISION_ROLLBACK", "策略文件比已应用记录旧；已暂停发送，请检查策略文件"));
        }
        let content_narrowed = generation < state.capture_generation;
        let task_narrowed = task_input && !state.policy.include_task_input;
        // Apply selection revocations even while Relay is offline and no sender
        // exists. Re-enabling a class must never revive its older unsent items.
        tx.execute(
            "UPDATE notification_outbox SET status='cancelled',claimed_at=NULL,next_attempt_at=NULL,
                last_error_code='NOTIFICATION_POLICY_CHANGED',last_error_message='Notification disabled',updated_at=?1
             WHERE status IN ('pending','sending','retry_wait','blocked_activation','blocked_reconnect')
               AND event_kind IN ('run_started','run_completed','run_failed','run_interrupted','attention_required')
               AND (?2=0 OR (?3=0 AND event_kind='run_started')
                         OR (?4=0 AND event_kind IN ('run_completed','run_failed','run_interrupted'))
                         OR (?5=0 AND event_kind='attention_required'))",
            rusqlite::params![now_ms()?,state.policy.observe_turns,state.policy.notify_started,state.policy.notify_ended,state.policy.notify_attention],
        )?;
        if content_narrowed || task_narrowed {
            let now = now_ms()?;
            if content_narrowed {
                tx.execute("DELETE FROM agent_outputs", [])?;
                tx.execute("UPDATE notification_outbox SET status='cancelled',claimed_at=NULL,next_attempt_at=NULL,last_error_code='NOTIFICATION_POLICY_CHANGED',last_error_message='Result content permission narrowed',updated_at=? WHERE event_kind IN ('run_completed','run_failed','run_interrupted') AND status IN ('pending','sending','retry_wait','blocked_activation','blocked_reconnect')", [now])?;
            }
            if task_narrowed {
                tx.execute("UPDATE notification_outbox SET status='cancelled',claimed_at=NULL,next_attempt_at=NULL,last_error_code='NOTIFICATION_POLICY_CHANGED',last_error_message='Task input permission narrowed',updated_at=? WHERE event_kind='run_started' AND status IN ('pending','sending','retry_wait','blocked_activation','blocked_reconnect')", [now])?;
            }
        }
        if revision != state.revision || generation != state.capture_generation || old_mode != mode || task_input != state.policy.include_task_input {
            tx.execute("UPDATE desktop_privacy SET policy_revision=?,content_epoch=?,capture_mode=?,task_input_enabled=? WHERE singleton=1",
                rusqlite::params![state.revision,state.capture_generation,mode,state.policy.include_task_input])?;
        }
        Ok(())
    })?;
    settings.replace(snapshot(state))?;
    Ok(())
}

pub(crate) fn snapshot(state: &CapturePolicyState) -> SettingsSnapshot {
    let policy = &state.policy;
    let mut snapshot = SettingsSnapshot::default();
    snapshot.capture.result_content_mode = policy.result_content_mode;
    snapshot.notifications.enabled = policy.observe_turns
        && (policy.notify_started || policy.notify_ended || policy.notify_attention);
    snapshot.notifications.mode = if policy.notify_started {
        NotificationMode::StartAndCompletion
    } else {
        NotificationMode::CompletionOnly
    };
    snapshot.notifications.notify_ended = policy.notify_ended;
    snapshot.notifications.content_epoch = state.capture_generation;
    snapshot.notifications.policy_revision = state.revision;
    snapshot.notifications.include_task_input = policy.include_task_input;
    snapshot.notifications.completion_quiet_ms = policy.completion_quiet_ms;
    snapshot.notifications.attention_enabled = policy.notify_attention;
    snapshot.notifications.result_content_mode = policy.result_content_mode;
    snapshot
}

pub(crate) fn require_applied(
    tx: &rusqlite::Transaction<'_>,
    revision: i64,
) -> Result<(), AppError> {
    let applied: i64 = tx.query_row(
        "SELECT policy_revision FROM desktop_privacy WHERE singleton=1",
        [],
        |row| row.get(0),
    )?;
    if applied != revision {
        return Err(AppError::new(
            "POLICY_PENDING_APPLY",
            "正在应用新的内容策略，事件保留等待重试",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, std::path::PathBuf, Db, SettingsStore) {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("agent-events.jsonl");
        let db = Db::open_in_memory().unwrap();
        let settings = SettingsStore::new();
        initialize(&db, &settings, &inbox).unwrap();
        (dir, inbox, db, settings)
    }

    #[test]
    fn failed_atomic_publish_does_not_modify_the_applied_policy_or_outputs() {
        let (_dir, inbox, db, settings) = setup();
        let before = read_authority(&inbox).unwrap();
        let revision = db.revision();
        let mut changed = before.policy.clone();
        changed.notify_ended = false;
        let error = commit_with_publish(&inbox, &changed, before.revision, |_, _| {
            Err(AppError::new("INJECTED_WRITE_FAILURE", "fixture"))
        })
        .unwrap_err();
        assert_eq!(error.code, "INJECTED_WRITE_FAILURE");
        assert_eq!(read_authority(&inbox).unwrap(), before);
        assert_eq!(db.revision(), revision);
        assert_eq!(settings.get().unwrap(), snapshot(&before));
    }

    #[test]
    fn committed_policy_survives_projection_failure_and_restart_only_applies_new_revision() {
        let (_dir, inbox, db, settings) = setup();
        let full = CapturePolicy {
            result_content_mode: crate::model::ResultContentMode::FullFinal,
            ..CapturePolicy::default()
        };
        let committed = commit(&inbox, &full, 0).unwrap();
        apply_projection(&db, &settings, &committed.state).unwrap();
        settings.begin_apply().unwrap();
        let narrow = commit(&inbox, &CapturePolicy::default(), 1).unwrap();
        // Inject a transaction failure after file commit; no consumer can use the old snapshot.
        let error = db
            .with_transaction::<()>(|_| Err(AppError::new("INJECTED_DB_FAILURE", "fixture")))
            .unwrap_err();
        assert_eq!(error.code, "INJECTED_DB_FAILURE");
        assert_eq!(settings.get().unwrap_err().code, "POLICY_PENDING_APPLY");
        assert_eq!(read_authority(&inbox).unwrap(), narrow.state);
        let recovered = SettingsStore::new();
        initialize(&db, &recovered, &inbox).unwrap();
        assert_eq!(recovered.get().unwrap(), snapshot(&narrow.state));
        assert_eq!(narrow.state.capture_generation, 1);
    }

    #[test]
    fn no_op_and_selection_changes_keep_content_generation() {
        let (dir, inbox, db, settings) = setup();
        let before = std::fs::read(dir.path().join("capture-policy.json")).unwrap();
        let revision = db.revision();
        assert_eq!(
            commit(&inbox, &CapturePolicy::default(), 0).unwrap().status,
            PolicySaveStatus::Unchanged
        );
        apply_projection(&db, &settings, &read_authority(&inbox).unwrap()).unwrap();
        assert_eq!(db.revision(), revision);
        assert_eq!(
            before,
            std::fs::read(dir.path().join("capture-policy.json")).unwrap()
        );
        let policy = CapturePolicy {
            notify_started: false,
            ..CapturePolicy::default()
        };
        let receipt = commit(&inbox, &policy, 0).unwrap();
        assert_eq!(receipt.state.capture_generation, 0);
        assert_eq!(receipt.state.revision, 1);
        assert_eq!(
            commit(&inbox, &policy, 0).unwrap().status,
            PolicySaveStatus::Conflict
        );
    }

    #[test]
    fn fresh_database_uses_retained_policy_and_corruption_never_restores_defaults() {
        let (dir, inbox, _db, _settings) = setup();
        let policy = CapturePolicy {
            result_content_mode: crate::model::ResultContentMode::FullFinal,
            ..CapturePolicy::default()
        };
        let saved = commit(&inbox, &policy, 0).unwrap();
        let fresh = Db::open_in_memory().unwrap();
        let settings = SettingsStore::new();
        assert_eq!(initialize(&fresh, &settings, &inbox).unwrap(), saved.state);
        std::fs::write(dir.path().join("capture-policy.json"), b"broken").unwrap();
        assert!(initialize(&fresh, &SettingsStore::new(), &inbox).is_err());
        assert_eq!(
            std::fs::read(dir.path().join("capture-policy.json")).unwrap(),
            b"broken"
        );
    }

    #[test]
    fn missing_committed_authority_never_writes_a_permissive_default() {
        let (dir, inbox, db, settings) = setup();
        let paused = CapturePolicy {
            observe_turns: false,
            ..CapturePolicy::default()
        };
        let saved = commit(&inbox, &paused, 0).unwrap();
        apply_projection(&db, &settings, &saved.state).unwrap();
        std::fs::remove_file(dir.path().join("capture-policy.json")).unwrap();
        let before = db.revision();
        let error = initialize(&db, &SettingsStore::new(), &inbox).unwrap_err();
        assert_eq!(error.code, "CAPTURE_POLICY_UNAVAILABLE");
        assert!(!dir.path().join("capture-policy.json").exists());
        assert_eq!(db.revision(), before);
        assert!(!settings.get().unwrap().notifications.enabled);
    }

    #[test]
    fn offline_selection_revocations_cancel_immediately_and_reenable_never_revives() {
        for disabled in ["observe", "started", "ended", "attention"] {
            let (_dir, inbox, db, settings) = setup();
            let all = CapturePolicy {
                notify_attention: true,
                ..CapturePolicy::default()
            };
            let initial = commit(&inbox, &all, 0).unwrap();
            apply_projection(&db, &settings, &initial.state).unwrap();
            db.with_transaction(|tx| {
                tx.execute("INSERT INTO agent_runs(run_key,agent_kind,instance_id,agent_label,status,last_event_at) VALUES('run','codex','desktop','Codex','running',1)", [])?;
                for kind in ["run_started", "run_completed", "run_failed", "run_interrupted", "attention_required"] {
                    tx.execute("INSERT INTO notification_outbox(id,agent_run_key,event_kind,dedupe_key,client_id,payload_json,priority,status,not_before,expires_at,created_at,updated_at) VALUES(?1,'run',?1,?1,?1,'fixture',1,'pending',1,9999999999999,1,1)", [kind])?;
                }
                Ok(())
            }).unwrap();
            let mut off = all.clone();
            match disabled {
                "observe" => off.observe_turns = false,
                "started" => off.notify_started = false,
                "ended" => off.notify_ended = false,
                "attention" => off.notify_attention = false,
                _ => unreachable!(),
            }
            let saved = commit(&inbox, &off, initial.state.revision).unwrap();
            apply_projection(&db, &settings, &saved.state).unwrap();
            let cancelled = || {
                db.with_transaction(|tx| {
                    Ok(tx.query_row(
                        "SELECT COUNT(*) FROM notification_outbox WHERE status='cancelled'",
                        [],
                        |r| r.get::<_, i64>(0),
                    )?)
                })
                .unwrap()
            };
            let expected = match disabled {
                "observe" => 5,
                "ended" => 3,
                _ => 1,
            };
            assert_eq!(
                cancelled(),
                expected,
                "{disabled} must be applied without a sender"
            );
            let restored = commit(&inbox, &all, saved.state.revision).unwrap();
            apply_projection(&db, &settings, &restored.state).unwrap();
            assert_eq!(
                cancelled(),
                expected,
                "{disabled} must stay cancelled after re-enable"
            );
            assert_eq!(
                restored.state.capture_generation,
                initial.state.capture_generation
            );
        }
    }
}
