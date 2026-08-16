//! GATT client: connection, discovery, read/write, and received
//! notifications, as client operations on the [`BleDriver`].
//!
//! Modeled on the NimBLE GATT client API of `esp-idf-svc`
//! (`src/ble/gatt/client.rs`).
//!
//! Every `ble_gattc_*` operation is initiate-now, complete-later: you start
//! it, and NimBLE invokes a callback when it finishes. We route *all* of
//! those per-operation callbacks through one shared
//! [`gattc_subscribe`](BleDriver::gattc_subscribe) hook, correlated by
//! `conn_handle` (GATT serializes one transaction per connection). Received
//! notifications/indications ([`GattcEvent::Notify`]) arrive on the
//! connection's GAP callback and are demuxed here.

use core::ffi::{c_int, c_void};
use core::ptr;

use nimble_rs_sys as sys;

use crate::mbuf::Mbuf;
use crate::{BleAddr, BleDriver, BleError, BleUuid, CallbackSlot, ConnHandle};

use super::AttrHandle;

pub(crate) static GATTC_CALLBACK: CallbackSlot<dyn for<'a> Fn(GattcEvent<'a>) + Sync> =
    CallbackSlot::new();

/// A GATT-client event, delivered to the single
/// [`gattc_subscribe`](BleDriver::gattc_subscribe) hook.
///
/// The discovery variants fire once per discovered item and then once more
/// with a `None` payload to signal completion. `status` is the raw
/// ATT/`BLE_HS_*` status (`0` on success, `BLE_HS_EDONE` on discovery
/// completion).
pub enum GattcEvent<'a> {
    /// A service discovered by [`discover_services`](BleDriver::discover_services).
    Service {
        conn_handle: ConnHandle,
        status: u16,
        service: Option<GattcService>,
    },
    /// A characteristic discovered by
    /// [`discover_characteristics`](BleDriver::discover_characteristics).
    Characteristic {
        conn_handle: ConnHandle,
        status: u16,
        chr: Option<GattcChr>,
    },
    /// Completion of a [`read`](BleDriver::read). On success `data` holds the
    /// value; check `status` before reading it.
    ReadComplete {
        conn_handle: ConnHandle,
        status: u16,
        attr_handle: AttrHandle,
        data: Mbuf<'a>,
    },
    /// Completion of a [`write`](BleDriver::write).
    WriteComplete {
        conn_handle: ConnHandle,
        status: u16,
        attr_handle: AttrHandle,
    },
    /// A notification or indication pushed by the peer (after subscribing by
    /// writing its CCCD).
    Notify {
        conn_handle: ConnHandle,
        attr_handle: AttrHandle,
        indication: bool,
        data: Mbuf<'a>,
    },
}

impl<'a> GattcEvent<'a> {
    /// Build [`Notify`](Self::Notify) from a raw GAP event. Called from the
    /// GAP trampoline's demux for `BLE_GAP_EVENT_NOTIFY_RX`.
    pub(crate) fn from_notify_rx(event: &'a sys::ble_gap_event) -> Self {
        let notify_rx = unsafe { &event.__bindgen_anon_1.notify_rx };

        Self::Notify {
            conn_handle: notify_rx.conn_handle,
            attr_handle: notify_rx.attr_handle,
            indication: notify_rx.indication() != 0,
            data: Mbuf::from_raw(notify_rx.om),
        }
    }
}

/// A remote service discovered on a peer (safe view of `ble_gatt_svc`).
pub struct GattcService {
    pub start_handle: AttrHandle,
    pub end_handle: AttrHandle,
    pub uuid: BleUuid,
}

impl From<&sys::ble_gatt_svc> for GattcService {
    fn from(svc: &sys::ble_gatt_svc) -> Self {
        Self {
            start_handle: svc.start_handle,
            end_handle: svc.end_handle,
            // `ble_uuid_any_t`'s first union member is the `ble_uuid_t` header
            uuid: unsafe { BleUuid::from_raw((&svc.uuid as *const sys::ble_uuid_any_t).cast()) },
        }
    }
}

