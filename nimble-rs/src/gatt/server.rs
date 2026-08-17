//! GATT server: the service table, its events, and server operations on the
//! [`BleDriver`].
//!
//! Modeled on the NimBLE GATT server API of `esp-idf-svc`
//! (`src/ble/gatt/server.rs`).

use enumset::EnumSet;

use nimble_rs_sys as sys;

use crate::mbuf::{mbuf_from_slice, Mbuf};
use crate::{BleDriver, BleError, BleUuid, ConnHandle};

use super::{flags_to_repr, AttrHandle, BleGattCharFlag};

/// A GATT-server event, delivered to the single
/// [`gatts_subscribe`](BleDriver::gatts_subscribe) hook.
///
/// There are no per-characteristic callbacks: NimBLE dispatches *every*
/// characteristic read and write through one shared trampoline, and they
/// arrive here as [`Read`](Self::Read) / [`Write`](Self::Write), keyed by the
/// globally-unique `attr_handle`. The [`Register`](Self::Register) variants
/// fire as the service table is registered (during host start, from inside
/// [`BleDriver::run`]'s first polls).
///
/// The hook returns the ATT status (`0` on success) for `Read`/`Write`; the
/// return is ignored for the others.
pub enum GattsEvent<'a> {
    Register(BleGattRegister),
    /// A peer is reading one of our characteristics; append the value to
    /// `reply`.
    ///
    /// Covers every ATT read, as well as *local* reads, which carry
    /// [`CONN_HANDLE_NONE`](crate::CONN_HANDLE_NONE) instead of a real
    /// connection.
    Read {
        conn_handle: ConnHandle,
        attr_handle: AttrHandle,
        /// Non-zero only for a long read (ATT Read Blob Request): the offset
        /// the peer is asking to continue from. **Always append the whole
        /// value regardless** - NimBLE slices `[offset..]` out of it itself.
        offset: u16,
        reply: Mbuf<'a>,
    },
    /// A peer wrote one of our characteristics. Covers every ATT write
    /// (Request, Command, Signed, coalesced long writes); local writes carry
    /// [`CONN_HANDLE_NONE`](crate::CONN_HANDLE_NONE).
    Write {
        conn_handle: ConnHandle,
        attr_handle: AttrHandle,
        data: Mbuf<'a>,
    },
    /// A peer's subscription state for one of our characteristics changed: it
    /// wrote the CCCD, the connection is going down, or a bond was restored -
    /// see `reason`. The `prev_*` / `cur_*` pairs give the edge.
    SubscriptionChanged {
        conn_handle: ConnHandle,
        attr_handle: AttrHandle,
        reason: SubscribeReason,
        prev_notify: bool,
        cur_notify: bool,
        prev_indicate: bool,
        cur_indicate: bool,
    },
    /// An indication/notification we sent completed (for an indication,
    /// `status` is the peer's confirmation result).
    NotifyComplete {
        conn_handle: ConnHandle,
        attr_handle: AttrHandle,
        indication: bool,
        status: i32,
    },
}

impl GattsEvent<'static> {
    /// Build the server-role `SubscriptionChanged` / `NotifyComplete` events
    /// from a raw GAP event. Returns `None` for any other event type. Called
    /// from the GAP trampoline's demux.
    pub(crate) fn from_gap(event: &sys::ble_gap_event) -> Option<Self> {
        let anon = &event.__bindgen_anon_1;

        match event.type_ as u32 {
            sys::BLE_GAP_EVENT_SUBSCRIBE => {
                let subscribe = unsafe { &anon.subscribe };
                Some(Self::SubscriptionChanged {
                    conn_handle: subscribe.conn_handle,
                    attr_handle: subscribe.attr_handle,
                    reason: SubscribeReason::from_raw(subscribe.reason),
                    prev_notify: subscribe.prev_notify() != 0,
                    cur_notify: subscribe.cur_notify() != 0,
                    prev_indicate: subscribe.prev_indicate() != 0,
                    cur_indicate: subscribe.cur_indicate() != 0,
                })
            }
            sys::BLE_GAP_EVENT_NOTIFY_TX => {
                let notify_tx = unsafe { &anon.notify_tx };
                Some(Self::NotifyComplete {
                    conn_handle: notify_tx.conn_handle,
                    attr_handle: notify_tx.attr_handle,
                    indication: notify_tx.indication() != 0,
                    status: notify_tx.status,
                })
            }
            _ => None,
        }
    }
}

