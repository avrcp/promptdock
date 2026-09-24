use super::*;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct InboxCursor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) generation: Option<String>,
    pub(super) offset: u64,
    pub(super) file_identity: Option<u128>,
}

pub(super) fn load_cursor_for_source(
    db: &Db,
    source: &SourceDescriptor,
) -> Result<InboxCursor, AppError> {
    db.source_cursor(source)?
        .map(|value| {
            serde_json::from_str(&value).map_err(|error| {
                AppError::internal(
                    "INVALID_SOURCE_CURSOR",
                    "采集 checkpoint 暂时不可用",
                    error.to_string(),
                )
            })
        })
        .transpose()
        .map(|cursor| cursor.unwrap_or_default())
}

pub(super) fn advanced_cursor(
    cursor: &InboxCursor,
    consumed: u64,
) -> Result<InboxCursor, SourceReadError> {
    let offset = cursor
        .offset
        .checked_add(consumed)
        .ok_or_else(|| source_error(std::io::Error::other("inbox cursor overflow")))?;
    Ok(InboxCursor {
        generation: cursor.generation.clone(),
        offset,
        file_identity: cursor.file_identity,
    })
}

pub(super) fn checkpoint_for_source(
    cursor: &InboxCursor,
    record: &InboxRecord,
    source: &SourceDescriptor,
) -> Result<SourceCheckpointUpdate, AppError> {
    let (status, error_code) = match record {
        InboxRecord::Envelope(_) => ("active", None),
        InboxRecord::Rejected(code) => ("degraded", Some((*code).to_string())),
    };
    checkpoint_for_status_for_source(cursor, status, error_code, source)
}

pub(super) fn checkpoint_for_status_for_source(
    cursor: &InboxCursor,
    status: &str,
    error_code: Option<String>,
    source: &SourceDescriptor,
) -> Result<SourceCheckpointUpdate, AppError> {
    Ok(SourceCheckpointUpdate {
        source: source.clone(),
        cursor_json: serde_json::to_string(cursor).map_err(|error| {
            AppError::internal(
                "CHECKPOINT_SERIALIZE_FAILED",
                "采集 checkpoint 暂时不可用",
                error.to_string(),
            )
        })?,
        source_revision: cursor.file_identity.map(|identity| identity.to_string()),
        status: status.into(),
        last_error_code: error_code,
        updated_at: now_ms()?,
    })
}

pub(super) fn persist_source_health_for_source(
    db: &Db,
    events: &dyn RuntimeEventEmitter,
    cursor: &InboxCursor,
    status: &'static str,
    error_code: &'static str,
    source: &SourceDescriptor,
) -> Result<(), AppError> {
    let checkpoint =
        checkpoint_for_status_for_source(cursor, status, Some(error_code.into()), source)?;
    db.checkpoint(&checkpoint)?;
    events.capture_health_changed(&checkpoint);
    Ok(())
}

#[cfg(windows)]
pub(super) fn file_identity(file: &File, _metadata: &std::fs::Metadata) -> Option<u128> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    // SAFETY: the handle is borrowed from a live File and the output points to
    // initialized writable storage for the duration of the call.
    let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let succeeded = unsafe {
        GetFileInformationByHandle(file.as_raw_handle(), std::ptr::from_mut(&mut information))
    };
    if succeeded == 0 {
        return None;
    }
    let volume = u128::from(information.dwVolumeSerialNumber);
    let index =
        (u128::from(information.nFileIndexHigh) << 32) | u128::from(information.nFileIndexLow);
    Some((volume << 64) | index)
}

#[cfg(unix)]
pub(super) fn file_identity(_file: &File, metadata: &std::fs::Metadata) -> Option<u128> {
    use std::os::unix::fs::MetadataExt;

    Some((u128::from(metadata.dev()) << 64) | u128::from(metadata.ino()))
}

#[cfg(not(any(unix, windows)))]
pub(super) fn file_identity(_file: &File, metadata: &std::fs::Metadata) -> Option<u128> {
    metadata
        .created()
        .ok()
        .and_then(|created| created.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
}
