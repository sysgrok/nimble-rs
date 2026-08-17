//! The `nimble_platform_mem_*` heap hooks the esp-nimble fork expects.
//!
//! With `MYNEWT_VAL_BLE_STATIC_TO_DYNAMIC=0` (this crate's fixed
//! configuration) most of the C host is statically allocated, but the
//! esp-nimble fork still heap-allocates a bounded, config-sized set at init:
//! the msys mbuf pools (`os_msys_buf_alloc`), the transport context and
//! pools (`ble_transport_ensure_ctx`/`ble_buf_alloc`), and the GATT service
//! registry (`ble_gatts_add_svcs`/`ble_gatts_start`).
//!
//! - With the `alloc` feature: backed by the global allocator, with the
//!   allocation size stashed in a small header (C's `free`/`realloc` don't
//!   pass the layout back).
//! - Without `alloc`: panicking stubs - the host cannot boot; a static-arena
//!   backend (future feature) would lift this, as every init-time allocation
//!   above is deterministic and `MYNEWT_VAL`-sized.

use core::ffi::c_void;

#[cfg(feature = "alloc")]
mod imp {
    use core::ffi::c_void;

    use alloc::alloc::{alloc_zeroed, dealloc, Layout};

    /// Max-align (C `malloc` contract) header holding the allocation size.
    const HEADER: usize = 16;
    const ALIGN: usize = 16;

    pub unsafe fn malloc(size: usize) -> *mut c_void {
        let Ok(layout) = Layout::from_size_align(HEADER + size, ALIGN) else {
            return core::ptr::null_mut();
        };

        let ptr = alloc_zeroed(layout);
        if ptr.is_null() {
            return core::ptr::null_mut();
        }

        ptr.cast::<usize>().write(size);
        ptr.add(HEADER).cast()
    }

    pub unsafe fn free(ptr: *mut c_void) {
        if ptr.is_null() {
            return;
        }

        let base = ptr.cast::<u8>().sub(HEADER);
        let size = base.cast::<usize>().read();
        dealloc(
            base,
            Layout::from_size_align_unchecked(HEADER + size, ALIGN),
        );
    }

    pub unsafe fn realloc(ptr: *mut c_void, new_size: usize) -> *mut c_void {
        if ptr.is_null() {
            return malloc(new_size);
        }

        let old_size = ptr.cast::<u8>().sub(HEADER).cast::<usize>().read();

        let new = malloc(new_size);
        if !new.is_null() {
            core::ptr::copy_nonoverlapping(
                ptr.cast::<u8>(),
                new.cast::<u8>(),
                old_size.min(new_size),
            );
            free(ptr);
        }
        new
    }
}

#[cfg(not(feature = "alloc"))]
mod imp {
    use core::ffi::c_void;

    pub unsafe fn malloc(_size: usize) -> *mut c_void {
        panic!(
            "nimble_platform_mem_malloc called without the `alloc` feature \
             (GATT service registration needs it)"
        );
    }

    pub unsafe fn free(ptr: *mut c_void) {
        if !ptr.is_null() {
            panic!("nimble_platform_mem_free called without the `alloc` feature");
        }
    }

    pub unsafe fn realloc(_ptr: *mut c_void, _size: usize) -> *mut c_void {
        panic!("nimble_platform_mem_realloc called without the `alloc` feature");
    }
}

#[no_mangle]
unsafe extern "C" fn nimble_platform_mem_malloc(size: usize) -> *mut c_void {
    imp::malloc(size)
}

#[no_mangle]
unsafe extern "C" fn nimble_platform_mem_calloc(n: usize, size: usize) -> *mut c_void {
    // The `alloc` implementation zeroes; the no-alloc one panics anyway
    match n.checked_mul(size) {
        Some(total) => imp::malloc(total),
        None => core::ptr::null_mut(),
    }
}

#[no_mangle]
unsafe extern "C" fn nimble_platform_mem_realloc(ptr: *mut c_void, size: usize) -> *mut c_void {
    imp::realloc(ptr, size)
}

#[no_mangle]
unsafe extern "C" fn nimble_platform_mem_free(ptr: *mut c_void) {
    imp::free(ptr)
}
