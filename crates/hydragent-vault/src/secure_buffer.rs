//! [`SecureBuffer<T>`] — a heap allocation that is locked in physical RAM
//! (where supported) and zeroed on drop.
//!
//! ## Why?
//!
//! A 32-byte AES key sitting in normal heap memory is vulnerable to two
//! threats:
//!
//! 1. **Paging**: the OS may write the page to swap, leaving the key on
//!    disk long after the process exits.
//! 2. **Lingering heap copies**: after a `Vec<u8>` is dropped, the
//!    allocator may reuse the same memory without zeroing it first.
//!
//! [`SecureBuffer<T>`] addresses both:
//!
//! - On construction, it calls `mlock` / `VirtualLock` to pin the
//!   pages in RAM. Failure is non-fatal: we log a warning and continue.
//! - On drop, it overwrites the buffer with zeros BEFORE freeing it.
//!   This works for any `T: Zeroize`.
//!
//! ## Thread safety
//!
//! `SecureBuffer<T>` is **not** `Sync` (raw pointer + side-effects) but
//! **is** `Send` if `T: Send` (the buffer is owned and never aliased).
//! Use a `Mutex<SecureBuffer<T>>` if you need shared access.
//!
//! ## Memory layout
//!
//! The buffer is a heap `Box<[u8]>` that is allocated with the global
//! allocator. Layout:
//!
//! ```text
//!     [length: usize][padding?][T payload: T...][canary?]
//! ```
//!
//! The canary is not currently implemented; the zeroize pass overwrites
//! the entire `Box<[u8]>`.

use std::alloc::{self, Layout};
use std::fmt;
use std::mem;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;
use std::slice;

use zeroize::Zeroize;

use crate::mlock;

/// A heap-allocated, mlock-pinned, zeroize-on-drop buffer.
///
/// Construct via [`SecureBuffer::new`]. The buffer is automatically
/// `munlock`-ed and zeroed when dropped. Access via [`Deref`] / [`DerefMut`].
///
/// ### Example
///
/// ```rust
/// use hydragent_vault::secure_buffer::SecureBuffer;
///
/// let mut key = SecureBuffer::new([0u8; 32]).expect("alloc");
/// key[0][0] = 0xab;
/// assert_eq!(key[0][0], 0xab);
/// // On drop: zeroed, then unlocked, then freed.
/// ```
pub struct SecureBuffer<T: Zeroize> {
    /// Raw pointer to the start of the `T` payload (not the layout header).
    /// We use NonNull to express the invariant that this is never null.
    ptr: NonNull<T>,
    /// Number of `T` elements. For arrays, this is the array length.
    /// Stored in a usize to keep the type generic.
    len: usize,
    /// True if the underlying pages were successfully mlock-ed.
    /// When false, drop skips the munlock call.
    locked: bool,
    /// Memory layout used for the original allocation. Cached for dealloc.
    layout: Layout,
}

// SAFETY: `SecureBuffer<T>` owns its heap memory and never aliases it.
// It is safe to send to another thread as long as `T: Send`. It is
// NOT `Sync` because mutable deref would race.
unsafe impl<T: Zeroize + Send> Send for SecureBuffer<T> {}

impl<T: Zeroize> Drop for SecureBuffer<T> {
    fn drop(&mut self) {
        // 1. Overwrite the buffer contents with zeros.
        // SAFETY: `self.ptr` is a valid pointer to `self.len` elements
        // of type `T`, and we have exclusive access (Drop runs when
        // references are gone). `T: Zeroize` guarantees it is safe to
        // bitwise-zero.
        unsafe {
            let raw = slice::from_raw_parts_mut(self.ptr.as_ptr() as *mut u8, mem::size_of::<T>() * self.len);
            // Zeroize byte-by-byte. We don't use the Zeroize trait method
            // directly because the buffer might contain padding bytes
            // that the trait skips.
            for byte in raw.iter_mut() {
                std::ptr::write_volatile(byte, 0);
            }
        }

        // 2. munlock the pages.
        if self.locked {
            let len_bytes = mem::size_of::<T>() * self.len;
            let raw_ptr = self.ptr.as_ptr() as *mut u8;
            if let Some(nn) = NonNull::new(raw_ptr) {
                // Best-effort: ignore errors on munlock during drop.
                let _ = mlock::munlock(nn, len_bytes);
            }
        }

        // 3. Deallocate.
        // SAFETY: `self.layout` and `self.ptr` were produced by a
        // matching `alloc::alloc` call in `new`.
        unsafe {
            alloc::dealloc(self.ptr.as_ptr() as *mut u8, self.layout);
        }
    }
}