/// Why a peer's subscription state changed (the `reason` of
/// [`GattsEvent::SubscriptionChanged`]).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum SubscribeReason {
    /// The peer wrote the characteristic's CCCD.
    Write,
    /// The connection is about to be terminated; the `cur_*` flags are
    /// therefore `false` - this is *not* the peer opting out.
    Term,
    /// A bond was restored from persistence and the subscription state came
    /// back with it.
    Restore,
    /// A reason code unknown to this version of the crate.
    Other(u8),
}

impl SubscribeReason {
    fn from_raw(reason: u8) -> Self {
        match reason as u32 {
            sys::BLE_GAP_SUBSCRIBE_REASON_WRITE => Self::Write,
            sys::BLE_GAP_SUBSCRIBE_REASON_TERM => Self::Term,
            sys::BLE_GAP_SUBSCRIBE_REASON_RESTORE => Self::Restore,
            _ => Self::Other(reason),
        }
    }
}

/// A GATT registration event (the payload of [`GattsEvent::Register`]).
/// Capture the value handles you need (matching on `uuid`) here.
pub enum BleGattRegister {
    Service {
        uuid: BleUuid,
        handle: AttrHandle,
    },
    Characteristic {
        uuid: BleUuid,
        def_handle: AttrHandle,
        val_handle: AttrHandle,
    },
    Descriptor {
        uuid: BleUuid,
        handle: AttrHandle,
    },
    Other,
}

impl From<&sys::ble_gatt_register_ctxt> for BleGattRegister {
    fn from(ctxt: &sys::ble_gatt_register_ctxt) -> Self {
        let anon = &ctxt.__bindgen_anon_1;

        match ctxt.op as u32 {
            sys::BLE_GATT_REGISTER_OP_SVC => {
                let svc = unsafe { &anon.svc };
                Self::Service {
                    uuid: unsafe { BleUuid::from_raw((*svc.svc_def).uuid) },
                    handle: svc.handle,
                }
            }
            sys::BLE_GATT_REGISTER_OP_CHR => {
                let chr = unsafe { &anon.chr };
                Self::Characteristic {
                    uuid: unsafe { BleUuid::from_raw((*chr.chr_def).uuid) },
                    def_handle: chr.def_handle,
                    val_handle: chr.val_handle,
                }
            }
            sys::BLE_GATT_REGISTER_OP_DSC => {
                let dsc = unsafe { &anon.dsc };
                Self::Descriptor {
                    uuid: unsafe { BleUuid::from_raw((*dsc.dsc_def).uuid) },
                    handle: dsc.handle,
                }
            }
            _ => Self::Other,
        }
    }
}

