//! Safe interaction with the NimBLE `os_mbuf` buffer system.

use core::ffi::{c_int, c_void};
use core::marker::PhantomData;

use nimble_rs_sys as sys;

use crate::BleError;

/// View of an `os_mbuf`, the data buffers used by NimBLE.
pub struct Mbuf<'a> {
    om: *mut sys::os_mbuf,
    _p: PhantomData<&'a mut sys::os_mbuf>,
}

impl Mbuf<'_> {
    pub(crate) fn from_raw(om: *mut sys::os_mbuf) -> Self {
        Self {
            om,
            _p: PhantomData,
        }
    }

    /// Copy this mbuf into `buf`, returning the number of bytes copied, or an
    /// error if `buf` is too small.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, BleError> {
        // A completion callback delivered with an error status may carry a null mbuf.
        if self.om.is_null() {
            return Ok(0);
        }

        let mut copied: u16 = 0;

        BleError::check(unsafe {
            sys::ble_hs_mbuf_to_flat(
                self.om,
                buf.as_mut_ptr() as *mut c_void,
                buf.len() as u16,
                &mut copied,
            )
        })?;

        Ok(copied as usize)
    }

    /// Append `buf` to the mbuf.
    pub fn append(&mut self, buf: &[u8]) -> Result<(), BleError> {
        BleError::check(unsafe {
            sys::os_mbuf_append(self.om, buf.as_ptr() as *const c_void, buf.len() as u16)
        })
    }
}

/// Allocate an `os_mbuf` and copy `buf` into it. Errors with `BLE_HS_ENOMEM`
/// if allocation fails.
pub(crate) fn mbuf_from_slice(buf: &[u8]) -> Result<*mut sys::os_mbuf, BleError> {
    let om = unsafe { sys::ble_hs_mbuf_from_flat(buf.as_ptr() as *const c_void, buf.len() as u16) };

    if om.is_null() {
        Err(BleError::new(sys::BLE_HS_ENOMEM as c_int))
    } else {
        Ok(om)
    }
}