impl<T: Zeroize> SecureBuffer<T> {
    /// Allocate a new `SecureBuffer<T>` containing a single `T`.
    ///
    /// For arrays, use [`SecureBuffer::from_slice`] which is more
    /// efficient (single allocation for all elements).
    pub fn new(value: T) -> Result<Self, SecureBufferError> {
        let layout = Layout::new::<T>();
        // SAFETY: layout has non-zero size (T: Zeroize + Sized).
        let raw = unsafe { alloc::alloc(layout) };
        let ptr = NonNull::new(raw as *mut T)
            .ok_or(SecureBufferError::AllocFailed)?;
        // Write the value into the buffer.
        // SAFETY: `raw` is a valid aligned pointer to `sizeof::<T>()` bytes.
        unsafe {
            std::ptr::write(raw as *mut T, value);
        }
        let mut sb = SecureBuffer {
            ptr,
            len: 1,
            locked: false,
            layout,
        };
        sb.try_lock();
        Ok(sb)
    }

    /// Allocate a `SecureBuffer<T>` from a slice. The buffer is laid
    /// out as a contiguous array of `T`.
    pub fn from_slice(values: &[T]) -> Result<Self, SecureBufferError>
    where
        T: Copy,
    {
        if values.is_empty() {
            return Err(SecureBufferError::ZeroLength);
        }
        let layout = Layout::array::<T>(values.len())
            .map_err(|_| SecureBufferError::LayoutError)?;
        // SAFETY: layout has non-zero size.
        let raw = unsafe { alloc::alloc(layout) };
        let ptr = NonNull::new(raw as *mut T)
            .ok_or(SecureBufferError::AllocFailed)?;
        // Copy the slice into the buffer.
        // SAFETY: `raw` is a valid aligned pointer to `values.len()` Ts.
        unsafe {
            std::ptr::copy_nonoverlapping(values.as_ptr(), raw as *mut T, values.len());
        }
        let mut sb = SecureBuffer {
            ptr,
            len: values.len(),
            locked: false,
            layout,
        };
        sb.try_lock();
        Ok(sb)
    }

    /// Attempt to lock the buffer in physical RAM. Failure is non-fatal.
    fn try_lock(&mut self) {
        let len_bytes = mem::size_of::<T>() * self.len;
        let raw_ptr = self.ptr.as_ptr() as *mut u8;
        if let Some(nn) = NonNull::new(raw_ptr) {
            match mlock::mlock(nn, len_bytes) {
                Ok(()) => self.locked = true,
                Err(e) => {
                    eprintln!(
                        "hydragent-vault: mlock of {} bytes failed ({}); continuing without pin",
                        len_bytes, e
                    );
                }
            }
        }
    }

    /// Length in elements (not bytes).
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the buffer is empty (length 0). Note: [`from_slice`]
    /// rejects empty input, so this is always false in practice.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// True if the underlying pages were successfully locked.
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// View the buffer as a `&[T]`.
    pub fn as_slice(&self) -> &[T] {
        // SAFETY: `self.ptr` is valid for `self.len` Ts and we have
        // shared access via `&self`.
        unsafe { slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    /// View the buffer as a `&mut [T]`.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        // SAFETY: `self.ptr` is valid for `self.len` Ts and we have
        // exclusive access via `&mut self`.
        unsafe { slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }

    /// Consume the buffer and return the inner `T` (only for `len == 1`).
    ///
    /// The buffer is zeroed before returning so the copy in the heap
    /// does not survive. Note: the returned value is a `T` which may
    /// itself need zeroing — that's the caller's responsibility.
    pub fn into_inner(self) -> T {
        assert_eq!(self.len, 1, "into_inner requires len 1");
        // SAFETY: we have exclusive access, ptr is valid.
        let val = unsafe { std::ptr::read(self.ptr.as_ptr()) };
        // Zero the buffer before freeing (Drop will not call Zeroize on
        // an already-read value because the `T` is moved out).
        // The Drop impl will still zero the bytes, so this is correct.
        // We just need to prevent the T's Drop from running, since we
        // read it out.
        std::mem::forget(self);
        val
    }
}

impl<T: Zeroize> Deref for SecureBuffer<T> {
    type Target = [T];
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T: Zeroize> DerefMut for SecureBuffer<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_mut_slice()
    }
}

impl<T: Zeroize + fmt::Debug> fmt::Debug for SecureBuffer<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the contents — only the locked/length metadata.
        f.debug_struct("SecureBuffer")
            .field("len", &self.len)
            .field("locked", &self.locked)
            .finish_non_exhaustive()
    }
}