/// A remote characteristic discovered on a peer (safe view of `ble_gatt_chr`).
pub struct GattcChr {
    pub def_handle: AttrHandle,
    pub val_handle: AttrHandle,
    /// Characteristic properties bitmask (`BLE_GATT_CHR_PROP_*`).
    pub properties: u8,
    pub uuid: BleUuid,
}

impl From<&sys::ble_gatt_chr> for GattcChr {
    fn from(chr: &sys::ble_gatt_chr) -> Self {
        Self {
            def_handle: chr.def_handle,
            val_handle: chr.val_handle,
            properties: chr.properties,
            uuid: unsafe { BleUuid::from_raw((&chr.uuid as *const sys::ble_uuid_any_t).cast()) },
        }
    }
}

fn error_status(error: *const sys::ble_gatt_error) -> u16 {
    if error.is_null() {
        0
    } else {
        unsafe { (*error).status }
    }
}

unsafe extern "C" fn disc_svc_cb(
    conn_handle: u16,
    error: *const sys::ble_gatt_error,
    service: *const sys::ble_gatt_svc,
    _arg: *mut c_void,
) -> c_int {
    if let Some(cb) = GATTC_CALLBACK.get() {
        cb(GattcEvent::Service {
            conn_handle,
            status: error_status(error),
            service: (!service.is_null()).then(|| GattcService::from(&*service)),
        });
    }
    0
}

unsafe extern "C" fn disc_chr_cb(
    conn_handle: u16,
    error: *const sys::ble_gatt_error,
    chr: *const sys::ble_gatt_chr,
    _arg: *mut c_void,
) -> c_int {
    if let Some(cb) = GATTC_CALLBACK.get() {
        cb(GattcEvent::Characteristic {
            conn_handle,
            status: error_status(error),
            chr: (!chr.is_null()).then(|| GattcChr::from(&*chr)),
        });
    }
    0
}

unsafe extern "C" fn read_cb(
    conn_handle: u16,
    error: *const sys::ble_gatt_error,
    attr: *mut sys::ble_gatt_attr,
    _arg: *mut c_void,
) -> c_int {
    if let Some(cb) = GATTC_CALLBACK.get() {
        let (attr_handle, om) = if attr.is_null() {
            (0, core::ptr::null_mut())
        } else {
            ((*attr).handle, (*attr).om)
        };

        cb(GattcEvent::ReadComplete {
            conn_handle,
            status: error_status(error),
            attr_handle,
            data: Mbuf::from_raw(om),
        });
    }
    0
}

unsafe extern "C" fn write_cb(
    conn_handle: u16,
    error: *const sys::ble_gatt_error,
    attr: *mut sys::ble_gatt_attr,
    _arg: *mut c_void,
) -> c_int {
    if let Some(cb) = GATTC_CALLBACK.get() {
        cb(GattcEvent::WriteComplete {
            conn_handle,
            status: error_status(error),
            attr_handle: if attr.is_null() { 0 } else { (*attr).handle },
        });
    }
    0
}

