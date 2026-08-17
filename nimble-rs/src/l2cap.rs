//! L2CAP connection-oriented channels (CoC): a credit-based data pipe that
//! runs *parallel* to GATT (both over a GAP connection), exposed as
//! operations on the [`BleDriver`].
//!
//! Modeled on the NimBLE L2CAP API of `esp-idf-svc` (`src/ble/l2cap.rs`).
//!
//! A channel is opened either by listening on a PSM
//! ([`l2cap_create_server`](BleDriver::l2cap_create_server)) or by connecting
//! to a peer's PSM ([`l2cap_connect`](BleDriver::l2cap_connect)). Both, plus
//! received SDUs and flow-control notifications, are delivered to the single
//! [`l2cap_subscribe`](BleDriver::l2cap_subscribe) hook.
//!
//! Flow control is credit-based and manual: after handling a
//! [`L2capEvent::Received`] you replenish the peer's credits with
//! [`l2cap_recv_ready`](BleDriver::l2cap_recv_ready); a
//! [`l2cap_send`](BleDriver::l2cap_send) that runs out of credits reports
//! [`SendOutcome::Stalled`] and resumes on [`L2capEvent::TxUnstalled`].

use core::ffi::{c_int, c_void};

use nimble_rs_sys as sys;

use crate::mbuf::Mbuf;
use crate::{BleDriver, BleError, CallbackSlot, ConnHandle};

pub(crate) static L2CAP_CALLBACK: CallbackSlot<dyn for<'a> Fn(L2capEvent<'a>) -> i32 + Sync> =
    CallbackSlot::new();

/// An opaque handle to an open L2CAP channel (wraps `*mut ble_l2cap_chan`).
///
/// # Validity
///
/// A handle is valid only while its channel is open - from
/// [`L2capEvent::Connected`] / [`L2capEvent::Accept`] until the matching
/// [`L2capEvent::Disconnected`]. Using it afterwards is undefined behavior
/// (it dereferences freed NimBLE state). It is `Send`/`Sync` so it can be
/// stashed and used from another task - which is exactly why honoring the
/// "not after `Disconnected`" rule is the caller's responsibility.
#[derive(Clone, Copy)]
pub struct L2capChan(*mut sys::ble_l2cap_chan);

// The pointer is an opaque NimBLE handle; all access goes through NimBLE's
// internally-locked API.
unsafe impl Send for L2capChan {}
unsafe impl Sync for L2capChan {}

/// The result of a non-erroring [`l2cap_send`](BleDriver::l2cap_send).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum SendOutcome {
    /// The whole SDU was handed to the controller.
    Sent,
    /// Ran out of peer credits mid-SDU; the remainder is queued and
    /// transmission resumes once [`L2capEvent::TxUnstalled`] fires. Do not
    /// send again until then.
    Stalled,
}

/// An L2CAP CoC event, delivered to the single
/// [`l2cap_subscribe`](BleDriver::l2cap_subscribe) hook. The hook returns an
/// ATT-style status (`0` = ok); it is only consulted for
/// [`Accept`](Self::Accept), where non-zero rejects the peer.
pub enum L2capEvent<'a> {
    /// A channel opened by [`l2cap_connect`](BleDriver::l2cap_connect)
    /// finished connecting; check `status` (`0` = success) before using `chan`.
    Connected {
        conn_handle: ConnHandle,
        status: i32,
        chan: L2capChan,
    },
    /// A channel disconnected. `chan` must not be used after this event.
    Disconnected {
        conn_handle: ConnHandle,
        chan: L2capChan,
    },
    /// An incoming connection to one of our
    /// [`l2cap_create_server`](BleDriver::l2cap_create_server) PSMs. The
    /// handler must provide the first receive buffer by calling
    /// [`l2cap_recv_ready`](BleDriver::l2cap_recv_ready) on `chan`, and may
    /// reject the peer by returning non-zero from the hook.
    Accept {
        conn_handle: ConnHandle,
        peer_sdu_size: u16,
        chan: L2capChan,
    },
    /// An SDU was received. `data` is valid only for the duration of the
    /// call. After handling it, replenish the peer's credits with
    /// [`l2cap_recv_ready`](BleDriver::l2cap_recv_ready).
    Received {
        conn_handle: ConnHandle,
        chan: L2capChan,
        data: Mbuf<'a>,
    },
    /// A previously [`Stalled`](SendOutcome::Stalled) send can continue
    /// (credits replenished). `status` is non-zero only on an allocation
    /// error mid-SDU.
    TxUnstalled {
        conn_handle: ConnHandle,
        status: i32,
        chan: L2capChan,
    },
    /// A channel MTU reconfiguration completed. `by_peer` is `false` for a
    /// local reconfigure completing and `true` when the peer reconfigured us.
    Reconfigured {
        conn_handle: ConnHandle,
        status: i32,
        chan: L2capChan,
        by_peer: bool,
    },
}

