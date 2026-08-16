//! The HCI bridge between the NimBLE C host and a `bt-hci` controller.
//!
//! Outbound (host -> controller): the C host hands raw packets to the
//! `ble_transport_to_ll_*_impl` symbols below; they are copied into bounded
//! static channels (the C buffers are freed immediately) and drained by the
//! pump. Depths mirror the C-side pools, so `try_send` cannot fail for ACL and
//! can fail for commands only on a C-side accounting bug.
//!
//! Inbound (controller -> host): the pump reads packets and forwards them into
//! the host through `ble_transport_alloc_*` + `ble_transport_to_hs_*`.
//!
//! The pump is one future, created and owned by the driver's `run()` method,
//! but *registered globally* so that `ble_npl_sem_pend` (the HCI command-ack
//! wait) can manually poll it while a C caller is parked - the
//! "pump-while-pending" mechanism that makes the whole stack thread-free.

#[cfg(not(feature = "external-ll"))]
use core::ffi::{c_int, c_void};
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};

use bt_hci::data::AclPacket;
use bt_hci::transport::Transport;
use bt_hci::{ControllerToHostPacket, FromHciBytes, HostToControllerPacket, PacketKind, WriteHci};

use embassy_futures::join::join;
use embassy_futures::select::{select, Either};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;

use nimble_rs_sys as sys;

use crate::fmt::Bytes;

/// HCI command packet: 2-byte opcode + 1-byte length + up to 255 bytes params.
const CMD_PACKET_MAX: usize = 3 + 255;
/// ACL data packet: 4-byte header + the C-side per-buffer payload capacity.
const ACL_PACKET_MAX: usize = 4 + sys::MYNEWT_VAL_BLE_TRANSPORT_ACL_SIZE as usize;
/// HCI event packet: 2-byte header + up to 255 bytes params.
const EVT_PACKET_MAX: usize = 2 + 255;

const CMD_QUEUE_DEPTH: usize = 2;
const ACL_QUEUE_DEPTH: usize = sys::MYNEWT_VAL_BLE_TRANSPORT_ACL_FROM_HS_COUNT as usize;

type Packet<const N: usize> = heapless::Vec<u8, N>;

/// The controller abstraction `nimble-rs` runs on: any async
/// [`bt_hci::controller::Controller`] extended with raw command submission.
///
/// The extension is unavoidable for a C host: NimBLE emits commands as raw
/// runtime packets, while bt-hci's typed command path (`ControllerCmdSync`)
/// neither accepts raw packets nor lets the raw Command Complete bytes be
/// reconstructed from a typed response. Every practical controller speaks raw
/// HCI at the bottom, so implementing [`Self::write_cmd`] is always possible:
/// Transport-shaped controllers get it for free via [`ForTransport`]; native
/// ones (e.g. nrf-sdc) dispatch on the opcode.
pub trait NimbleController: bt_hci::controller::Controller {
    /// Sends a raw HCI command packet (opcode + length + parameters, no
    /// transport indicator byte) to the controller.
    fn write_cmd(&self, cmd_packet: &[u8]) -> impl Future<Output = Result<(), Self::Error>>;
}

/// A raw HCI command packet, made writable through a [`Transport`].
struct RawCmd<'a>(&'a [u8]);

impl WriteHci for RawCmd<'_> {
    #[inline(always)]
    fn size(&self) -> usize {
        self.0.len()
    }

    fn write_hci<W: embedded_io::Write>(&self, mut writer: W) -> Result<(), W::Error> {
        writer.write_all(self.0)
    }

    async fn write_hci_async<W: embedded_io_async::Write>(
        &self,
        mut writer: W,
    ) -> Result<(), W::Error> {
        writer.write_all(self.0).await
    }
}

impl HostToControllerPacket for RawCmd<'_> {
    const KIND: PacketKind = PacketKind::Cmd;
}