/// GATT-client operations on the [`BleDriver`]. Available for any `S` - a
/// device can be both a server and a client. `&self`, so callable
/// re-entrantly from within the client callback.
impl<S> BleDriver<S> {
    /// Subscribe to GATT-client events ([`GattcEvent`]): per-operation
    /// completions plus received notifications/indications.
    pub fn gattc_subscribe(&self, callback: &'static (dyn for<'a> Fn(GattcEvent<'a>) + Sync)) {
        GATTC_CALLBACK.set(Some(callback));
    }

    /// Stop delivering GATT-client events to the subscribed hook.
    pub fn gattc_unsubscribe(&self) {
        GATTC_CALLBACK.set(None);
    }

    /// Initiate a connection to `peer`. The connect/disconnect outcome
    /// arrives on the GAP hook ([`gap_subscribe`](BleDriver::gap_subscribe));
    /// the connection's received notifications arrive on the GATTC hook.
    pub fn connect(&self, own_addr_type: u8, peer: &BleAddr) -> Result<(), BleError> {
        // bindgen does not emit `BLE_HS_FOREVER` (its C macro is `INT32_MAX`)
        const BLE_HS_FOREVER: c_int = i32::MAX;

        BleError::check(unsafe {
            sys::ble_gap_connect(
                own_addr_type,
                peer.raw() as *const _,
                BLE_HS_FOREVER,
                ptr::null(),
                Some(crate::gap_event_cb),
                ptr::null_mut(),
            )
        })
    }

    /// Terminate the connection `conn_handle`.
    pub fn disconnect(&self, conn_handle: ConnHandle) -> Result<(), BleError> {
        // 0x13 = BLE_ERR_REM_USER_CONN_TERM ("remote user terminated connection")
        BleError::check(unsafe { sys::ble_gap_terminate(conn_handle, 0x13) })
    }

    /// Discover all of the peer's primary services. Results arrive as
    /// [`GattcEvent::Service`].
    pub fn discover_services(&self, conn_handle: ConnHandle) -> Result<(), BleError> {
        BleError::check(unsafe {
            sys::ble_gattc_disc_all_svcs(conn_handle, Some(disc_svc_cb), ptr::null_mut())
        })
    }

    /// Discover the peer's characteristics in the attribute-handle range
    /// `[start_handle, end_handle]` (e.g. a service's range). Results arrive
    /// as [`GattcEvent::Characteristic`].
    pub fn discover_characteristics(
        &self,
        conn_handle: ConnHandle,
        start_handle: AttrHandle,
        end_handle: AttrHandle,
    ) -> Result<(), BleError> {
        BleError::check(unsafe {
            sys::ble_gattc_disc_all_chrs(
                conn_handle,
                start_handle,
                end_handle,
                Some(disc_chr_cb),
                ptr::null_mut(),
            )
        })
    }

    /// Read the value of `attr_handle` on the peer (a Read Request). The
    /// result arrives as [`GattcEvent::ReadComplete`].
    pub fn read(&self, conn_handle: ConnHandle, attr_handle: AttrHandle) -> Result<(), BleError> {
        BleError::check(unsafe {
            sys::ble_gattc_read(conn_handle, attr_handle, Some(read_cb), ptr::null_mut())
        })
    }

    /// Write `data` to `attr_handle` on the peer as a **Write Request**
    /// (acknowledged): the peer sends a Write Response, and its completion
    /// arrives as [`GattcEvent::WriteComplete`].
    pub fn write(
        &self,
        conn_handle: ConnHandle,
        attr_handle: AttrHandle,
        data: &[u8],
    ) -> Result<(), BleError> {
        // NimBLE copies `data` into its own mbuf, so it need not outlive the call
        BleError::check(unsafe {
            sys::ble_gattc_write_flat(
                conn_handle,
                attr_handle,
                data.as_ptr() as *const c_void,
                data.len() as u16,
                Some(write_cb),
                ptr::null_mut(),
            )
        })
    }

    /// Write `data` to `attr_handle` on the peer as a **Write Command**
    /// (unacknowledged): fire-and-forget, there is **no** completion event.
    /// The returned `Result` only reflects whether the command was accepted
    /// for transmission.
    pub fn write_cmd(
        &self,
        conn_handle: ConnHandle,
        attr_handle: AttrHandle,
        data: &[u8],
    ) -> Result<(), BleError> {
        BleError::check(unsafe {
            sys::ble_gattc_write_no_rsp_flat(
                conn_handle,
                attr_handle,
                data.as_ptr() as *const c_void,
                data.len() as u16,
            )
        })
    }
}