impl<'a> L2capEvent<'a> {
    /// Build an [`L2capEvent`] from a raw NimBLE `ble_l2cap_event`. Returns
    /// `None` for event types this wrapper does not model.
    fn from_raw(event: &'a sys::ble_l2cap_event) -> Option<Self> {
        let anon = &event.__bindgen_anon_1;

        Some(match event.type_ as u32 {
            sys::BLE_L2CAP_EVENT_COC_CONNECTED => {
                let e = unsafe { &anon.connect };
                Self::Connected {
                    conn_handle: e.conn_handle,
                    status: e.status,
                    chan: L2capChan(e.chan),
                }
            }
            sys::BLE_L2CAP_EVENT_COC_DISCONNECTED => {
                let e = unsafe { &anon.disconnect };
                Self::Disconnected {
                    conn_handle: e.conn_handle,
                    chan: L2capChan(e.chan),
                }
            }
            sys::BLE_L2CAP_EVENT_COC_ACCEPT => {
                let e = unsafe { &anon.accept };
                Self::Accept {
                    conn_handle: e.conn_handle,
                    peer_sdu_size: e.peer_sdu_size,
                    chan: L2capChan(e.chan),
                }
            }
            sys::BLE_L2CAP_EVENT_COC_DATA_RECEIVED => {
                let e = unsafe { &anon.receive };
                Self::Received {
                    conn_handle: e.conn_handle,
                    chan: L2capChan(e.chan),
                    data: Mbuf::from_raw(e.sdu_rx),
                }
            }
            sys::BLE_L2CAP_EVENT_COC_TX_UNSTALLED => {
                let e = unsafe { &anon.tx_unstalled };
                Self::TxUnstalled {
                    conn_handle: e.conn_handle,
                    status: e.status,
                    chan: L2capChan(e.chan),
                }
            }
            sys::BLE_L2CAP_EVENT_COC_RECONFIG_COMPLETED
            | sys::BLE_L2CAP_EVENT_COC_PEER_RECONFIGURED => {
                let e = unsafe { &anon.reconfigured };
                Self::Reconfigured {
                    conn_handle: e.conn_handle,
                    status: e.status,
                    chan: L2capChan(e.chan),
                    by_peer: event.type_ as u32 == sys::BLE_L2CAP_EVENT_COC_PEER_RECONFIGURED,
                }
            }
            _ => return None,
        })
    }
}

/// Free an mbuf whose ownership NimBLE handed to us (a received SDU, or a
/// buffer a failing call did not take). Null-safe.
fn free_mbuf(om: *mut sys::os_mbuf) {
    if !om.is_null() {
        unsafe { sys::os_mbuf_free_chain(om) };
    }
}

/// Allocate a receive/transmit SDU buffer of `size` bytes from the system
/// mbuf pool (`os_msys`).
fn alloc_sdu(size: u16) -> Result<*mut sys::os_mbuf, BleError> {
    let om = unsafe { sys::os_msys_get_pkthdr(size, 0) };
    if om.is_null() {
        Err(BleError::new(sys::BLE_HS_ENOMEM as c_int))
    } else {
        Ok(om)
    }
}

unsafe extern "C" fn l2cap_event_cb(event: *mut sys::ble_l2cap_event, _arg: *mut c_void) -> c_int {
    let event = &*event;

    // A received SDU's mbuf is ours to free after dispatch; grab the pointer
    // before dispatching.
    let received_sdu = if event.type_ as u32 == sys::BLE_L2CAP_EVENT_COC_DATA_RECEIVED {
        Some(event.__bindgen_anon_1.receive.sdu_rx)
    } else {
        None
    };

    let status = match (L2CAP_CALLBACK.get(), L2capEvent::from_raw(event)) {
        (Some(cb), Some(event)) => cb(event),
        _ => 0,
    };

    if let Some(om) = received_sdu {
        free_mbuf(om);
    }

    status as c_int
}

