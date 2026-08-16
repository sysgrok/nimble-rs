//! GAP: device name, connection events, and advertising.
//!
//! Modeled on the NimBLE GAP API of `esp-idf-svc` (`src/ble/gap.rs`).

use core::ffi::c_int;
use core::ptr;

use nimble_rs_sys as sys;

use crate::{BleAddr, BleDriver, BleError, ConnHandle};

/// Parameters for a legacy advertising procedure (safe version of
/// `ble_gap_adv_params`).
#[derive(Clone, Copy, Default)]
pub struct BleAdvParams {
    pub conn_mode: u8,
    pub disc_mode: u8,
    pub itvl_min: u16,
    pub itvl_max: u16,
    pub channel_map: u8,
    pub filter_policy: u8,
    pub high_duty_cycle: bool,
}

impl From<&BleAdvParams> for sys::ble_gap_adv_params {
    fn from(params: &BleAdvParams) -> Self {
        let mut raw: sys::ble_gap_adv_params = unsafe { core::mem::zeroed() };

        raw.conn_mode = params.conn_mode;
        raw.disc_mode = params.disc_mode;
        raw.itvl_min = params.itvl_min;
        raw.itvl_max = params.itvl_max;
        raw.channel_map = params.channel_map;
        raw.filter_policy = params.filter_policy;
        raw.set_high_duty_cycle(params.high_duty_cycle as _);

        raw
    }
}

/// Structured advertising payload (safe version of `ble_hs_adv_fields`).
#[derive(Clone, Copy, Default)]
pub struct BleAdvFields<'a> {
    pub flags: u8,
    pub name: Option<&'a str>,
    pub tx_power_level: Option<i8>,
    pub appearance: Option<u16>,
    pub service_data_uuid16: Option<&'a [u8]>,
    pub manufacturer_data: Option<&'a [u8]>,
}

impl From<&BleAdvFields<'_>> for sys::ble_hs_adv_fields {
    fn from(fields: &BleAdvFields) -> Self {
        let mut raw: sys::ble_hs_adv_fields = unsafe { core::mem::zeroed() };

        raw.flags = fields.flags;

        if let Some(name) = fields.name {
            raw.name = name.as_ptr();
            raw.name_len = name.len() as _;
            raw.set_name_is_complete(1);
        }

        if let Some(tx_power_level) = fields.tx_power_level {
            raw.tx_pwr_lvl = tx_power_level;
            raw.set_tx_pwr_lvl_is_present(1);
        }

        if let Some(appearance) = fields.appearance {
            raw.appearance = appearance;
            raw.set_appearance_is_present(1);
        }

        if let Some(service_data) = fields.service_data_uuid16 {
            raw.svc_data_uuid16 = service_data.as_ptr();
            raw.svc_data_uuid16_len = service_data.len() as _;
        }

        if let Some(manufacturer_data) = fields.manufacturer_data {
            raw.mfg_data = manufacturer_data.as_ptr();
            raw.mfg_data_len = manufacturer_data.len() as _;
        }

        raw
    }
}

/// Role-agnostic connection events. NimBLE multiplexes *role-specific* events
/// (server: `Subscribe`/`NotifyComplete`) onto the same connection callback,
/// but those are demuxed to [`GattsEvent`](crate::gatt::server::GattsEvent) -
/// so they are not part of this enum.
pub enum GapEvent {
    Connect {
        conn_handle: ConnHandle,
        status: Result<(), BleError>,
    },
    Disconnect {
        conn_handle: ConnHandle,
        reason: BleError,
    },
    Mtu {
        conn_handle: ConnHandle,
        value: u16,
    },
    Other,
}

impl From<&sys::ble_gap_event> for GapEvent {
    fn from(event: &sys::ble_gap_event) -> Self {
        let anon = &event.__bindgen_anon_1;

        match event.type_ as u32 {
            sys::BLE_GAP_EVENT_CONNECT => {
                let connect = unsafe { &anon.connect };
                Self::Connect {
                    conn_handle: connect.conn_handle,
                    status: BleError::check(connect.status),
                }
            }
            sys::BLE_GAP_EVENT_DISCONNECT => {
                let disconnect = unsafe { &anon.disconnect };
                Self::Disconnect {
                    conn_handle: disconnect.conn.conn_handle,
                    reason: BleError::new(disconnect.reason),
                }
            }
            sys::BLE_GAP_EVENT_MTU => {
                let mtu = unsafe { &anon.mtu };
                Self::Mtu {
                    conn_handle: mtu.conn_handle,
                    value: mtu.value,
                }
            }
            _ => Self::Other,
        }
    }
}

