//! Safe, async, cross-platform Rust API for the esp-nimble BLE host, running
//! over any [`bt-hci`](https://crates.io/crates/bt-hci) controller.
//!
//! Work in progress - see `docs/PLAN.md` in the repository root. The overall
//! architecture:
//!
//! - The NimBLE C host is compiled by `nimble-rs-sys`; its OS layer (NPL) and
//!   HCI transport are implemented here, in Rust (`npl`, `hci` modules) -
//!   **thread-free** and **allocation-free**.
//! - [`BleDriver::run`] is the single future that drives everything: the host
//!   event loop, the callout timers and the HCI pump. Host API calls may
//!   briefly stall the executor while awaiting a command ack from the
//!   controller (bounded by the HCI command timeout) - the same latency the C
//!   design imposes, just not hidden behind a context switch.
#![no_std]

use core::cell::Cell;
use core::convert::Infallible;
use core::ffi::{c_int, c_void};
use core::future::poll_fn;
use core::pin::pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::Poll;

use embassy_futures::select::select3;

use nimble_rs_sys as sys;

pub use hci::{ForTransport, NimbleController};

#[cfg(feature = "std")]
extern crate std;

#[cfg(feature = "alloc")]
extern crate alloc;

// `fmt` must come first for its macros to be visible
#[macro_use]
mod fmt;

pub mod gap;
pub mod gatt;
pub mod hci;
#[cfg(feature = "l2cap")]
pub mod l2cap;
pub mod mbuf;
mod mem;
mod npl;
mod port;

/// Support hooks for external test harnesses (the upstream NimBLE test suite
/// runner). Only present with the `external-ll` feature.
#[cfg(feature = "external-ll")]
pub mod test_support {
    /// Fires every callout whose deadline has passed (the job of the driver's
    /// `run()` future, which a synchronous test harness does not poll).
    pub fn fire_due_timers() {
        crate::npl::timers_fire_due();
    }

    /// Drains and runs every event on the default queue.
    pub fn drain_events() {
        crate::port::drain_events();
    }
}

extern "C" {
    // Declared in `store/ram/ble_store_ram.h` but not surfaced by bindgen
    fn ble_store_ram_init();
}

/// A connection handle.
pub type ConnHandle = u16;

/// The "no connection" handle: *local* GATT reads and writes carry this
/// instead of a real connection handle.
pub const CONN_HANDLE_NONE: ConnHandle = sys::BLE_HS_CONN_HANDLE_NONE as ConnHandle;

/// A BLE UUID, either 16-bit (assigned) or 128-bit (vendor-specific).
#[derive(Clone, Copy)]
pub enum BleUuid {
    Uuid16(sys::ble_uuid16_t),
    Uuid128(sys::ble_uuid128_t),
}

impl core::fmt::Debug for BleUuid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Uuid16(uuid) => write!(f, "Uuid16({:#06x})", uuid.value),
            Self::Uuid128(uuid) => write!(f, "Uuid128({:#034x})", u128::from_le_bytes(uuid.value)),
        }
    }
}

impl BleUuid {
    pub const fn uuid16(uuid: u16) -> Self {
        Self::Uuid16(sys::ble_uuid16_t {
            u: sys::ble_uuid_t {
                type_: sys::BLE_UUID_TYPE_16 as u8,
            },
            value: uuid,
        })
    }

    pub const fn uuid128(uuid: u128) -> Self {
        Self::Uuid128(sys::ble_uuid128_t {
            u: sys::ble_uuid_t {
                type_: sys::BLE_UUID_TYPE_128 as u8,
            },
            value: uuid.to_le_bytes(),
        })
    }

    pub const fn as_ptr(&self) -> *const sys::ble_uuid_t {
        match self {
            Self::Uuid16(uuid) => &uuid.u as *const sys::ble_uuid_t,
            Self::Uuid128(uuid) => &uuid.u as *const sys::ble_uuid_t,
        }
    }

    /// # Safety
    ///
    /// `uuid` must point to a valid `ble_uuid_t` header and the concrete
    /// 16-/128-bit body it introduces.
    pub(crate) unsafe fn from_raw(uuid: *const sys::ble_uuid_t) -> Self {
        match (*uuid).type_ as u32 {
            sys::BLE_UUID_TYPE_128 => Self::Uuid128(*uuid.cast::<sys::ble_uuid128_t>()),
            // Only 16- and 128-bit UUIDs are modelled; anything else reads as 16-bit
            _ => Self::Uuid16(*uuid.cast::<sys::ble_uuid16_t>()),
        }
    }
}