/// Adapts any [`bt_hci::transport::Transport`] (H4 UART, Linux HCI sockets,
/// USB, ESP VHCI, ...) into a [`NimbleController`].
///
/// Unlike `bt_hci::ExternalController`, no command-slot machinery is involved:
/// the NimBLE host does its own command flow control and ack matching.
pub struct ForTransport<T>(T);

impl<T: Transport> ForTransport<T> {
    /// Wraps the given transport.
    pub const fn new(transport: T) -> Self {
        Self(transport)
    }

    /// Returns the wrapped transport.
    pub fn release(self) -> T {
        self.0
    }
}

impl<T: Transport> embedded_io::ErrorType for ForTransport<T> {
    type Error = T::Error;
}

impl<T: Transport> bt_hci::controller::Controller for ForTransport<T> {
    async fn write_acl_data(
        &self,
        packet: &bt_hci::data::AclPacket<'_>,
    ) -> Result<(), Self::Error> {
        self.0.write(packet).await
    }

    async fn write_sync_data(
        &self,
        packet: &bt_hci::data::SyncPacket<'_>,
    ) -> Result<(), Self::Error> {
        self.0.write(packet).await
    }

    async fn write_iso_data(
        &self,
        packet: &bt_hci::data::IsoPacket<'_>,
    ) -> Result<(), Self::Error> {
        self.0.write(packet).await
    }

    async fn read<'a>(&self, buf: &'a mut [u8]) -> Result<ControllerToHostPacket<'a>, Self::Error> {
        self.0.read(buf).await
    }
}

impl<T: Transport> NimbleController for ForTransport<T> {
    async fn write_cmd(&self, cmd_packet: &[u8]) -> Result<(), Self::Error> {
        self.0.write(&RawCmd(cmd_packet)).await
    }
}

//
// Host -> controller: the `ble_transport_ll_*` implementation
//

static CMD_QUEUE: Channel<CriticalSectionRawMutex, Packet<CMD_PACKET_MAX>, CMD_QUEUE_DEPTH> =
    Channel::new();
static ACL_QUEUE: Channel<CriticalSectionRawMutex, Packet<ACL_PACKET_MAX>, ACL_QUEUE_DEPTH> =
    Channel::new();

#[cfg(not(feature = "external-ll"))]
#[no_mangle]
extern "C" fn ble_transport_ll_init() {}

#[cfg(not(feature = "external-ll"))]
#[no_mangle]
extern "C" fn ble_transport_ll_deinit() {
    while CMD_QUEUE.try_receive().is_ok() {}
    while ACL_QUEUE.try_receive().is_ok() {}
}

#[cfg(not(feature = "external-ll"))]
#[no_mangle]
unsafe extern "C" fn ble_transport_to_ll_cmd_impl(buf: *mut c_void) -> c_int {
    // Flat command buffer: [opcode: 2][len: 1][params: len]
    let len = 3 + *buf.cast::<u8>().add(2) as usize;

    let mut packet = Packet::new();
    unwrap!(packet.extend_from_slice(core::slice::from_raw_parts(buf.cast(), len)));

    sys::ble_transport_free(buf);

    if CMD_QUEUE.try_send(packet).is_err() {
        // Cannot happen: the host has at most `POOL_CMD_COUNT` (<= 2) command
        // buffers, and each is queued here at most once before being freed.
        error!("HCI cmd queue overflow");
        return sys::ble_error_codes_BLE_ERR_MEM_CAPACITY as _;
    }

    0
}

#[cfg(not(feature = "external-ll"))]
#[no_mangle]
unsafe extern "C" fn ble_transport_to_ll_acl_impl(om: *mut sys::os_mbuf) -> c_int {
    let mut packet = Packet::<ACL_PACKET_MAX>::new();

    // Flatten the mbuf chain
    let mut cur = om;
    while !cur.is_null() {
        let data = core::slice::from_raw_parts((*cur).om_data, (*cur).om_len as usize);
        if packet.extend_from_slice(data).is_err() {
            error!("outgoing ACL packet larger than the transport buffer");
            sys::os_mbuf_free_chain(om);
            return sys::ble_error_codes_BLE_ERR_MEM_CAPACITY as _;
        }
        cur = (*cur).om_next.sle_next;
    }

    sys::os_mbuf_free_chain(om);

    if ACL_QUEUE.try_send(packet).is_err() {
        // Cannot happen: depth equals the C-side `ACL_FROM_HS` pool count
        error!("HCI ACL queue overflow");
        return sys::ble_error_codes_BLE_ERR_MEM_CAPACITY as _;
    }

    0
}