/// A connection descriptor (safe view of `ble_gap_conn_desc`).
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct BleConnDesc(sys::ble_gap_conn_desc);

impl BleConnDesc {
    pub const fn peer_addr(&self) -> BleAddr {
        BleAddr::new(self.0.peer_id_addr.type_, self.0.peer_id_addr.val)
    }
}

/// Look up a connection descriptor by handle.
///
/// A stateless query against the running host, so it is a free function rather
/// than a [`BleDriver`] method - convenient to call from within a GAP event
/// callback.
pub fn conn_find(conn_handle: ConnHandle) -> Result<BleConnDesc, BleError> {
    let mut desc: sys::ble_gap_conn_desc = unsafe { core::mem::zeroed() };
    BleError::check(unsafe { sys::ble_gap_conn_find(conn_handle, &mut desc) })?;

    Ok(BleConnDesc(desc))
}

/// GAP operations on the [`BleDriver`]: advertising, the device name, and GAP
/// event subscription. Available for any `S`. `&self`, so callable
/// re-entrantly from within the GAP event callback.
impl<S> BleDriver<S> {
    /// Set the device name exposed via the GAP service.
    pub fn set_device_name(&self, name: &str) -> Result<(), BleError> {
        // NimBLE copies the name into its own buffer; NUL-terminate on the stack
        let mut buf = [0u8; 64];
        if name.len() >= buf.len() || name.as_bytes().contains(&0) {
            return Err(BleError::new(sys::BLE_HS_EINVAL as c_int));
        }
        buf[..name.len()].copy_from_slice(name.as_bytes());

        BleError::check(unsafe { sys::ble_svc_gap_device_name_set(buf.as_ptr().cast()) })
    }

    /// Subscribe to GAP events (connect / disconnect / MTU / ...). The callback
    /// runs from inside [`BleDriver::run`]'s poll and returns the GAP status
    /// code (`0` on success). The trampoline is wired at
    /// [`adv_start`](Self::adv_start), so this only needs to be set before that.
    pub fn gap_subscribe(&self, callback: &'static (dyn Fn(GapEvent) -> i32 + Sync)) {
        critical_section::with(|_| crate::GAP_CALLBACK.0.set(Some(callback)));
    }

    /// Stop delivering GAP events to the subscribed callback.
    pub fn gap_unsubscribe(&self) {
        critical_section::with(|_| crate::GAP_CALLBACK.0.set(None));
    }

    /// Set the raw advertising payload.
    pub fn adv_set_data(&self, data: &[u8]) -> Result<(), BleError> {
        // NimBLE copies the payload into its own buffer
        BleError::check(unsafe { sys::ble_gap_adv_set_data(data.as_ptr(), data.len() as c_int) })
    }

    /// Encode `fields` into the advertising payload.
    pub fn adv_set_fields(&self, fields: &BleAdvFields) -> Result<(), BleError> {
        let raw: sys::ble_hs_adv_fields = fields.into();

        BleError::check(unsafe { sys::ble_gap_adv_set_fields(&raw) })
    }

    /// Start a legacy advertising procedure. Drive this from a
    /// [`host_subscribe`](BleDriver::host_subscribe) closure once the host has
    /// synced, and restart it from a [`GapEvent::Disconnect`] handler. Events
    /// for the resulting connection are delivered to the
    /// [`gap_subscribe`](Self::gap_subscribe) callback.
    pub fn adv_start(&self, own_addr_type: u8, params: &BleAdvParams) -> Result<(), BleError> {
        let raw: sys::ble_gap_adv_params = params.into();

        // bindgen does not emit `BLE_HS_FOREVER`, as its C macro expands to
        // `INT32_MAX` rather than to an integer literal. Advertise with no timeout.
        const BLE_HS_FOREVER: c_int = i32::MAX;

        let rc = unsafe {
            sys::ble_gap_adv_start(
                own_addr_type,
                ptr::null(),
                BLE_HS_FOREVER as _,
                &raw,
                Some(crate::gap_event_cb),
                ptr::null_mut(),
            )
        };
        if rc == sys::BLE_HS_EALREADY as c_int {
            return Ok(());
        }

        BleError::check(rc)
    }

    /// Stop advertising.
    pub fn adv_stop(&self) -> Result<(), BleError> {
        BleError::check(unsafe { sys::ble_gap_adv_stop() })
    }
}