impl PartialEq for BleUuid {
    fn eq(&self, other: &Self) -> bool {
        unsafe { sys::ble_uuid_cmp(self.as_ptr(), other.as_ptr()) == 0 }
    }
}

impl Eq for BleUuid {}

/// A BLE device address (type + 6 bytes).
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct BleAddr(sys::ble_addr_t);

impl BleAddr {
    pub const fn new(kind: u8, val: [u8; 6]) -> Self {
        Self(sys::ble_addr_t { type_: kind, val })
    }

    pub const fn raw(&self) -> &sys::ble_addr_t {
        &self.0
    }

    pub const fn kind(&self) -> u8 {
        self.0.type_
    }

    pub const fn val(&self) -> [u8; 6] {
        self.0.val
    }
}

impl core::fmt::Debug for BleAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let v = &self.0.val;
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}/{}",
            v[5], v[4], v[3], v[2], v[1], v[0], self.0.type_
        )
    }
}

/// An error code from the NimBLE host (the `BLE_HS_E*` namespace), or 0.
#[derive(Copy, Clone, Eq, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(transparent)]
pub struct BleError(c_int);

impl BleError {
    /// Creates an error from a raw NimBLE host return code.
    pub const fn new(code: c_int) -> Self {
        Self(code)
    }

    /// The raw NimBLE host return code.
    pub const fn code(&self) -> c_int {
        self.0
    }

    /// Converts a raw NimBLE host return code into a result.
    pub fn check(code: c_int) -> Result<(), Self> {
        if code == 0 {
            Ok(())
        } else {
            Err(Self(code))
        }
    }
}

impl core::fmt::Debug for BleError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "BleError({})", self.0)
    }
}

impl core::fmt::Display for BleError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "BLE host error {}", self.0)
    }
}

#[cfg(feature = "std")]
impl std::error::Error for BleError {}

/// Events of the host stack itself.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum HostEvent {
    /// The host and controller are in sync; the stack is usable from now on.
    Sync,
    /// The host has reset itself (e.g. after a fatal HCI error); the carried
    /// value is a `BLE_HS_E*` reason code. A sync will follow.
    Reset(c_int),
}

/// A critical-section-guarded slot for a subscribed `&'static` callback.
pub(crate) struct CallbackSlot<F: ?Sized + 'static>(Cell<Option<&'static F>>);

// Guarded by the global critical section.
unsafe impl<F: ?Sized> Send for CallbackSlot<F> {}
unsafe impl<F: ?Sized> Sync for CallbackSlot<F> {}

impl<F: ?Sized> CallbackSlot<F> {
    const fn new() -> Self {
        Self(Cell::new(None))
    }

    fn get(&self) -> Option<&'static F> {
        critical_section::with(|_| self.0.get())
    }

    pub(crate) fn set(&self, callback: Option<&'static F>) {
        critical_section::with(|_| self.0.set(callback));
    }
}

static HOST_CALLBACK: CallbackSlot<dyn Fn(HostEvent) + Sync> = CallbackSlot::new();
static GAP_CALLBACK: CallbackSlot<dyn for<'a> Fn(gap::GapEvent<'a>) -> i32 + Sync> =
    CallbackSlot::new();
#[cfg(feature = "gatt-server")]
static GATTS_CALLBACK: CallbackSlot<dyn for<'a> Fn(gatt::server::GattsEvent<'a>) -> u8 + Sync> =
    CallbackSlot::new();

static TAKEN: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sync() {
    debug!("NimBLE host sync");
    if let Some(cb) = HOST_CALLBACK.get() {
        cb(HostEvent::Sync);
    }
}

extern "C" fn on_reset(reason: c_int) {
    warn!("NimBLE host reset, reason {}", reason);
    if let Some(cb) = HOST_CALLBACK.get() {
        cb(HostEvent::Reset(reason));
    }
}

/// The connection event callback (wired at `adv_start`). NimBLE multiplexes
/// role-specific events onto it, so we **demux by role**: the role-agnostic
/// connection events go to the GAP hook, the server-role events
/// (`Subscribe`/`NotifyComplete`) to the GATTS hook.
pub(crate) unsafe extern "C" fn gap_event_cb(
    event: *mut sys::ble_gap_event,
    _arg: *mut c_void,
) -> c_int {
    let event = &*event;

    match event.type_ as u32 {
        #[cfg(feature = "gatt-server")]
        sys::BLE_GAP_EVENT_SUBSCRIBE | sys::BLE_GAP_EVENT_NOTIFY_TX => {
            if let (Some(cb), Some(event)) = (
                GATTS_CALLBACK.get(),
                gatt::server::GattsEvent::from_gap(event),
            ) {
                cb(event);
            }
            0
        }
        #[cfg(feature = "gatt-client")]
        sys::BLE_GAP_EVENT_NOTIFY_RX => {
            if let Some(cb) = gatt::client::GATTC_CALLBACK.get() {
                cb(gatt::client::GattcEvent::from_notify_rx(event));
            }
            0
        }
        _ => {
            if let Some(cb) = GAP_CALLBACK.get() {
                cb(gap::GapEvent::from(event))
            } else {
                0
            }
        }
    }
}