/// GATT-server operations on the [`BleDriver`], available when the driver was
/// built with a service table via
/// [`new_with_services`](BleDriver::new_with_services). `&self`, so callable
/// re-entrantly.
impl<S> BleDriver<S>
where
    S: AsRef<[sys::ble_gatt_svc_def]>,
{
    /// Subscribe to GATT-server events ([`GattsEvent`]). Set this **before**
    /// awaiting [`BleDriver::run`]: the `Register` events (carrying the
    /// attribute handles NimBLE assigned) fire during host start.
    pub fn gatts_subscribe(
        &self,
        callback: &'static (dyn for<'a> Fn(GattsEvent<'a>) -> u8 + Sync),
    ) {
        critical_section::with(|_| crate::GATTS_CALLBACK.0.set(Some(callback)));
    }

    /// Stop delivering GATT-server events to the subscribed hook.
    pub fn gatts_unsubscribe(&self) {
        critical_section::with(|_| crate::GATTS_CALLBACK.0.set(None));
    }

    /// Send a "free-form" characteristic indication to `conn_handle`.
    pub fn indicate(
        &self,
        conn_handle: ConnHandle,
        val_handle: AttrHandle,
        data: &[u8],
    ) -> Result<(), BleError> {
        let om = mbuf_from_slice(data)?;

        // `ble_gatts_indicate_custom` takes ownership of `om` and frees it on
        // all paths (no leak, no double-free).
        BleError::check(unsafe { sys::ble_gatts_indicate_custom(conn_handle, val_handle, om) })
    }

    /// Send a characteristic notification to `conn_handle`.
    pub fn notify(
        &self,
        conn_handle: ConnHandle,
        val_handle: AttrHandle,
        data: &[u8],
    ) -> Result<(), BleError> {
        let om = mbuf_from_slice(data)?;

        // Takes ownership of `om`, as above
        BleError::check(unsafe { sys::ble_gatts_notify_custom(conn_handle, val_handle, om) })
    }
}

//
// Runtime (heap-allocated) service tables
//

/// A characteristic in a [`BleGattService`] - just its UUID and flags. Reads
/// and writes are serviced by the single
/// [`gatts_subscribe`](BleDriver::gatts_subscribe) hook (dispatched by the
/// value handle reported via [`BleGattRegister`]).
#[derive(Clone, Copy)]
pub struct BleGattCharacteristic {
    uuid: BleUuid,
    flags: EnumSet<BleGattCharFlag>,
}

impl BleGattCharacteristic {
    pub fn new(uuid: BleUuid, flags: EnumSet<BleGattCharFlag>) -> Self {
        Self { uuid, flags }
    }
}

/// A GATT service definition, borrowing its characteristics from the caller
/// (an array, a `heapless::Vec`, ... - the borrow only needs to outlive
/// [`BleGattServices::new`], which copies everything it keeps).
pub struct BleGattService<'a> {
    primary: bool,
    uuid: BleUuid,
    characteristics: &'a [BleGattCharacteristic],
}

impl<'a> BleGattService<'a> {
    pub fn new(primary: bool, uuid: BleUuid, characteristics: &'a [BleGattCharacteristic]) -> Self {
        Self {
            primary,
            uuid,
            characteristics,
        }
    }
}

/// A GATT service table built **at runtime**, as the raw NimBLE
/// `ble_gatt_svc_def` tree, ready to hand to
/// [`BleDriver::new_with_services`]. For a **compile-time** table, see the
/// [`gatt_services!`](crate::gatt_services) macro.
///
/// The table lives in exact-size allocations on the platform C heap (the
/// same `nimble_platform_mem_*` backend the host itself uses - no Rust
/// allocator involved): one flat array of characteristic copies (the UUID
/// storage), one flat array of `ble_gatt_chr_def`s with a terminator per
/// service, the service UUIDs, and the `ble_gatt_svc_def` array. The def
/// arrays hold raw pointers into the sibling allocations; nothing moves, so
/// the pointer graph stays valid for as long as this struct lives.
pub struct BleGattServices {
    _chars: crate::mem::CSlice<BleGattCharacteristic>,
    _svc_uuids: crate::mem::CSlice<BleUuid>,
    _chr_defs: crate::mem::CSlice<sys::ble_gatt_chr_def>,
    svc_defs: crate::mem::CSlice<sys::ble_gatt_svc_def>,
}

