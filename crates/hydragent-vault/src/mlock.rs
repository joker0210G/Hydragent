//! Cross-platform memory locking (`mlock` / `VirtualLock`).
//!
//! Sensitive data (master keys, derived keys, raw secrets) should not be
//! paged to disk. On Linux/macOS, [`libc::mlock`] pins the pages in physical
//! RAM. On Windows, [`windows_sys::Win32::System::Memory::VirtualLock`]
//! does the same thing (it only prevents paging, not direct memory reads).
//!
//! ## Caveats
//!
//! - On Linux, non-root processes can lock up to `RLIMIT_MEMLOCK` bytes
//!   (default 64 KiB on most systems). For a 32-byte key this is fine.
//! - On Windows, `VirtualLock` requires the pages to be in the working set
//!   and the process must not exceed its "minimum working set size" limit.
//! - macOS: a one-time `mlock` is fine; revocation only works for memory
//!   the process owns (we own our own buffers).
//!
//! Failure to lock is **non-fatal**: we log a warning and continue. The
//! zero-on-drop guarantee is independent of the lock guarantee — both
//! apply when the lock succeeds, only zero-on-drop applies when it fails.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};

/// True if memory locking has ever been observed to succeed on this platform.
/// Used as a hint to skip the call in tight loops, and for tests.
static MLOCK_AVAILABLE: AtomicBool = AtomicBool::new(true);

/// An error from a memory lock call.
#[derive(Debug, thiserror::Error)]
pub enum MlockError {
    /// The platform call returned a non-zero error code.
    #[error("mlock failed: {0}")]
    LockFailed(String),
    /// The unlock call failed.
    #[error("munlock failed: {0}")]
    UnlockFailed(String),
    /// Pointer was null.
    #[error("null pointer passed to mlock")]
    NullPointer,
    /// Length was zero.
    #[error("zero-length mlock is a no-op")]
    ZeroLength,
}

/// Lock `len` bytes starting at `ptr` in physical RAM.
///
/// On Unix this is `mlock(2)`. On Windows this is `VirtualLock`. Both are
/// best-effort: on failure we return the OS error code but do not panic.
pub fn mlock(ptr: NonNull<u8>, len: usize) -> Result<(), MlockError> {
    if len == 0 {
        return Err(MlockError::ZeroLength);
    }
    let result = mlock_impl(ptr.as_ptr(), len);
    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            // On Linux, ENOMEM is common for non-root processes that have
            // hit their RLIMIT_MEMLOCK. We mark mlock as unavailable to
            // avoid log spam on subsequent calls.
            MLOCK_AVAILABLE.store(false, Ordering::Relaxed);
            Err(MlockError::LockFailed(e))
        }
    }
}

/// Unlock a previously locked region. Counterpart to [`mlock`].
pub fn munlock(ptr: NonNull<u8>, len: usize) -> Result<(), MlockError> {
    if len == 0 {
        return Err(MlockError::ZeroLength);
    }
    munlock_impl(ptr.as_ptr(), len).map_err(MlockError::UnlockFailed)
}

/// Returns true if memory locking is believed to work on this platform
/// (no recent failures have been observed).
pub fn is_mlock_available() -> bool {
    MLOCK_AVAILABLE.load(Ordering::Relaxed)
}

// --- platform-specific implementations ---

#[cfg(unix)]
fn mlock_impl(ptr: *mut u8, len: usize) -> Result<(), String> {
    // SAFETY: caller guarantees `ptr` is a valid pointer for `len` bytes.
    let ret = unsafe { libc::mlock(ptr as *const libc::c_void, len) };
    if ret == 0 {
        Ok(())
    } else {
        let errno = std::io::Error::last_os_error();
        Err(format!("mlock(2) returned {} ({})", ret, errno))
    }
}

#[cfg(unix)]
fn munlock_impl(ptr: *mut u8, len: usize) -> Result<(), String> {
    // SAFETY: caller guarantees `ptr` is a valid pointer for `len` bytes.
    let ret = unsafe { libc::munlock(ptr as *const libc::c_void, len) };
    if ret == 0 {
        Ok(())
    } else {
        let errno = std::io::Error::last_os_error();
        Err(format!("munlock(2) returned {} ({})", ret, errno))
    }
}

#[cfg(windows)]
fn mlock_impl(ptr: *mut u8, len: usize) -> Result<(), String> {
    use windows_sys::Win32::System::Memory::VirtualLock;
    // SAFETY: caller guarantees `ptr` is a valid pointer for `len` bytes.
    let ret = unsafe { VirtualLock(ptr as *const std::ffi::c_void, len) };
    if ret != 0 {
        Ok(())
    } else {
        let err = std::io::Error::last_os_error();
        Err(format!("VirtualLock failed ({})", err))
    }
}

#[cfg(windows)]
fn munlock_impl(ptr: *mut u8, len: usize) -> Result<(), String> {
    use windows_sys::Win32::System::Memory::VirtualUnlock;
    // SAFETY: caller guarantees `ptr` is a valid pointer for `len` bytes.
    let ret = unsafe { VirtualUnlock(ptr as *const std::ffi::c_void, len) };
    if ret != 0 {
        Ok(())
    } else {
        let err = std::io::Error::last_os_error();
        Err(format!("VirtualUnlock failed ({})", err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    /// On a normal process we can lock a stack/heap buffer and unlock it.
    /// The test is skipped if mlock is unavailable (common in CI / sandboxes).
    #[test]
    fn mlock_unlock_roundtrip() {
        let mut buf = [0u8; 256];
        let ptr = NonNull::new(buf.as_mut_ptr()).unwrap();
        match mlock(ptr, buf.len()) {
            Ok(()) => {
                munlock(ptr, buf.len()).expect("munlock");
            }
            Err(MlockError::LockFailed(e)) => {
                eprintln!("mlock unavailable, skipping: {}", e);
            }
            Err(e) => panic!("unexpected mlock error: {}", e),
        }
    }

    #[test]
    fn mlock_zero_length_errors() {
        let buf = [0u8; 1];
        let ptr = NonNull::new(buf.as_ptr() as *mut u8).unwrap();
        assert!(mlock(ptr, 0).is_err());
    }

    #[test]
    fn munlock_zero_length_errors() {
        let buf = [0u8; 1];
        let ptr = NonNull::new(buf.as_ptr() as *mut u8).unwrap();
        assert!(munlock(ptr, 0).is_err());
    }

    #[test]
    fn null_pointer_construction_fails() {
        // Sanity: NonNull::new on null is None.
        let null = ptr::null_mut::<u8>();
        assert!(NonNull::new(null).is_none());
    }
}
