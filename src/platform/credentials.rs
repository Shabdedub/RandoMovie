//! The TMDB token in Windows Credential Manager (a generic credential for this user on
//! this machine). The token never touches the settings file, logs or the binary.

use std::ffi::c_void;

use windows::Win32::Foundation::ERROR_NOT_FOUND;
use windows::Win32::Security::Credentials::{
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW,
    CredWriteW,
};
use windows::core::{HSTRING, PWSTR};

/// Credential Manager entry for the token.
pub const TMDB_TARGET: &str = "RandoMovie/TMDB";
const USER_NAME: &str = "TMDB API Read Access Token";

/// Reads a secret. `Ok(None)` when there is no entry.
pub fn read(target: &str) -> Result<Option<String>, String> {
    let name = HSTRING::from(target);
    let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
    if let Err(e) = unsafe { CredReadW(&name, CRED_TYPE_GENERIC, None, &mut credential) } {
        if e.code() == ERROR_NOT_FOUND.to_hresult() {
            return Ok(None);
        }
        return Err(e.message());
    }
    let secret = unsafe {
        let c = &*credential;
        let bytes = if c.CredentialBlob.is_null() {
            &[][..]
        } else {
            std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize)
        };
        let secret = String::from_utf8(bytes.to_vec()).ok();
        CredFree(credential as *const c_void);
        secret
    };
    Ok(secret.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()))
}

/// Creates or replaces a secret.
pub fn write(target: &str, secret: &str) -> Result<(), String> {
    let mut target_w: Vec<u16> = target.encode_utf16().chain(Some(0)).collect();
    let mut user_w: Vec<u16> = USER_NAME.encode_utf16().chain(Some(0)).collect();
    let mut blob = secret.as_bytes().to_vec();
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target_w.as_mut_ptr()),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        UserName: PWSTR(user_w.as_mut_ptr()),
        ..Default::default()
    };
    let result = unsafe { CredWriteW(&credential, 0) };
    blob.fill(0);
    result.map_err(|e| e.message())
}

/// Deletes a secret. Deleting a missing entry succeeds.
pub fn delete(target: &str) -> Result<(), String> {
    let name = HSTRING::from(target);
    match unsafe { CredDeleteW(&name, CRED_TYPE_GENERIC, None) } {
        Ok(()) => Ok(()),
        Err(e) if e.code() == ERROR_NOT_FOUND.to_hresult() => Ok(()),
        Err(e) => Err(e.message()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "writes a temporary entry to Windows Credential Manager"]
    fn round_trip() {
        let target = format!("RandoMovie/test-{}", fastrand::u64(..));
        assert_eq!(read(&target).unwrap(), None);
        write(&target, "secret-value").unwrap();
        assert_eq!(read(&target).unwrap().as_deref(), Some("secret-value"));
        write(&target, "replaced").unwrap();
        assert_eq!(read(&target).unwrap().as_deref(), Some("replaced"));
        delete(&target).unwrap();
        assert_eq!(read(&target).unwrap(), None);
        delete(&target).unwrap();
    }
}
