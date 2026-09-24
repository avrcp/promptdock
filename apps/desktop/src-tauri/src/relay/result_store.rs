use super::result_protocol::RelayResultReceipt;
use crate::{db::Db, error::AppError};
use rusqlite::{params, OptionalExtension};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResultPublicationRecord {
    pub outbox_id: String,
    pub source_hash: String,
    pub result_revision: i64,
    pub destination_identity: String,
    pub request_digest: String,
    pub accepted_at: Option<i64>,
    pub page_state: Option<String>,
    pub page_expires_at: Option<i64>,
    pub notification_id: Option<String>,
    pub notification_status: Option<String>,
    pub updated_at: Option<i64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResultReceiptApply {
    Applied,
    AlreadyApplied,
    Stale,
}

pub(crate) fn register(db: &Db, record: &ResultPublicationRecord) -> Result<(), AppError> {
    db.with_transaction(|tx| {
        tx.execute(
            "INSERT INTO relay_result_publications(outbox_id, source_hash, result_revision, destination_identity, request_digest) VALUES (?, ?, ?, ?, ?) ON CONFLICT(outbox_id) DO NOTHING",
            params![record.outbox_id, record.source_hash, record.result_revision, record.destination_identity, record.request_digest],
        )?;
        let current = load_tx(tx, &record.outbox_id)?
            .ok_or_else(|| AppError::new("RESULT_METADATA_UNAVAILABLE", "结果发布登记不可用"))?;
        if current.source_hash != record.source_hash || current.result_revision != record.result_revision || current.destination_identity != record.destination_identity || current.request_digest != record.request_digest {
            return Err(AppError::new("RESULT_IDEMPOTENCY_CONFLICT", "结果发布身份与首次尝试不一致"));
        }
        Ok(())
    })
}
pub(crate) fn apply(
    db: &Db,
    outbox_id: &str,
    receipt: &RelayResultReceipt,
    allow_notification_change: bool,
) -> Result<ResultReceiptApply, AppError> {
    apply_inner(db, outbox_id, receipt, allow_notification_change, None)
}
pub(crate) fn apply_action(
    db: &Db,
    id: &str,
    receipt: &RelayResultReceipt,
    action: &str,
    request_id: &str,
) -> Result<ResultReceiptApply, AppError> {
    apply_inner(
        db,
        id,
        receipt,
        action == "resend",
        Some((action, request_id)),
    )
}
fn apply_inner(
    db: &Db,
    outbox_id: &str,
    receipt: &RelayResultReceipt,
    allow_notification_change: bool,
    completion: Option<(&str, &str)>,
) -> Result<ResultReceiptApply, AppError> {
    db.try_with_connection(|conn| {
        let tx = conn.unchecked_transaction()?;
        let current = load_tx(&tx, outbox_id)?.ok_or_else(|| AppError::new("RESULT_METADATA_UNAVAILABLE", "结果发布登记缺失"))?;
        if !receipt.is_valid_for(outbox_id, &current.source_hash) { return Err(AppError::new("RESULT_RECEIPT_INVALID", "结果回执与已冻结身份不一致")); }
        if current.updated_at.is_some_and(|version| receipt.updated_at < version) { return Ok(ResultReceiptApply::Stale); }
        let pending_resend: (Option<String>,Option<String>) = tx.query_row("SELECT pending_resend_request_id,pending_resend_notification_id FROM relay_result_publications WHERE outbox_id=?", [outbox_id], |row| Ok((row.get(0)?,row.get(1)?)))?;
        if completion.is_some_and(|(action,_)| action == "resend") && pending_resend.0.is_some() && pending_resend.1.as_deref() == Some(receipt.notification_id.as_str()) {
            return Err(AppError::new("RESULT_RESEND_UNCONFIRMED", "重发回执尚未确认新的通知身份"));
        }
        let allow_notification_change = allow_notification_change || (pending_resend.0.is_some() && pending_resend.1 == current.notification_id);
        if current.notification_id.as_deref().is_some_and(|id| id != receipt.notification_id) && !allow_notification_change { return Err(AppError::new("RESULT_NOTIFICATION_CONFLICT", "结果回执试图替换首次通知身份")); }
        if current.accepted_at.is_some_and(|value| value != receipt.accepted_at) || current.page_expires_at.is_some_and(|value| value != receipt.page_expires_at) { return Err(AppError::new("RESULT_RECEIPT_CONFLICT", "结果回执试图更改冻结的时间事实")); }
        if current.page_state.as_deref().is_some_and(is_terminal_page) && receipt.page_state.as_str() == "available" { return Err(AppError::new("RESULT_PAGE_STATE_CONFLICT", "结果页终态不能回退为可访问")); }
        if current.notification_status.as_deref().is_some_and(is_terminal_notification) && !is_terminal_notification(receipt.notification_status.as_str()) && !(allow_notification_change && current.notification_id.as_deref() != Some(receipt.notification_id.as_str())) { return Err(AppError::new("RESULT_NOTIFICATION_STATE_CONFLICT", "终态通知只能通过显式重发创建新身份")); }
        if let Some(updated_at) = current.updated_at {
            if receipt.updated_at < updated_at { return Ok(ResultReceiptApply::Stale); }
            if receipt.updated_at == updated_at {
                if current.page_state.as_deref() == Some(receipt.page_state.as_str()) && current.page_expires_at == Some(receipt.page_expires_at) && current.notification_id.as_deref() == Some(receipt.notification_id.as_str()) && current.notification_status.as_deref() == Some(receipt.notification_status.as_str()) {
                    if let Some((action,request_id)) = completion { complete_action_tx(&tx,outbox_id,action,request_id)?; }
                    tx.commit()?;
                    return Ok(ResultReceiptApply::AlreadyApplied);
                }
                return Err(AppError::new("RESULT_RECEIPT_CONFLICT", "相同结果回执版本包含冲突事实"));
            }
        }
        tx.execute("UPDATE relay_result_publications SET accepted_at=?, page_state=?, page_expires_at=?, notification_id=?, notification_status=?, updated_at=? WHERE outbox_id=?", params![receipt.accepted_at, receipt.page_state.as_str(), receipt.page_expires_at, receipt.notification_id, receipt.notification_status, receipt.updated_at, outbox_id])?;
        // A background poll observes the durable resend intent without an
        // action response. Clear it only when the intent still names the
        // current (old) notification and this validated receipt replaced it.
        if completion.is_none()
            && pending_resend.0.is_some()
            && pending_resend.1 == current.notification_id
            && pending_resend.1.as_deref() != Some(receipt.notification_id.as_str())
        {
            tx.execute(
                "UPDATE relay_result_publications
                 SET pending_resend_request_id = NULL, pending_resend_notification_id = NULL
                 WHERE outbox_id = ?1
                   AND pending_resend_request_id = ?2
                   AND pending_resend_notification_id = ?3
                   AND notification_id = ?4",
                params![
                    outbox_id,
                    pending_resend.0,
                    pending_resend.1,
                    receipt.notification_id,
                ],
            )?;
        }
        tx.execute("UPDATE notification_outbox SET remote_status=?, remote_updated_at=? WHERE id=?", params![receipt.notification_status, receipt.updated_at, outbox_id])?;
        if receipt.page_state.as_str() == "revoked" {
            tx.execute("UPDATE relay_result_publications SET pending_revoke_request_id=NULL WHERE outbox_id=?", [outbox_id])?;
        }
        if let Some((action,request_id)) = completion { complete_action_tx(&tx,outbox_id,action,request_id)?; }
        tx.commit()?;
        Ok(ResultReceiptApply::Applied)
    })
}
/// Persist the user intent before HTTP. A retry after process restart reuses it.
pub(crate) fn register_action(
    db: &Db,
    id: &str,
    action: &str,
    request_id: &str,
) -> Result<String, AppError> {
    db.with_transaction(|tx| {
        let (update, select) = match action {
            "resend" => ("UPDATE relay_result_publications SET pending_resend_request_id=COALESCE(pending_resend_request_id,?1),pending_resend_notification_id=COALESCE(pending_resend_notification_id,notification_id) WHERE outbox_id=?2 AND accepted_at IS NOT NULL", "SELECT pending_resend_request_id FROM relay_result_publications WHERE outbox_id=?1"),
            "revoke" => ("UPDATE relay_result_publications SET pending_revoke_request_id=COALESCE(pending_revoke_request_id,?1) WHERE outbox_id=?2 AND accepted_at IS NOT NULL", "SELECT pending_revoke_request_id FROM relay_result_publications WHERE outbox_id=?1"),
            _ => return Err(AppError::new("INVALID_RESULT_ACTION", "结果操作无效")),
        };
        if tx.execute(update, params![request_id,id])? != 1 { return Err(AppError::new("RESULT_NOT_PUBLISHED", "结果尚未被服务器接管")); }
        tx.query_row(select, [id], |row| row.get(0)).map_err(AppError::from)
    })
}
fn complete_action_tx(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
    action: &str,
    request_id: &str,
) -> Result<(), AppError> {
    let sql = match action {
            "resend" => "UPDATE relay_result_publications SET pending_resend_request_id=NULL,pending_resend_notification_id=NULL WHERE outbox_id=?1 AND pending_resend_request_id=?2",
            "revoke" => "UPDATE relay_result_publications SET pending_revoke_request_id=NULL WHERE outbox_id=?1 AND pending_revoke_request_id=?2",
            _ => return Err(AppError::new("INVALID_RESULT_ACTION", "结果操作无效")),
        };
    tx.execute(sql, params![id, request_id])?;
    Ok(())
}
fn is_terminal_page(state: &str) -> bool {
    matches!(state, "revoked" | "expired" | "content_unavailable")
}
fn is_terminal_notification(state: &str) -> bool {
    matches!(
        state,
        "provider_accepted"
            | "dead_letter"
            | "expired"
            | "cancelled"
            | "blocked_target_changed"
            | "delivery_unknown"
    )
}
pub(crate) fn detail(db: &Db, id: &str) -> Result<Option<ResultPublicationRecord>, AppError> {
    db.with_connection(|conn| load_conn(conn, id))
}
fn decode(row: &rusqlite::Row<'_>) -> rusqlite::Result<ResultPublicationRecord> {
    Ok(ResultPublicationRecord {
        outbox_id: row.get(0)?,
        source_hash: row.get(1)?,
        result_revision: row.get(2)?,
        destination_identity: row.get(3)?,
        request_digest: row.get(4)?,
        accepted_at: row.get(5)?,
        page_state: row.get(6)?,
        page_expires_at: row.get(7)?,
        notification_id: row.get(8)?,
        notification_status: row.get(9)?,
        updated_at: row.get(10)?,
    })
}
fn load_conn(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<Option<ResultPublicationRecord>, AppError> {
    conn.query_row("SELECT outbox_id,source_hash,result_revision,destination_identity,request_digest,accepted_at,page_state,page_expires_at,notification_id,notification_status,updated_at FROM relay_result_publications WHERE outbox_id=?", [id], decode).optional().map_err(AppError::from)
}
fn load_tx(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
) -> Result<Option<ResultPublicationRecord>, AppError> {
    tx.query_row("SELECT outbox_id,source_hash,result_revision,destination_identity,request_digest,accepted_at,page_state,page_expires_at,notification_id,notification_status,updated_at FROM relay_result_publications WHERE outbox_id=?",[id],decode).optional().map_err(AppError::from)
}

#[cfg(test)]
mod action_tests {
    use super::super::result_protocol::RelayResultPageState;
    use super::*;

    fn seed(db: &Db) {
        db.with_transaction(|tx| {
            tx.execute("INSERT INTO notification_outbox(id,event_kind,dedupe_key,client_id,payload_json,priority,status,not_before,expires_at,created_at,updated_at) VALUES('r','test','r','r','{}',1,'delivered',1,999,1,1)", [])?;
            Ok(())
        }).unwrap();
        register(
            db,
            &ResultPublicationRecord {
                outbox_id: "r".into(),
                source_hash: "a".repeat(64),
                result_revision: 1,
                destination_identity: "test-destination".into(),
                request_digest: "b".repeat(64),
                accepted_at: None,
                page_state: None,
                page_expires_at: None,
                notification_id: None,
                notification_status: None,
                updated_at: None,
            },
        )
        .unwrap();
        apply(db, "r", &receipt("first", 1), false).unwrap();
    }
    fn receipt(id: &str, version: i64) -> RelayResultReceipt {
        RelayResultReceipt {
            schema_version: 1,
            result_id: "r".into(),
            source_hash: "a".repeat(64),
            accepted_at: 1,
            updated_at: version,
            page_state: RelayResultPageState::Available,
            page_expires_at: 999,
            notification_id: id.into(),
            notification_status: "provider_accepted".into(),
        }
    }
    #[test]
    fn pending_resend_survives_restart_and_poll_recovers_response_loss_once() {
        let directory = tempfile::tempdir().unwrap();
        let db = Db::open(directory.path()).unwrap();
        seed(&db);
        assert_eq!(
            register_action(&db, "r", "resend", "intent-a").unwrap(),
            "intent-a"
        );
        drop(db);
        let db = Db::open(directory.path()).unwrap();
        assert_eq!(
            register_action(&db, "r", "resend", "intent-b").unwrap(),
            "intent-a"
        );
        assert_eq!(
            apply(&db, "r", &receipt("second", 2), false).unwrap(),
            ResultReceiptApply::Applied
        );
        assert_eq!(
            apply(&db, "r", &receipt("first", 1), false).unwrap(),
            ResultReceiptApply::Stale
        );
        assert!(apply(&db, "r", &receipt("third", 3), false).is_err());
        assert_eq!(
            detail(&db, "r")
                .unwrap()
                .unwrap()
                .notification_id
                .as_deref(),
            Some("second")
        );
        assert_eq!(
            register_action(&db, "r", "resend", "intent-c").unwrap(),
            "intent-c"
        );
    }

    #[test]
    fn generic_poll_completes_resend_intent_and_removes_terminal_row_from_hot_queue() {
        use crate::notification::outbox::OutboxRepository;
        use std::sync::Arc;

        let db = Arc::new(Db::open_in_memory().unwrap());
        seed(&db);
        db.with_connection(|conn| {
            conn.execute(
                "UPDATE notification_outbox
                 SET acceptance_stage = 'relay', relay_notification_id = 'first',
                     remote_status = 'provider_accepted', delivered_at = 1",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        register_action(&db, "r", "resend", "intent-a").unwrap();
        let outbox = OutboxRepository::new(Arc::clone(&db));
        assert_eq!(
            outbox
                .relay_reconciliation_candidates_after(10, None)
                .unwrap()
                .len(),
            1
        );

        assert_eq!(
            apply(&db, "r", &receipt("second", 2), false).unwrap(),
            ResultReceiptApply::Applied
        );
        assert!(outbox
            .relay_reconciliation_candidates_after(10, None)
            .unwrap()
            .is_empty());
        assert_eq!(
            register_action(&db, "r", "resend", "intent-b").unwrap(),
            "intent-b"
        );
    }
    #[test]
    fn action_receipt_and_completion_are_atomic_including_already_applied() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        register_action(&db, "r", "resend", "a").unwrap();
        apply_action(&db, "r", &receipt("second", 2), "resend", "a").unwrap();
        assert_eq!(register_action(&db, "r", "resend", "b").unwrap(), "b");
        // An unchanged old receipt must not complete a newly registered resend.
        assert!(apply_action(&db, "r", &receipt("second", 2), "resend", "b").is_err());
        assert_eq!(register_action(&db, "r", "resend", "c").unwrap(), "b");
        apply(&db, "r", &receipt("third", 3), false).unwrap();
        assert_eq!(
            apply_action(&db, "r", &receipt("third", 3), "resend", "b").unwrap(),
            ResultReceiptApply::AlreadyApplied
        );
        assert_eq!(register_action(&db, "r", "resend", "d").unwrap(), "d");
    }
    #[test]
    fn pending_revoke_never_authorizes_notification_replacement() {
        let db = Db::open_in_memory().unwrap();
        seed(&db);
        register_action(&db, "r", "revoke", "revoke-a").unwrap();
        assert!(apply(&db, "r", &receipt("second", 2), false).is_err());
        assert_eq!(
            register_action(&db, "r", "revoke", "revoke-b").unwrap(),
            "revoke-a"
        );
    }
}