#[cfg(feature = "gatt-server")]
pub(crate) unsafe extern "C" fn gatts_register_cb(
    ctxt: *mut sys::ble_gatt_register_ctxt,
    _arg: *mut c_void,
) {
    if let Some(cb) = GATTS_CALLBACK.get() {
        // The registration events carry no reply; the status return is ignored
        cb(gatt::server::GattsEvent::Register(
            gatt::server::BleGattRegister::from(&*ctxt),
        ));
    }
}

/// The single access trampoline shared by *every* characteristic - NimBLE
/// dispatches reads and writes here, and we route them to the one
/// [`gatts_subscribe`](BleDriver::gatts_subscribe) hook, keyed by the
/// (globally unique) `attr_handle`.
#[cfg(feature = "gatt-server")]
pub(crate) unsafe extern "C" fn gatts_access_cb(
    conn_handle: u16,
    attr_handle: u16,
    ctxt: *mut sys::ble_gatt_access_ctxt,
    _arg: *mut c_void,
) -> c_int {
    let Some(cb) = GATTS_CALLBACK.get() else {
        return sys::BLE_ATT_ERR_UNLIKELY as c_int;
    };

    let mbuf = mbuf::Mbuf::from_raw((*ctxt).om);

    let event = match (*ctxt).op as u32 {
        sys::BLE_GATT_ACCESS_OP_READ_CHR => gatt::server::GattsEvent::Read {
            conn_handle,
            attr_handle,
            offset: (*ctxt).offset,
            reply: mbuf,
        },
        // Writes are always delivered whole and at offset 0 (NimBLE coalesces
        // long writes)
        sys::BLE_GATT_ACCESS_OP_WRITE_CHR => gatt::server::GattsEvent::Write {
            conn_handle,
            attr_handle,
            data: mbuf,
        },
        _ => return sys::BLE_ATT_ERR_UNLIKELY as c_int,
    };

    cb(event) as c_int
}

/// The BLE host driver: a singleton wrapping the NimBLE host stack.
///
/// Construct it with [`BleDriver::new`] (or, with a GATT service table, via
/// [`BleDriver::new_with_services`]), subscribe to the events you need, then
/// drive the stack by awaiting [`BleDriver::run`] with a controller. All
/// callbacks run from inside `run`'s poll.
///
/// `S` is the GATT service table (`()` for none); the driver keeps it alive
/// for as long as the C host may reference it.
pub struct BleDriver<S = ()> {
    // Kept alive (not read back) for as long as the C host may reference it
    _services: S,
}

impl BleDriver<()> {
    /// Initializes the NimBLE host stack (without a GATT service table).
    /// Errors if another driver instance exists or the stack fails to
    /// initialize.
    pub fn new() -> Result<Self, BleError> {
        Self::init(())?;

        Ok(Self { _services: () })
    }
}

#[cfg(feature = "gatt-server")]
impl<S> BleDriver<S>
where
    S: AsRef<[sys::ble_gatt_svc_def]>,
{
    /// Initializes the NimBLE host stack with a GATT service table: either a
    /// `&'static` one from the [`gatt_services!`](crate::gatt_services) macro,
    /// or a runtime-built [`BleGattServices`](gatt::server::BleGattServices).
    ///
    /// The attribute handles NimBLE assigns are reported through the
    /// [`Register`](gatt::server::GattsEvent::Register) events during host
    /// start, so [`gatts_subscribe`](Self::gatts_subscribe) before awaiting
    /// [`run`](Self::run).
    pub fn new_with_services(services: S) -> Result<Self, BleError> {
        Self::init(())?;

        unsafe {
            let cfg = core::ptr::addr_of_mut!(sys::ble_hs_cfg);
            (*cfg).gatts_register_cb = Some(gatts_register_cb);
        }

        let defs = services.as_ref();

        let result = unsafe {
            BleError::check(sys::ble_gatts_count_cfg(defs.as_ptr()))
                .and_then(|_| BleError::check(sys::ble_gatts_add_svcs(defs.as_ptr())))
        };

        if let Err(err) = result {
            drop(BleDriver { _services: () }); // deinit through the common path
            return Err(err);
        }

        Ok(Self {
            _services: services,
        })
    }
}

