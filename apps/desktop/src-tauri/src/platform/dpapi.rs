use std::path::Path;

use serde::de::DeserializeOwned;
use serde::Serialize;
use zeroize::Zeroizing;

pub(crate) const MAX_ENCRYPTED_FILE_BYTES: u64 = 4 * 1024 * 1024;
pub(crate) const MAX_PLAINTEXT_FILE_BYTES: usize = 2 * 1024 * 1024;
const DPAPI_PAYLOAD_MARKER: u8 = 1;

#[derive(Debug, thiserror::Error)]
pub(crate) enum DpapiError {
    #[cfg_attr(windows, allow(dead_code))]
    #[error("secret store is unavailable on this platform")]
    UnsupportedPlatform,
    #[error("secret store data is invalid: {0}")]
    InvalidData(&'static str),
    #[error("secret serialization failed")]
    Serialization(#[source] serde_json::Error),
    #[error("secret store file operation failed")]
    Io(#[source] std::io::Error),
    #[error("Windows data protection failed during {operation}")]
    DataProtection {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
}

impl From<serde_json::Error> for DpapiError {
    fn from(source: serde_json::Error) -> Self {
        Self::Serialization(source)
    }
}

impl From<std::io::Error> for DpapiError {
    fn from(source: std::io::Error) -> Self {
        Self::Io(source)
    }
}

pub(crate) fn read_encrypted_json<T: DeserializeOwned>(
    path: &Path,
) -> Result<Option<T>, DpapiError> {
    let metadata = match path.metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if metadata.len() > MAX_ENCRYPTED_FILE_BYTES {
        return Err(DpapiError::InvalidData(
            "encrypted secret file exceeds its limit",
        ));
    }
    let ciphertext = std::fs::read(path)?;
    if ciphertext.len() as u64 > MAX_ENCRYPTED_FILE_BYTES {
        return Err(DpapiError::InvalidData(
            "encrypted secret file exceeds its limit",
        ));
    }
    let plaintext = unprotect_bytes(&ciphertext)?;
    if plaintext.len() > MAX_PLAINTEXT_FILE_BYTES {
        return Err(DpapiError::InvalidData(
            "decrypted secret document exceeds its limit",
        ));
    }
    serde_json::from_slice(&plaintext)
        .map(Some)
        .map_err(DpapiError::from)
}

pub(crate) fn write_encrypted_json<T: Serialize>(
    path: &Path,
    value: &T,
    write_error_code: &'static str,
    write_error_message: &'static str,
) -> Result<(), DpapiError> {
    let plaintext = Zeroizing::new(serde_json::to_vec(value)?);
    if plaintext.len() > MAX_PLAINTEXT_FILE_BYTES {
        return Err(DpapiError::InvalidData("secret document exceeds its limit"));
    }
    let ciphertext = protect_bytes(&plaintext)?;
    if ciphertext.len() as u64 > MAX_ENCRYPTED_FILE_BYTES {
        return Err(DpapiError::InvalidData(
            "encrypted secret document exceeds its limit",
        ));
    }
    crate::atomic_file::write(path, &ciphertext, write_error_code, write_error_message)
        .map_err(|error| DpapiError::Io(std::io::Error::other(error.to_string())))
}

#[cfg(windows)]
pub(crate) fn protect_bytes(plaintext: &[u8]) -> Result<Vec<u8>, DpapiError> {
    use std::ptr;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let framed_length = plaintext
        .len()
        .checked_add(1)
        .ok_or(DpapiError::InvalidData("secret payload is too large"))?;
    let mut framed = Zeroizing::new(Vec::with_capacity(framed_length));
    framed.push(DPAPI_PAYLOAD_MARKER);
    framed.extend_from_slice(plaintext);
    let input_length = u32::try_from(framed.len())
        .map_err(|_| DpapiError::InvalidData("secret payload is too large"))?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: input_length,
        pbData: framed.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let succeeded = unsafe {
        CryptProtectData(
            &input,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if succeeded == 0 {
        return Err(data_protection_error("protect"));
    }
    let allocation = LocalAllocation::new(output, false);
    allocation.copy_bytes()
}

#[cfg(windows)]
pub(crate) fn unprotect_bytes(ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>, DpapiError> {
    use std::ptr;
    use windows_sys::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let input_length = u32::try_from(ciphertext.len())
        .map_err(|_| DpapiError::InvalidData("encrypted secret payload is too large"))?;
    let input = CRYPT_INTEGER_BLOB {
        cbData: input_length,
        pbData: if ciphertext.is_empty() {
            ptr::null_mut()
        } else {
            ciphertext.as_ptr().cast_mut()
        },
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let succeeded = unsafe {
        CryptUnprotectData(
            &input,
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if succeeded == 0 {
        return Err(data_protection_error("unprotect"));
    }
    let allocation = LocalAllocation::new(output, true);
    let mut plaintext = Zeroizing::new(allocation.copy_bytes()?);
    if plaintext.first() != Some(&DPAPI_PAYLOAD_MARKER) {
        return Err(DpapiError::InvalidData("DPAPI payload framing is invalid"));
    }
    plaintext.remove(0);
    Ok(plaintext)
}

#[cfg(windows)]
fn data_protection_error(operation: &'static str) -> DpapiError {
    DpapiError::DataProtection {
        operation,
        source: std::io::Error::last_os_error(),
    }
}

#[cfg(windows)]
struct LocalAllocation {
    blob: windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB,
    zero_on_drop: bool,
}

#[cfg(windows)]
impl LocalAllocation {
    fn new(
        blob: windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB,
        zero_on_drop: bool,
    ) -> Self {
        Self { blob, zero_on_drop }
    }

    fn copy_bytes(&self) -> Result<Vec<u8>, DpapiError> {
        let length = usize::try_from(self.blob.cbData)
            .map_err(|_| DpapiError::InvalidData("DPAPI output is too large"))?;
        if length == 0 {
            return Ok(Vec::new());
        }
        if self.blob.pbData.is_null() {
            return Err(DpapiError::InvalidData(
                "DPAPI returned an invalid output buffer",
            ));
        }
        let bytes = unsafe { std::slice::from_raw_parts(self.blob.pbData, length) };
        Ok(bytes.to_vec())
    }
}

#[cfg(windows)]
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::LocalFree;

        if self.blob.pbData.is_null() {
            return;
        }
        if self.zero_on_drop && self.blob.cbData > 0 {
            unsafe {
                use zeroize::Zeroize as _;
                std::slice::from_raw_parts_mut(self.blob.pbData, self.blob.cbData as usize)
                    .zeroize();
            }
        }
        unsafe {
            LocalFree(self.blob.pbData.cast());
        }
        self.blob.pbData = std::ptr::null_mut();
        self.blob.cbData = 0;
    }
}

#[cfg(not(windows))]
pub(crate) fn protect_bytes(_plaintext: &[u8]) -> Result<Vec<u8>, DpapiError> {
    Err(DpapiError::UnsupportedPlatform)
}

#[cfg(not(windows))]
pub(crate) fn unprotect_bytes(_ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>, DpapiError> {
    Err(DpapiError::UnsupportedPlatform)
}