#[cfg(not(feature = "external-ll"))]
#[no_mangle]
unsafe extern "C" fn ble_transport_to_ll_iso_impl(om: *mut sys::os_mbuf) -> c_int {
    // ISO is compiled out (`MYNEWT_VAL_BLE_ISO=0`)
    sys::os_mbuf_free_chain(om);
    sys::ble_error_codes_BLE_ERR_UNSUPPORTED as _
}

//
// Controller -> host
//

fn rx_dispatch(packet: &ControllerToHostPacket<'_>) {
    match packet {
        ControllerToHostPacket::Event(event) => {
            // Reconstruct the raw HCI event: [code: 1][len: 1][params]
            let code = event.kind.0;
            let params = event.data;

            // LE meta advertising reports are "discardable": controllers can
            // flood them faster than the host consumes them, and the dedicated
            // (larger) discardable pool sheds the overflow.
            const LE_META: u8 = 0x3e;
            const LE_ADV_RPT: u8 = 0x02;
            const LE_EXT_ADV_RPT: u8 = 0x0d;
            let discardable = code == LE_META
                && matches!(params.first(), Some(&LE_ADV_RPT) | Some(&LE_EXT_ADV_RPT));

            unsafe {
                let buf: *mut u8 = sys::ble_transport_alloc_evt(discardable as _).cast();
                if buf.is_null() {
                    if discardable {
                        trace!("dropping discardable HCI event, pool empty");
                    } else {
                        // Must not drop: a lost non-discardable event (e.g. a
                        // command ack) wedges the host. The pools are sized so
                        // that this cannot happen when the host keeps up.
                        error!("non-discardable HCI event dropped, pool empty");
                    }
                    return;
                }

                buf.write(code);
                buf.add(1).write(params.len() as u8);
                core::ptr::copy_nonoverlapping(params.as_ptr(), buf.add(2), params.len());

                sys::ble_transport_to_hs_evt_impl(buf.cast());
            }
        }
        ControllerToHostPacket::Acl(acl) => unsafe {
            let om = sys::ble_transport_alloc_acl_from_ll();
            if om.is_null() {
                warn!("incoming ACL packet dropped, msys pool empty");
                return;
            }

            // Serialize back to the raw wire form: [handle+flags: 2][len: 2][data]
            let mut raw = [0; ACL_PACKET_MAX];
            let size = acl.size();
            unwrap!(acl.write_hci(&mut raw[..size]), "ACL packet too large");

            if sys::os_mbuf_append(om, raw.as_ptr().cast(), size as _) != 0 {
                error!("incoming ACL packet dropped, mbuf append failed");
                sys::os_mbuf_free_chain(om);
                return;
            }

            sys::ble_transport_to_hs_acl_impl(om);
        },
        ControllerToHostPacket::Sync(_) | ControllerToHostPacket::Iso(_) => {
            trace!("ignoring sync/iso packet from controller");
        }
    }
}