impl BleGattServices {
    /// Builds the table, copying the borrowed definitions into owned C-heap
    /// storage. Fails with `BLE_HS_ENOMEM` if the C heap is exhausted.
    pub fn new(services: &[BleGattService<'_>]) -> Result<Self, crate::BleError> {
        use crate::mem::CSlice;

        let total_chars = services.iter().map(|s| s.characteristics.len()).sum();

        let mut chars = CSlice::<BleGattCharacteristic>::new_zeroed(total_chars)?;
        let mut svc_uuids = CSlice::<BleUuid>::new_zeroed(services.len())?;
        // One terminator (zeroed entry) after each service's characteristics
        let mut chr_defs =
            CSlice::<sys::ble_gatt_chr_def>::new_zeroed(total_chars + services.len())?;
        let mut svc_defs = CSlice::<sys::ble_gatt_svc_def>::new_zeroed(services.len() + 1)?;

        let mut char_at = 0;
        let mut def_at = 0;

        for (svc_at, service) in services.iter().enumerate() {
            svc_uuids[svc_at] = service.uuid;

            svc_defs[svc_at] = sys::ble_gatt_svc_def {
                type_: if service.primary {
                    sys::BLE_GATT_SVC_TYPE_PRIMARY as u8
                } else {
                    sys::BLE_GATT_SVC_TYPE_SECONDARY as u8
                },
                uuid: svc_uuids[svc_at].as_ptr(),
                includes: core::ptr::null_mut(),
                characteristics: unsafe { chr_defs.as_ptr().add(def_at) },
            };

            for chr in service.characteristics {
                chars[char_at] = *chr;

                chr_defs[def_at] = sys::ble_gatt_chr_def {
                    uuid: chars[char_at].uuid.as_ptr(),
                    // The one trampoline for every characteristic;
                    // `attr_handle` disambiguates.
                    access_cb: Some(crate::gatts_access_cb),
                    arg: core::ptr::null_mut(),
                    flags: flags_to_repr(chr.flags),
                    // Handles are captured from the registration event
                    val_handle: core::ptr::null_mut(),
                    ..Default::default()
                };

                char_at += 1;
                def_at += 1;
            }

            // The per-service terminator: already zeroed by the allocation
            def_at += 1;
        }
        // ... as is the final `svc_defs` terminator entry

        Ok(Self {
            _chars: chars,
            _svc_uuids: svc_uuids,
            _chr_defs: chr_defs,
            svc_defs,
        })
    }
}

impl AsRef<[sys::ble_gatt_svc_def]> for BleGattServices {
    fn as_ref(&self) -> &[sys::ble_gatt_svc_def] {
        &self.svc_defs
    }
}

//
// Static (compile-time) service tables - see the `gatt_services!` macro.
//

/// A **static** (compile-time) GATT service table: a null-terminated
/// `ble_gatt_svc_def` array wrapped so it can live in a `static`. Build one
/// with the [`gatt_services!`](crate::gatt_services) macro and pass `&NAME` to
/// [`BleDriver::new_with_services`] - it needs no heap and lands in flash.
#[repr(transparent)]
pub struct GattServices<const N: usize>([sys::ble_gatt_svc_def; N]);

// SAFETY: the raw pointers in the table only address other items of the same
// `'static` tree, and NimBLE consumes the table read-only.
unsafe impl<const N: usize> Sync for GattServices<N> {}

impl<const N: usize> GattServices<N> {
    /// Wrap a fully-built, null-terminated service array. Prefer the
    /// [`gatt_services!`](crate::gatt_services) macro.
    #[doc(hidden)]
    pub const fn new(defs: [sys::ble_gatt_svc_def; N]) -> Self {
        Self(defs)
    }
}

impl<const N: usize> AsRef<[sys::ble_gatt_svc_def]> for GattServices<N> {
    fn as_ref(&self) -> &[sys::ble_gatt_svc_def] {
        &self.0
    }
}

/// The characteristics of one service, wrapped for `static` storage. An
/// implementation detail of [`gatt_services!`](crate::gatt_services).
#[doc(hidden)]
#[repr(transparent)]
pub struct GattChrs<const N: usize>([sys::ble_gatt_chr_def; N]);

// SAFETY: as for `GattServices`.
unsafe impl<const N: usize> Sync for GattChrs<N> {}

impl<const N: usize> GattChrs<N> {
    pub const fn new(defs: [sys::ble_gatt_chr_def; N]) -> Self {
        Self(defs)
    }

