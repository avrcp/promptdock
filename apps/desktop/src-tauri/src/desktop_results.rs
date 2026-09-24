//! Explicit local content access; never used by status events or diagnostics.
use crate::{
    content_crypto::{unprotect_json, ContentPurpose},
    db::Db,
    error::AppError,
    notification::outbox::NotificationPayloadV2,
};
use rusqlite::OptionalExtension;
use serde::Serialize;
use std::path::Path;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeliveryDetail {
    id: String,
    title: String,
    body: Option<String>,
    content_mode: crate::model::ResultContentMode,
    content_bytes: usize,
    source_hash: Option<String>,
    unavailable_reason: Option<&'static str>,
}

pub(crate) fn detail(db: &Db, id: &str) -> Result<DeliveryDetail, AppError> {
    if id.is_empty() || id.len() > 128 {
        return Err(AppError::new("INVALID_NOTIFICATION_ID", "通知标识无效"));
    }
    let protected: String = db.with_connection(|conn| {
        conn.query_row(
            "SELECT payload_json FROM notification_outbox WHERE id=?",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| AppError::new("NOTIFICATION_NOT_FOUND", "投递记录已清理或不存在"))
    })?;
    let payload: NotificationPayloadV2 =
        unprotect_json(ContentPurpose::NotificationPayload, &protected)?;
    payload.validate()?;
    let available = payload.content_mode != crate::model::ResultContentMode::StatusOnly
        && payload.source_hash.is_some();
    Ok(DeliveryDetail {
        id: id.into(),
        title: payload.title,
        body: available.then_some(payload.body),
        content_mode: payload.content_mode,
        content_bytes: payload.content_bytes,
        source_hash: payload.source_hash,
        unavailable_reason: (!available).then_some("此轮未采集正文，或 Hook 未提供最终回答。"),
    })
}

pub(crate) fn export(db: &Db, dir: &Path, id: &str) -> Result<String, AppError> {
    let detail = detail(db, id)?;
    let body = detail
        .body
        .ok_or_else(|| AppError::new("RESULT_BODY_UNAVAILABLE", "此记录没有可导出的正文"))?;
    let exports = dir.join("exports");
    std::fs::create_dir_all(&exports)
        .map_err(|_| AppError::new("RESULT_EXPORT_FAILED", "无法创建导出目录"))?;
    // User-controlled notification IDs are never used as filesystem names.
    let path = exports.join(format!("result-{}.txt", uuid::Uuid::new_v4()));
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|_| AppError::new("RESULT_EXPORT_FAILED", "无法创建导出文件"))?;
    file.write_all(body.as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|_| AppError::new("RESULT_EXPORT_FAILED", "无法写入导出文件"))?;
    Ok(path.to_string_lossy().into_owned())
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;
    #[test]
    fn explicit_export_preserves_exact_utf8_and_does_not_use_id_as_a_path() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_in_memory().unwrap();
        let body = "  中文🧪\r\n```rust\r\n  let x = 1;\r\n```\n\n";
        let mut payload = NotificationPayloadV2::for_run("Codex", None, None, None, None, None);
        payload.title = "最终回答".into();
        payload.body = body.into();
        payload.content_mode = crate::model::ResultContentMode::FullFinal;
        payload.result_revision = Some(1);
        payload.content_bytes = body.len();
        payload.source_hash = Some(crate::agent::source_hash_sha256(body));
        let protected =
            crate::content_crypto::protect_json(ContentPurpose::NotificationPayload, &payload)
                .unwrap();
        db.with_connection(|conn| {
            conn.execute("INSERT INTO notification_outbox(id,event_kind,dedupe_key,client_id,payload_json,delivery_backend,priority,status,not_before,expires_at,created_at,updated_at) VALUES ('../outside','test','d','d',?,'relay',1,'pending',1,100,1,1)", [protected])?;
            Ok(())
        }).unwrap();
        let path = export(&db, dir.path(), "../outside").unwrap();
        assert_eq!(
            Path::new(&path).parent(),
            Some(dir.path().join("exports").as_path())
        );
        assert_eq!(std::fs::read(path).unwrap(), body.as_bytes());
    }
}
