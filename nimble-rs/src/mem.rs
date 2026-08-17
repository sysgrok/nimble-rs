//! The `nimble_platform_mem_*` heap hooks the esp-nimble fork expects.
//!
//! With `MYNEWT_VAL_BLE_STATIC_TO_DYNAMIC=0` (this crate's fixed
//! configuration) most of the C host is statically allocated, but the
//! esp-nimble fork still heap-allocates a bounded, config-sized set at init:
//! the msys mbuf pools (`os_msys_buf_alloc`), the transport context and
//! pools (`ble_transport_ensure_ctx`/`ble_buf_alloc`), and the GATT service
//! registry (`ble_gatts_add_svcs`/`ble_gatts_start`).
//!
//! - With the `use-c-heap` feature (a default): thin delegation to the
//!   platform's C heap - `libc` on hosted targets, and on baremetal whatever
//!   provides the C allocation entry points (e.g. `esp-alloc`, or `tinyrlibc`
//!   with its `alloc` feature routing them to the Rust global allocator) -
//!   the same pattern the `openthread` and `mbedtls-rs` wrappers use.
//! - Without it: the symbols are **left undefined**, for the application to
//!   provide as it sees fit (a static arena, a pool, a custom heap) - the
//!   four `extern "C"` functions below are the contract, and the linker
//!   will list exactly what is missing. Note that `nimble_platform_mem_malloc`
//!   must return **zeroed** memory (the C consumers were written against
//!   ESP-IDF's calloc-backed hooks).

/// An owned, exact-size slice on the C-host heap backend - the `MBox` idea
/// from `mbedtls-rs`, extended to slices. Allocated zeroed via the same
/// `nimble_platform_mem_*` hooks the C host uses (whichever backend provides
/// them), freed on drop. The backing never moves, so raw pointers into it
/// stay valid for the lifetime of the owner - which is what the runtime GATT
/// service builder needs for its def-tree pointer graph.
pub(crate) struct CSlice<T: Copy> {
    ptr: core::ptr::NonNull<T>,
    len: usize,
}

impl<T: Copy> CSlice<T> {
    pub fn new_zeroed(len: usize) -> Result<Self, crate::BleError> {
        if len == 0 {
            return Ok(Self {
                ptr: core::ptr::NonNull::dangling(),
                len: 0,
            });
        }

        core::ptr::NonNull::new(
            unsafe { hooks::nimble_platform_mem_calloc(len, core::mem::size_of::<T>()) }.cast(),
        )
        .map(|ptr| Self { ptr, len })
        .ok_or(crate::BleError::new(crate::sys::BLE_HS_ENOMEM as _))
    }
}

impl<T: Copy> core::ops::Deref for CSlice<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        unsafe { core::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl<T: Copy> core::ops::DerefMut for CSlice<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl<T: Copy> Drop for CSlice<T> {
    fn drop(&mut self) {
        if self.len > 0 {
            unsafe { hooks::nimble_platform_mem_free(self.ptr.as_ptr().cast()) };
        }
    }
}

// An owned buffer of `Copy` data; safe to hand across threads.
unsafe impl<T: Copy + Send> Send for CSlice<T> {}
unsafe impl<T: Copy + Sync> Sync for CSlice<T> {}

// The backend contract, as seen from the Rust side (`CSlice` below): with
// `use-c-heap` these resolve (at the symbol level) to the definitions above;
// without it, to whatever the application provides. (A nested module, so the
// Rust names don't collide with those definitions.)
mod hooks {
    use core::ffi::c_void;

    extern "C" {
        pub fn nimble_platform_mem_calloc(nmemb: usize, size: usize) -> *mut c_void;
        pub fn nimble_platform_mem_free(ptr: *mut c_void);
    }
}

#[cfg(feature = "use-c-heap")]
mod imp {
    use core::ffi::c_void;

    extern "C" {
        fn calloc(nmemb: usize, size: usize) -> *mut c_void;
        fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void;
        fn free(ptr: *mut c_void);
    }

    #[no_mangle]
    unsafe extern "C" fn nimble_platform_mem_malloc(size: usize) -> *mut c_void {
        calloc(1, size)
    }

    #[no_mangle]
    unsafe extern "C" fn nimble_platform_mem_calloc(n: usize, size: usize) -> *mut c_void {
        calloc(n, size)
    }

    #[no_mangle]
    unsafe extern "C" fn nimble_platform_mem_realloc(ptr: *mut c_void, size: usize) -> *mut c_void {
        realloc(ptr, size)
    }

    #[no_mangle]
    unsafe extern "C" fn nimble_platform_mem_free(ptr: *mut c_void) {
        free(ptr)
    }
}

/// The C `assert()` hook of the bundled bare-metal sysroot
/// (`nimble-rs-sys/gen/sysroot/include/assert.h`); on hosted targets the
/// real libc assert is used instead and this stays dead code.
#[no_mangle]
extern "C" fn nimble_rs_assert_fail(file: *const core::ffi::c_char, line: core::ffi::c_int) -> ! {
    let file = if file.is_null() {
        "<unknown>"
    } else {
        unsafe { core::ffi::CStr::from_ptr(file) }
            .to_str()
            .unwrap_or("<non-utf8>")
    };

    panic!("C assertion failed at {}:{}", file, line);
}