    pub const fn as_ptr(&self) -> *const sys::ble_gatt_chr_def {
        self.0.as_ptr()
    }
}

// All-zero templates: a zeroed `ble_gatt_*_def` is exactly the null
// terminator, and the base the builders below fill in, so fields we do not
// set are zeroed without enumerating them.
const ZERO_SVC: sys::ble_gatt_svc_def = unsafe { core::mem::MaybeUninit::zeroed().assume_init() };
const ZERO_CHR: sys::ble_gatt_chr_def = unsafe { core::mem::MaybeUninit::zeroed().assume_init() };

/// The service-array terminator. Public only for the macro.
#[doc(hidden)]
pub const SVC_SENTINEL: sys::ble_gatt_svc_def = ZERO_SVC;

/// The characteristic-array terminator. Public only for the macro.
#[doc(hidden)]
pub const CHR_SENTINEL: sys::ble_gatt_chr_def = ZERO_CHR;

/// Build one characteristic def for a static table. Public only for the macro.
#[doc(hidden)]
pub const fn make_chr(
    uuid: *const sys::ble_uuid_t,
    flags: sys::ble_gatt_chr_flags,
) -> sys::ble_gatt_chr_def {
    sys::ble_gatt_chr_def {
        uuid,
        access_cb: Some(crate::gatts_access_cb),
        flags,
        ..ZERO_CHR
    }
}

/// Build one service def for a static table. Public only for the macro.
#[doc(hidden)]
pub const fn make_svc(
    primary: bool,
    uuid: *const sys::ble_uuid_t,
    characteristics: *const sys::ble_gatt_chr_def,
) -> sys::ble_gatt_svc_def {
    sys::ble_gatt_svc_def {
        type_: if primary {
            sys::BLE_GATT_SVC_TYPE_PRIMARY as u8
        } else {
            sys::BLE_GATT_SVC_TYPE_SECONDARY as u8
        },
        uuid,
        characteristics,
        ..ZERO_SVC
    }
}

/// Define a **static** (compile-time, heap-free) GATT service table.
///
/// Expands to a `static NAME` holding the null-terminated NimBLE
/// `ble_gatt_svc_def` tree (services, characteristics, UUIDs). Pass `&NAME` to
/// [`BleDriver::new_with_services`](crate::BleDriver::new_with_services).
/// Reads and writes are serviced through the single
/// [`gatts_subscribe`](crate::BleDriver::gatts_subscribe) hook, keyed by the
/// value handle reported in the `Register` events.
///
/// ```ignore
/// use nimble_rs::BleUuid;
/// use nimble_rs::gatt_services;
///
/// const SVC: BleUuid = BleUuid::uuid128(0xad91b201_73474047_9e173bed_82d75f9d);
/// const RECV: BleUuid = BleUuid::uuid128(0xb6fccb50_87be44f3_ae22f854_85ea42c4);
/// const HR: BleUuid = BleUuid::uuid16(0x2A37);
///
/// gatt_services!(SERVICES {
///     primary(SVC) {
///         chr(RECV, Write);
///         chr(HR, Notify | Indicate);
///     }
/// });
/// // ... let driver = BleDriver::new_with_services(&SERVICES)?;
/// ```
#[macro_export]
macro_rules! gatt_services {
    // --- internal helpers (matched before the public arm) ---

    // a `*const ble_uuid_t` backed by a fresh block-scoped `'static` (stable address)
    (@uuid_ptr $uuid:expr) => {{
        static U: $crate::BleUuid = $uuid;
        U.as_ptr()
    }};

    // one characteristic def
    (@chr $uuid:expr, $($flag:ident)|+ ) => {
        $crate::gatt::server::make_chr(
            $crate::gatt_services!(@uuid_ptr $uuid),
            0 $( | $crate::gatt::BleGattCharFlag::$flag.repr() )+,
        )
    };

    // map `primary`/`secondary` to a bool; a unit token used only for counting
    (@primary primary) => { true };
    (@primary secondary) => { false };
    (@unit $_t:expr) => { () };

    // --- public entry ---
    (
        $vis:vis $NAME:ident {
            $(
                $kind:ident ( $svc_uuid:expr ) {
                    $( chr ( $chr_uuid:expr, $($flag:ident)|+ ) ; )*
                }
            )+
        }
    ) => {
        $vis static $NAME: $crate::gatt::server::GattServices<
            { <[()]>::len(&[ $( $crate::gatt_services!(@unit $svc_uuid) ),+ ]) + 1 }
        > = $crate::gatt::server::GattServices::new([
            $(
                {
                    static CHRS: $crate::gatt::server::GattChrs<
                        { <[()]>::len(&[ $( $crate::gatt_services!(@unit $chr_uuid) ),* ]) + 1 }
                    > = $crate::gatt::server::GattChrs::new([
                        $( $crate::gatt_services!(@chr $chr_uuid, $($flag)|+), )*
                        $crate::gatt::server::CHR_SENTINEL
                    ]);
                    $crate::gatt::server::make_svc(
                        $crate::gatt_services!(@primary $kind),
                        $crate::gatt_services!(@uuid_ptr $svc_uuid),
                        CHRS.as_ptr(),
                    )
                },
            )+
            $crate::gatt::server::SVC_SENTINEL
        ]);
    };
}
