use serde::Serialize;
use std::fmt;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppError {
    pub code: &'static str,
    pub message: String,
}

impl AppError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn internal(
        code: &'static str,
        message: impl Into<String>,
        _detail: impl Into<String>,
    ) -> Self {
        let error = Self {
            code,
            message: message.into(),
        };
        tracing::error!(code = error.code, "application operation failed");
        error
    }

    pub fn store(detail: impl Into<String>) -> Self {
        Self::internal("STORE_UNAVAILABLE", "提示词数据库暂时不可用", detail)
    }
}

impl fmt::Debug for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppError")
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AppError {}

impl From<rusqlite::Error> for AppError {
    fn from(error: rusqlite::Error) -> Self {
        Self::store(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialized_error_hides_internal_detail() {
        let error = AppError::internal("STORE_UNAVAILABLE", "数据库暂时不可用", "secret path");
        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(value["code"], "STORE_UNAVAILABLE");
        assert_eq!(value["message"], "数据库暂时不可用");
        assert!(value.get("detail").is_none());
        assert!(!value.to_string().contains("secret path"));
        assert!(!format!("{error:?}").contains("secret path"));
    }
}