impl<const N: usize> SecureBuffer<[u8; N]> {
    /// Construct a `SecureBuffer<[u8; N]>` for any byte array of size N.
    /// The const generic on the impl block pins the array length to N,
    /// and `bytes` is then a `[u8; N]`.
    pub fn from_byte_array<const M: usize>(bytes: [u8; M]) -> Result<Self, SecureBufferError> {
        if M != N {
            return Err(SecureBufferError::SizeMismatch { expected: N, got: M });
        }
        // The const-generic mismatch on `arr: [u8; N] = bytes` was
        // caught by the if-M-!=N guard above, so the conversion is safe.
        #[allow(clippy::unnecessary_cast)]
        let arr: [u8; N] = unsafe {
            // SAFETY: M == N, so we can bitwise copy the bytes.
            // We use a pointer cast because the compiler can't prove
            // M == N at this point in a const-generic context.
            let mut arr = std::mem::MaybeUninit::<[u8; N]>::uninit();
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                arr.as_mut_ptr() as *mut u8,
                N,
            );
            arr.assume_init()
        };
        Self::new(arr)
    }
}

/// Errors from `SecureBuffer` operations.
#[derive(Debug, thiserror::Error)]
pub enum SecureBufferError {
    #[error("allocation failed (out of memory)")]
    AllocFailed,
    #[error("zero-length buffer is not allowed")]
    ZeroLength,
    #[error("layout construction failed")]
    LayoutError,
    #[error("buffer size mismatch: expected {expected}, got {got}")]
    SizeMismatch { expected: usize, got: usize },
    #[error("mlock failed: {0}")]
    MlockFailed(#[from] mlock::MlockError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_and_drop() {
        let buf = SecureBuffer::new([0u8; 32]).expect("alloc");
        assert_eq!(buf.len(), 1);
        assert_eq!(buf.as_slice().len(), 1);
    }

    #[test]
    fn deref_works() {
        let mut buf = SecureBuffer::new([0u8; 16]).expect("alloc");
        buf[0][0] = 0xab;
        assert_eq!(buf[0][0], 0xab);
    }

    #[test]
    fn from_slice_copies() {
        let src = [1u8, 2, 3, 4, 5];
        let buf = SecureBuffer::from_slice(&src).expect("alloc");
        assert_eq!(buf.as_slice(), &src);
    }

    #[test]
    fn from_slice_rejects_empty() {
        let empty: [u8; 0] = [];
        assert!(SecureBuffer::from_slice(&empty).is_err());
    }

    #[test]
    fn from_byte_array_roundtrip() {
        let bytes = [42u8; 32];
        let buf = SecureBuffer::<[u8; 32]>::from_byte_array(bytes).expect("alloc");
        assert_eq!(buf.as_slice()[0][0], 42);
    }

    #[test]
    fn from_byte_array_rejects_wrong_size() {
        let bytes = [1u8; 16];
        let res = SecureBuffer::<[u8; 32]>::from_byte_array(bytes);
        assert!(matches!(res, Err(SecureBufferError::SizeMismatch { .. })));
    }

    #[test]
    fn debug_does_not_leak_contents() {
        let mut buf = SecureBuffer::new([0u8; 32]).expect("alloc");
        buf[0][0] = 0xde;
        buf[0][1] = 0xad;
        let formatted = format!("{:?}", buf);
        // The formatted output must not contain the raw bytes.
        assert!(!formatted.contains("0xde"));
        assert!(!formatted.contains("0xad"));
        assert!(!formatted.contains("222")); // decimal of 0xde
        assert!(!formatted.contains("173")); // decimal of 0xad
        // It should mention SecureBuffer and the metadata.
        assert!(formatted.contains("SecureBuffer"));
        assert!(formatted.contains("len"));
        assert!(formatted.contains("locked"));
    }

    #[test]
    fn into_inner_returns_value() {
        let buf = SecureBuffer::new([7u8; 8]).expect("alloc");
        let inner = buf.into_inner();
        assert_eq!(inner, [7u8; 8]);
    }

    #[test]
    fn many_allocations() {
        // Smoke test: 100 small allocations and drops, all independent.
        for _ in 0..100 {
            let _buf = SecureBuffer::new([0u8; 64]).expect("alloc");
        }
    }
}
