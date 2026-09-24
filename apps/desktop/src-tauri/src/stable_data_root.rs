use crate::error::AppError;
use std::path::{Path, PathBuf};

const PRODUCT_DIRECTORY: &str = ".promptdock-desktop";

/// Builds the product data root below the user's profile directory.
///
/// On Windows, Tauri resolves `home_dir()` through the Profile known folder.
/// Keeping the product root outside AppData avoids MSIX AppData virtualization
/// selecting a different physical directory for packaged and unpackaged callers.
pub(crate) fn from_profile(profile: &Path) -> Result<PathBuf, AppError> {
    if !profile.is_absolute() {
        return Err(AppError::new(
            "PRODUCT_DATA_ROOT_UNAVAILABLE",
            "无法定位绝对用户配置目录，应用已停止启动",
        ));
    }

    Ok(profile.join(PRODUCT_DIRECTORY))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_root_is_a_fixed_direct_child_of_an_absolute_profile() {
        let profile = std::env::temp_dir().join("PromptDock profile 空 格");
        assert!(profile.is_absolute());

        let root = from_profile(&profile).unwrap();

        assert_eq!(root.parent(), Some(profile.as_path()));
        assert_eq!(root.file_name(), Some(PRODUCT_DIRECTORY.as_ref()));
        assert!(root.is_absolute());
    }

    #[test]
    fn relative_profile_fails_closed() {
        let error = from_profile(Path::new("relative-profile")).unwrap_err();

        assert_eq!(error.code, "PRODUCT_DATA_ROOT_UNAVAILABLE");
    }
}