/// The pump: drains the outbound queues into the controller and feeds inbound
/// packets into the host. Runs forever; owned (and primarily polled) by the
/// driver's `run()` future, and additionally polled from `ble_npl_sem_pend`
/// via [`pump_manual`].
pub(crate) async fn pump<C: NimbleController>(controller: &C) -> core::convert::Infallible {
    join(
        async {
            // TX: commands take priority over data
            loop {
                let result = match select(CMD_QUEUE.receive(), ACL_QUEUE.receive()).await {
                    Either::First(cmd) => {
                        trace!("HCI cmd -> controller: {}", Bytes(&cmd));
                        controller.write_cmd(&cmd).await
                    }
                    Either::Second(acl) => {
                        trace!("HCI ACL -> controller: {}", Bytes(&acl));
                        match AclPacket::from_hci_bytes_complete(&acl) {
                            Ok(packet) => controller.write_acl_data(&packet).await,
                            Err(_) => {
                                error!("malformed outgoing ACL packet");
                                continue;
                            }
                        }
                    }
                };

                if result.is_err() {
                    error!("HCI write failed");
                }
            }
        },
        async {
            // RX
            const RX_BUF: usize = if EVT_PACKET_MAX > ACL_PACKET_MAX {
                EVT_PACKET_MAX
            } else {
                ACL_PACKET_MAX
            } + 1;
            let mut buf = [0; RX_BUF];
            loop {
                match controller.read(&mut buf).await {
                    Ok(packet) => rx_dispatch(&packet),
                    Err(_) => error!("HCI read failed"),
                }
            }
        },
    )
    .await
    .0
}

//
// Global pump registration (for pump-while-pending)
//

struct PumpSlot {
    /// Type-erased `Pin<&mut dyn Future<Output = !>>` of the running pump.
    fut: Option<*mut dyn Future<Output = core::convert::Infallible>>,
    /// Poll-in-progress flag: prevents re-entrant polling (impossible by
    /// construction in a single-context setup, racy only across std threads).
    busy: bool,
}

// Guarded by the global critical section.
unsafe impl Send for PumpSlot {}

static PUMP: critical_section::Mutex<core::cell::RefCell<PumpSlot>> =
    critical_section::Mutex::new(core::cell::RefCell::new(PumpSlot {
        fut: None,
        busy: false,
    }));

/// Registers the driver's pump future for the lifetime of the returned guard.
///
/// # Safety
///
/// `fut` must stay pinned and valid until the guard is dropped.
pub(crate) unsafe fn register_pump(
    fut: *mut (dyn Future<Output = core::convert::Infallible> + '_),
) -> PumpGuard {
    // Erase the lifetime; validity for the guard's lifetime is the caller's
    // contract, and the guard removes the registration on drop.
    let fut: *mut dyn Future<Output = core::convert::Infallible> = core::mem::transmute(fut);
    critical_section::with(|cs| {
        let mut slot = PUMP.borrow_ref_mut(cs);
        debug_assert!(slot.fut.is_none());
        slot.fut = Some(fut);
        slot.busy = false;
    });
    PumpGuard
}

pub(crate) struct PumpGuard;

impl Drop for PumpGuard {
    fn drop(&mut self) {
        critical_section::with(|cs| {
            let mut slot = PUMP.borrow_ref_mut(cs);
            slot.fut = None;
            slot.busy = false;
        });
    }
}

/// Polls the registered pump once with the given waker. Called from the
/// driver's `run()` future (with the executor's waker) and from blocked NPL
/// waits (with a parker waker). Returns `false` if no pump is registered or a
/// poll is already in flight.
pub(crate) fn pump_manual(waker: &Waker) -> bool {
    let Some(fut) = critical_section::with(|cs| {
        let mut slot = PUMP.borrow_ref_mut(cs);
        if slot.busy {
            None
        } else if let Some(fut) = slot.fut {
            slot.busy = true;
            Some(fut)
        } else {
            None
        }
    }) else {
        return false;
    };

    let mut cx = Context::from_waker(waker);
    // SAFETY: `fut` is valid and pinned per `register_pump`'s contract, and
    // the `busy` flag guarantees exclusive access for the duration of the poll.
    match unsafe { Pin::new_unchecked(&mut *fut) }.poll(&mut cx) {
        Poll::Pending => (),
        Poll::Ready(never) => match never {},
    }

    critical_section::with(|cs| {
        PUMP.borrow_ref_mut(cs).busy = false;
    });

    true
}
