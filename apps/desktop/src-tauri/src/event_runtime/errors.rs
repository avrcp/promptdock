use super::*;

#[derive(Debug)]
pub(super) struct SourceReadError {
    pub(super) code: &'static str,
    pub(super) source: std::io::Error,
}

pub(super) fn maintenance_error(code: &'static str, error: std::io::Error) -> AppError {
    AppError::internal(code, "采集文件维护失败", error.to_string())
}
pub(super) fn source_error(error: std::io::Error) -> SourceReadError {
    let code = if error.kind() == std::io::ErrorKind::PermissionDenied {
        "SOURCE_PERMISSION_DENIED"
    } else {
        "SOURCE_UNAVAILABLE"
    };
    SourceReadError {
        code,
        source: error,
    }
}
