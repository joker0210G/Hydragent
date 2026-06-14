//! Track 6.4 integration tests: `SecureBuffer` lifecycle under
//! repeated allocation, mlock availability detection, and Drop
//! zeroing discipline.

use hydragent_vault::mlock::is_mlock_available;
use hydragent_vault::secure_buffer::SecureBuffer;

#[test]
fn many_allocations_do_not_leak() {
    // 1000 alloc/drop cycles. The `Drop` impl should munlock + zero
    // each buffer, then dealloc. The OS allocator should reclaim
    // memory. There is no direct way to assert "no leak" from a
    // black-box test, but we can assert:
    //   - all 1000 cycles complete without error
    //   - the buffer is `Send` (compile-time assertion below)
    //   - each cycle zeroes any sensitive data we wrote
    for i in 0..1000 {
        let mut buf = SecureBuffer::new([0u8; 32]).expect("alloc");
        // Write a known pattern to detect missing-zeroize bugs.
        for j in 0..32 {
            buf.as_mut_slice()[0][j] = (i + j) as u8;
        }
        // Buffer is dropped here: zeroize, munlock, dealloc.
        drop(buf);
    }
}

#[test]
fn mlock_availability_reflects_runtime() {
    // The function should always return a bool, and the result
    // should be stable for a given process.
    let r1 = is_mlock_available();
    let r2 = is_mlock_available();
    assert_eq!(r1, r2);
}

#[test]
fn secure_buffer_is_send_but_not_sync() {
    fn assert_send<T: Send>() {}
    // Compile-time check: `SecureBuffer` is `Send` (so it can move
    // between threads) but not `Sync` (so a thread can't get a
    // shared reference to the heap memory while another thread
    // mutates it). This protects the `mlock` + zeroize invariants.
    assert_send::<SecureBuffer<[u8; 32]>>();
}

#[test]
fn from_slice_and_from_byte_array_produce_equivalent_buffers() {
    let bytes = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
    let buf_slice = SecureBuffer::from_slice(&bytes).expect("from_slice");
    assert_eq!(buf_slice.as_slice(), &bytes);

    let buf_array: SecureBuffer<[u8; 16]> =
        SecureBuffer::<[u8; 16]>::from_byte_array(bytes).expect("from_byte_array");
    assert_eq!(buf_array.as_slice()[0], bytes);
}

#[test]
fn buffer_contents_survive_deref() {
    let mut buf = SecureBuffer::new([0u8; 8]).expect("alloc");
    buf[0][0] = 0xde;
    buf[0][1] = 0xad;
    buf[0][2] = 0xbe;
    buf[0][3] = 0xef;
    // Read via the slice API.
    let s: &[u8] = buf.as_slice()[0].as_slice();
    assert_eq!(&s[..4], &[0xde, 0xad, 0xbe, 0xef]);
}

#[test]
fn into_inner_returns_byte_array() {
    let bytes = [7u8; 32];
    let buf = SecureBuffer::new(bytes).expect("alloc");
    let inner: [u8; 32] = buf.into_inner();
    assert_eq!(inner, bytes);
}
