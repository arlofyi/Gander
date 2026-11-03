// SPDX-License-Identifier: Apache-2.0
//! DPAPI (`CryptProtectData`, current-user scope) for the host identity key file.
//!
//! The key file is additionally written with an owner-only DACL (`config_store`). DPAPI ties the
//! bytes to the user's logon credentials, so a copied file (backup, another account, another
//! machine) is useless.

use std::io;

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};

/// Optional entropy binding the blob to Gander's use.
const ENTROPY: &[u8] = b"gander-host-identity-v1";

fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr().cast_mut(),
    }
}

fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    // SAFETY: on success the OS returned `cbData` bytes at `pbData`, allocated with LocalAlloc.
    unsafe {
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData.cast());
        v
    }
}

/// Encrypts `plain` for the current user.
pub fn protect(plain: &[u8]) -> io::Result<Vec<u8>> {
    let input = blob(plain);
    let entropy = blob(ENTROPY);
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: input blobs point to live slices the API only reads; `out` is written on success.
    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            &entropy,
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(take(out))
}

/// Decrypts a blob produced by [`protect`] for the same user.
pub fn unprotect(protected: &[u8]) -> io::Result<Vec<u8>> {
    let input = blob(protected);
    let entropy = blob(ENTROPY);
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: as in `protect`.
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            &entropy,
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(take(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_tamper() {
        let secret = b"not a real key, generated per test";
        let p = protect(secret).unwrap();
        assert_ne!(p.as_slice(), secret.as_slice());
        assert_eq!(unprotect(&p).unwrap(), secret);
        let mut bad = p.clone();
        let last = bad.len() - 1;
        bad[last] ^= 0xff;
        assert!(unprotect(&bad).is_err());
    }
}