impl<S> BleDriver<S> {
    fn init(_: ()) -> Result<(), BleError> {
        if TAKEN
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(BleError::new(sys::BLE_HS_EALREADY as _));
        }

        if port::init().is_err() {
            TAKEN.store(false, Ordering::SeqCst);
            return Err(BleError::new(sys::BLE_HS_ENOMEM as _));
        }

        unsafe {
            // Pre-`run` single-threaded window: install the host callbacks
            let cfg = core::ptr::addr_of_mut!(sys::ble_hs_cfg);
            (*cfg).sync_cb = Some(on_sync);
            (*cfg).reset_cb = Some(on_reset);

            sys::ble_svc_gap_init();
            sys::ble_svc_gatt_init();
            ble_store_ram_init();
        }

        Ok(())
    }

    /// Subscribes to host events. The callback runs from inside [`Self::run`]'s
    /// poll and may freely call host APIs.
    pub fn host_subscribe(&self, callback: &'static (dyn Fn(HostEvent) + Sync)) {
        HOST_CALLBACK.set(Some(callback));
    }

    /// Drives the host stack over the given controller. Must be polled for the
    /// stack to make any progress; never returns under normal operation.
    ///
    /// Contract: do not block the task polling `run` with unrelated long
    /// operations - host API calls made from *other* tasks (or from callbacks)
    /// rely on `run` being responsive. Calls that send HCI commands stall the
    /// executor for the command-ack round-trip (µs-ms, bounded by the HCI
    /// command timeout).
    pub async fn run<C: NimbleController>(&self, controller: C) -> Result<Infallible, BleError> {
        let mut pump = pin!(hci::pump(&controller));

        // SAFETY: `pump` is pinned in this frame and `guard` (dropped before
        // it) deregisters it.
        let guard = unsafe {
            let ptr: *mut (dyn core::future::Future<Output = Infallible> + '_) =
                pump.as_mut().get_unchecked_mut();
            hci::register_pump(ptr)
        };

        let events = pin!(port::run_events());
        let timers = pin!(port::run_timers());
        // The pump is driven through the same registration used by blocked NPL
        // waits, so that polls never race. This arm re-registers the executor's
        // waker with the transport on every `run` poll, repairing any waker
        // "stolen" by an intervening pump-while-pending wait.
        let pump_arm = pin!(poll_fn(|cx| {
            hci::pump_manual(cx.waker());
            Poll::<Infallible>::Pending
        }));

        let never = select3(events, timers, pump_arm).await;

        drop(guard);
        match never {
            embassy_futures::select::Either3::First(never) => match never {},
            embassy_futures::select::Either3::Second(never) => match never {},
            embassy_futures::select::Either3::Third(never) => match never {},
        }
    }

    /// The device's identity address, ensuring one exists (generating a random
    /// static address if the controller has no public one).
    pub fn address(&self) -> Result<([u8; 6], u8), BleError> {
        let mut addr = [0; 6];
        let mut addr_type = 0;

        unsafe {
            BleError::check(sys::ble_hs_util_ensure_addr(0))?;
            BleError::check(sys::ble_hs_id_infer_auto(0, &mut addr_type))?;
            BleError::check(sys::ble_hs_id_copy_addr(
                addr_type,
                addr.as_mut_ptr(),
                core::ptr::null_mut(),
            ))?;
        }

        Ok((addr, addr_type))
    }

    /// Whether the host is synced with the controller (i.e. operational).
    pub fn synced(&self) -> bool {
        unsafe { sys::ble_hs_synced() != 0 }
    }
}

impl<S> Drop for BleDriver<S> {
    fn drop(&mut self) {
        unsafe {
            let cfg = core::ptr::addr_of_mut!(sys::ble_hs_cfg);
            (*cfg).sync_cb = None;
            (*cfg).reset_cb = None;
            #[cfg(feature = "gatt-server")]
            {
                (*cfg).gatts_register_cb = None;
            }
        }

        HOST_CALLBACK.set(None);
        GAP_CALLBACK.set(None);
        #[cfg(feature = "gatt-server")]
        GATTS_CALLBACK.set(None);
        #[cfg(feature = "gatt-client")]
        gatt::client::GATTC_CALLBACK.set(None);
        #[cfg(feature = "l2cap")]
        l2cap::L2CAP_CALLBACK.set(None);

        // `port::deinit` tears the whole host down (including the GATT service
        // registry), after which the C side no longer references `self.services`
        port::deinit();

        TAKEN.store(false, Ordering::SeqCst);
    }
}