/// L2CAP CoC operations on the [`BleDriver`]. Available for any `S`; a
/// channel just needs a GAP connection underneath. `&self`, so callable
/// re-entrantly from within the L2CAP hook (e.g. calling
/// [`l2cap_recv_ready`](Self::l2cap_recv_ready) from an
/// [`Accept`](L2capEvent::Accept)).
impl<'d, S> BleDriver<'d, S> {
    /// Subscribe to L2CAP CoC events ([`L2capEvent`]) - connection lifecycle,
    /// received SDUs, and flow-control notifications for every channel.
    pub fn l2cap_subscribe(
        &self,
        callback: &'static (dyn for<'a> Fn(L2capEvent<'a>) -> i32 + Sync),
    ) {
        L2CAP_CALLBACK.set(Some(callback));
    }

    /// Stop delivering L2CAP events to the subscribed hook.
    pub fn l2cap_unsubscribe(&self) {
        L2CAP_CALLBACK.set(None);
    }

    /// Listen for incoming L2CAP CoC connections on `psm`, negotiating an MTU
    /// of `mtu`. Incoming connections arrive as [`L2capEvent::Accept`] on the
    /// L2CAP hook. May be called at runtime (unlike a GATT service table,
    /// which is fixed at construction).
    pub fn l2cap_create_server(&self, psm: u16, mtu: u16) -> Result<(), BleError> {
        BleError::check(unsafe {
            sys::ble_l2cap_create_server(psm, mtu, Some(l2cap_event_cb), core::ptr::null_mut())
        })
    }

    /// Open an L2CAP CoC to the peer's `psm` over the existing connection
    /// `conn_handle`, negotiating an MTU of `mtu`. The outcome arrives as
    /// [`L2capEvent::Connected`]. The initial receive buffer is allocated
    /// internally (`mtu` bytes from `os_msys`).
    pub fn l2cap_connect(
        &self,
        conn_handle: ConnHandle,
        psm: u16,
        mtu: u16,
    ) -> Result<(), BleError> {
        let sdu_rx = alloc_sdu(mtu)?;

        let rc = unsafe {
            sys::ble_l2cap_connect(
                conn_handle,
                psm,
                mtu,
                sdu_rx,
                Some(l2cap_event_cb),
                core::ptr::null_mut(),
            )
        };

        // `ble_l2cap_connect` only takes ownership of `sdu_rx` once it has
        // allocated the channel; its two pre-allocation failure paths are
        // `EINVAL` (null args - impossible here) and `ENOTCONN`. Every later
        // error path frees the buffer via `ble_l2cap_chan_free`, and its
        // `ENOMEM` is returned from *both* sides of that line, so it is not
        // safe to free on. Free only on the unambiguous pre-allocation codes -
        // this avoids a double-free at the cost of a possible leak on the rare
        // mid-connect allocation failure.
        if rc == sys::BLE_HS_ENOTCONN as c_int || rc == sys::BLE_HS_EINVAL as c_int {
            free_mbuf(sdu_rx);
        }

        BleError::check(rc)
    }

    /// Send `data` as one SDU over `chan`. On success returns
    /// [`SendOutcome::Sent`]; if the peer's credits run out mid-SDU it
    /// returns [`SendOutcome::Stalled`] and the remainder resumes on
    /// [`L2capEvent::TxUnstalled`] (do not call again until then).
    pub fn l2cap_send(&self, chan: L2capChan, data: &[u8]) -> Result<SendOutcome, BleError> {
        // NimBLE fragments/copies the SDU into its own buffers, so `data`
        // need not outlive the call
        let sdu = unsafe {
            sys::ble_hs_mbuf_from_flat(data.as_ptr() as *const c_void, data.len() as u16)
        };
        if sdu.is_null() {
            return Err(BleError::new(sys::BLE_HS_ENOMEM as c_int));
        }

        let rc = unsafe { sys::ble_l2cap_send(chan.0, sdu) };

        match rc as u32 {
            0 => Ok(SendOutcome::Sent),
            sys::BLE_HS_ESTALLED => Ok(SendOutcome::Stalled),
            // `ble_l2cap_coc_send` takes ownership of `sdu` except on its two
            // pre-ownership returns `EBADDATA` (SDU larger than the channel
            // MTU) and `EBUSY`; free it back on those.
            sys::BLE_HS_EBADDATA | sys::BLE_HS_EBUSY => {
                free_mbuf(sdu);
                BleError::check(rc).map(|()| SendOutcome::Sent)
            }
            _ => BleError::check(rc).map(|()| SendOutcome::Sent),
        }
    }

    /// Signal readiness to receive another SDU of up to `sdu_size` bytes on
    /// `chan`, replenishing the peer's credits. Call this to provide the
    /// first buffer on [`L2capEvent::Accept`] and to re-arm after each
    /// [`L2capEvent::Received`]. The buffer is allocated internally from
    /// `os_msys`.
    pub fn l2cap_recv_ready(&self, chan: L2capChan, sdu_size: u16) -> Result<(), BleError> {
        let sdu_rx = alloc_sdu(sdu_size)?;

        let rc = unsafe { sys::ble_l2cap_recv_ready(chan.0, sdu_rx) };

        // `ble_l2cap_coc_recv_ready` stores `sdu_rx` before its `ENOENT`
        // return, so that path owns it; only its pre-store `EINVAL` (null -
        // impossible here) and `EBUSY` returns leave it with us to free.
        if rc == sys::BLE_HS_EBUSY as c_int || rc == sys::BLE_HS_EINVAL as c_int {
            free_mbuf(sdu_rx);
        }

        BleError::check(rc)
    }

    /// Disconnect the L2CAP channel `chan`. The completion arrives as
    /// [`L2capEvent::Disconnected`].
    pub fn l2cap_disconnect(&self, chan: L2capChan) -> Result<(), BleError> {
        BleError::check(unsafe { sys::ble_l2cap_disconnect(chan.0) })
    }
}
