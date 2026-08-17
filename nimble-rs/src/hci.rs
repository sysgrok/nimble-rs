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

use bt_hci::cmd::controller_baseband::{Reset, SetEventMask, SetEventMaskPage2};
use bt_hci::cmd::info::{
    ReadBdAddr, ReadLocalSupportedCmds, ReadLocalSupportedFeatures, ReadLocalVersionInformation,
};
use bt_hci::cmd::le::{
    LeAddDeviceToResolvingList, LeClearResolvingList, LeRand, LeReadBufferSize,
    LeReadLocalSupportedFeatures, LeRemoveDeviceFromResolvingList, LeSetAddrResolutionEnable,
    LeSetAdvEnable, LeSetEventMask, LeSetPrivacyMode, LeSetRandomAddr,
    LeSetResolvablePrivateAddrTimeout,
};
use bt_hci::cmd::{Cmd, SyncCmd};
use bt_hci::controller::ControllerCmdSync;
use bt_hci::data::AclPacket;
use bt_hci::{ControllerToHostPacket, FromHciBytes, WriteHci};

#[cfg(any(feature = "central", feature = "peripheral"))]
use bt_hci::cmd::le::{
    LeAddDeviceToFilterAcceptList, LeClearFilterAcceptList, LeConnUpdate, LeReadChannelMap,
    LeReadFilterAcceptListSize, LeReadRemoteFeatures, LeReadSuggestedDefaultDataLength,
    LeRemoveDeviceFromFilterAcceptList, LeSetDataLength, LeSetHostChannelClassification,
    LeWriteSuggestedDefaultDataLength,
};
#[cfg(any(feature = "central", feature = "peripheral"))]
use bt_hci::cmd::link_control::{Disconnect, ReadRemoteVersionInformation};
#[cfg(any(feature = "central", feature = "peripheral"))]
use bt_hci::cmd::status::ReadRssi;
#[cfg(any(feature = "central", feature = "peripheral"))]
use bt_hci::cmd::AsyncCmd;
#[cfg(any(feature = "central", feature = "peripheral"))]
use bt_hci::controller::ControllerCmdAsync;

#[cfg(feature = "broadcaster")]
use bt_hci::cmd::le::{
    LeReadAdvPhysicalChannelTxPower, LeSetAdvData, LeSetAdvParams, LeSetScanResponseData,
};

#[cfg(feature = "observer")]
use bt_hci::cmd::le::{LeSetScanEnable, LeSetScanParams};

#[cfg(feature = "central")]
use bt_hci::cmd::le::{LeCreateConn, LeCreateConnCancel};

#[cfg(any(feature = "sm", feature = "sm-sc-only"))]
use bt_hci::cmd::le::{
    LeEnableEncryption, LeLongTermKeyRequestNegativeReply, LeLongTermKeyRequestReply,
};

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

//
// The controller contract: stock bt-hci traits only, aggregated - like
// trouble-host's `Controller` - into one alias trait with a blanket impl.
// Nothing bespoke to implement: `bt_hci::controller::ExternalController`
// (for transports) and native controllers such as nrf-sdc satisfy it out of
// the box for the commands their configuration needs.
//

/// Commands every configuration sends (startup, identity, privacy).
pub trait CoreCmds:
    bt_hci::controller::Controller
    + ControllerCmdSync<Reset>
    + ControllerCmdSync<SetEventMask>
    + ControllerCmdSync<SetEventMaskPage2>
    + ControllerCmdSync<LeSetEventMask>
    + ControllerCmdSync<ReadLocalVersionInformation>
    + ControllerCmdSync<ReadLocalSupportedCmds>
    + ControllerCmdSync<ReadLocalSupportedFeatures>
    + ControllerCmdSync<ReadBdAddr>
    + ControllerCmdSync<LeReadBufferSize>
    + ControllerCmdSync<LeReadLocalSupportedFeatures>
    + ControllerCmdSync<LeRand>
    + ControllerCmdSync<LeSetRandomAddr>
    + ControllerCmdSync<LeSetAdvEnable>
    + ControllerCmdSync<LeSetAddrResolutionEnable>
    + ControllerCmdSync<LeClearResolvingList>
    + ControllerCmdSync<LeAddDeviceToResolvingList>
    + ControllerCmdSync<LeRemoveDeviceFromResolvingList>
    + ControllerCmdSync<LeSetPrivacyMode>
    + ControllerCmdSync<LeSetResolvablePrivateAddrTimeout>
{
}

impl<
        C: bt_hci::controller::Controller
            + ControllerCmdSync<Reset>
            + ControllerCmdSync<SetEventMask>
            + ControllerCmdSync<SetEventMaskPage2>
            + ControllerCmdSync<LeSetEventMask>
            + ControllerCmdSync<ReadLocalVersionInformation>
            + ControllerCmdSync<ReadLocalSupportedCmds>
            + ControllerCmdSync<ReadLocalSupportedFeatures>
            + ControllerCmdSync<ReadBdAddr>
            + ControllerCmdSync<LeReadBufferSize>
            + ControllerCmdSync<LeReadLocalSupportedFeatures>
            + ControllerCmdSync<LeRand>
            + ControllerCmdSync<LeSetRandomAddr>
            + ControllerCmdSync<LeSetAdvEnable>
            + ControllerCmdSync<LeSetAddrResolutionEnable>
            + ControllerCmdSync<LeClearResolvingList>
            + ControllerCmdSync<LeAddDeviceToResolvingList>
            + ControllerCmdSync<LeRemoveDeviceFromResolvingList>
            + ControllerCmdSync<LeSetPrivacyMode>
            + ControllerCmdSync<LeSetResolvablePrivateAddrTimeout>,
    > CoreCmds for C
{
}

/// Connection management commands (any connectable/initiating role).
///
/// Deliberately absent: the LE Remote Connection Parameter Request
/// reply/negative-reply pair. nrf-sdc's serialized API does not expose them
/// at all, and the host sends them only in response to the (equally rare)
/// LL parameter-request event - where the dispatcher's unknown-command ack
/// declines the request gracefully. Revisit if the ecosystem grows the
/// impls.
#[cfg(any(feature = "central", feature = "peripheral"))]
pub trait ConnCmds:
    bt_hci::controller::Controller
    + ControllerCmdSync<Disconnect>
    + ControllerCmdAsync<LeConnUpdate>
    + ControllerCmdAsync<LeReadRemoteFeatures>
    + ControllerCmdAsync<ReadRemoteVersionInformation>
    + ControllerCmdSync<LeSetDataLength>
    + ControllerCmdSync<LeReadSuggestedDefaultDataLength>
    + ControllerCmdSync<LeWriteSuggestedDefaultDataLength>
    + ControllerCmdSync<LeReadChannelMap>
    + ControllerCmdSync<LeSetHostChannelClassification>
    + ControllerCmdSync<ReadRssi>
    + ControllerCmdSync<LeAddDeviceToFilterAcceptList>
    + ControllerCmdSync<LeRemoveDeviceFromFilterAcceptList>
    + ControllerCmdSync<LeClearFilterAcceptList>
    + ControllerCmdSync<LeReadFilterAcceptListSize>
{
}

#[cfg(any(feature = "central", feature = "peripheral"))]
impl<
        C: bt_hci::controller::Controller
            + ControllerCmdSync<Disconnect>
            + ControllerCmdAsync<LeConnUpdate>
            + ControllerCmdAsync<LeReadRemoteFeatures>
            + ControllerCmdAsync<ReadRemoteVersionInformation>
            + ControllerCmdSync<LeSetDataLength>
            + ControllerCmdSync<LeReadSuggestedDefaultDataLength>
            + ControllerCmdSync<LeWriteSuggestedDefaultDataLength>
            + ControllerCmdSync<LeReadChannelMap>
            + ControllerCmdSync<LeSetHostChannelClassification>
            + ControllerCmdSync<ReadRssi>
            + ControllerCmdSync<LeAddDeviceToFilterAcceptList>
            + ControllerCmdSync<LeRemoveDeviceFromFilterAcceptList>
            + ControllerCmdSync<LeClearFilterAcceptList>
            + ControllerCmdSync<LeReadFilterAcceptListSize>,
    > ConnCmds for C
{
}

/// Auto-implemented when no connectable/initiating role is enabled.
#[cfg(not(any(feature = "central", feature = "peripheral")))]
pub trait ConnCmds: bt_hci::controller::Controller {}
#[cfg(not(any(feature = "central", feature = "peripheral")))]
impl<C: bt_hci::controller::Controller> ConnCmds for C {}

/// Advertising commands (`broadcaster`; enable itself is in [`CoreCmds`]).
#[cfg(feature = "broadcaster")]
pub trait AdvCmds:
    bt_hci::controller::Controller
    + ControllerCmdSync<LeSetAdvParams>
    + ControllerCmdSync<LeSetAdvData>
    + ControllerCmdSync<LeSetScanResponseData>
    + ControllerCmdSync<LeReadAdvPhysicalChannelTxPower>
{
}

#[cfg(feature = "broadcaster")]
impl<
        C: bt_hci::controller::Controller
            + ControllerCmdSync<LeSetAdvParams>
            + ControllerCmdSync<LeSetAdvData>
            + ControllerCmdSync<LeSetScanResponseData>
            + ControllerCmdSync<LeReadAdvPhysicalChannelTxPower>,
    > AdvCmds for C
{
}

/// Auto-implemented when `broadcaster` is not enabled.
#[cfg(not(feature = "broadcaster"))]
pub trait AdvCmds: bt_hci::controller::Controller {}
#[cfg(not(feature = "broadcaster"))]
impl<C: bt_hci::controller::Controller> AdvCmds for C {}

/// Scanning commands (`observer`).
#[cfg(feature = "observer")]
pub trait ScanCmds:
    bt_hci::controller::Controller
    + ControllerCmdSync<LeSetScanParams>
    + ControllerCmdSync<LeSetScanEnable>
{
}

#[cfg(feature = "observer")]
impl<
        C: bt_hci::controller::Controller
            + ControllerCmdSync<LeSetScanParams>
            + ControllerCmdSync<LeSetScanEnable>,
    > ScanCmds for C
{
}

/// Auto-implemented when `observer` is not enabled.
#[cfg(not(feature = "observer"))]
pub trait ScanCmds: bt_hci::controller::Controller {}
#[cfg(not(feature = "observer"))]
impl<C: bt_hci::controller::Controller> ScanCmds for C {}

/// Connection initiation commands (`central`).
#[cfg(feature = "central")]
pub trait CentralCmds:
    bt_hci::controller::Controller
    + ControllerCmdAsync<LeCreateConn>
    + ControllerCmdSync<LeCreateConnCancel>
{
}

#[cfg(feature = "central")]
impl<
        C: bt_hci::controller::Controller
            + ControllerCmdAsync<LeCreateConn>
            + ControllerCmdSync<LeCreateConnCancel>,
    > CentralCmds for C
{
}

/// Auto-implemented when `central` is not enabled.
#[cfg(not(feature = "central"))]
pub trait CentralCmds: bt_hci::controller::Controller {}
#[cfg(not(feature = "central"))]
impl<C: bt_hci::controller::Controller> CentralCmds for C {}

/// Security Manager commands (`sm`/`sm-sc-only`).
#[cfg(any(feature = "sm", feature = "sm-sc-only"))]
pub trait SmCmds:
    bt_hci::controller::Controller
    + ControllerCmdAsync<LeEnableEncryption>
    + ControllerCmdSync<LeLongTermKeyRequestReply>
    + ControllerCmdSync<LeLongTermKeyRequestNegativeReply>
{
}

#[cfg(any(feature = "sm", feature = "sm-sc-only"))]
impl<
        C: bt_hci::controller::Controller
            + ControllerCmdAsync<LeEnableEncryption>
            + ControllerCmdSync<LeLongTermKeyRequestReply>
            + ControllerCmdSync<LeLongTermKeyRequestNegativeReply>,
    > SmCmds for C
{
}

/// Auto-implemented when no Security Manager feature is enabled.
#[cfg(not(any(feature = "sm", feature = "sm-sc-only")))]
pub trait SmCmds: bt_hci::controller::Controller {}
#[cfg(not(any(feature = "sm", feature = "sm-sc-only")))]
impl<C: bt_hci::controller::Controller> SmCmds for C {}

/// The controller `nimble-rs` runs on: a stock async
/// [`bt_hci::controller::Controller`] implementing the standard typed-command
/// traits for the commands the enabled features make the host emit. Blanket
/// implemented - there is nothing to implement by hand.
pub trait Controller:
    bt_hci::controller::Controller + CoreCmds + ConnCmds + AdvCmds + ScanCmds + CentralCmds + SmCmds
{
}

impl<
        C: bt_hci::controller::Controller
            + CoreCmds
            + ConnCmds
            + AdvCmds
            + ScanCmds
            + CentralCmds
            + SmCmds,
    > Controller for C
{
}

//
// Raw -> typed command dispatch
//
// The C host emits commands as raw packets; each is parsed into its typed
// bt-hci command (`Params: FromHciBytes` + the generated `From<Params>`),
// executed through `ControllerCmdSync`/`ControllerCmdAsync`, and the ack the
// host expects (Command Complete / Command Status) is synthesized back from
// the typed result (`Return: WriteHci`) and fed into the host's RX path.
//

/// `Unknown HCI Command` / `Invalid HCI Command Parameters` / `Hardware
/// Failure` - synthesized ack statuses for commands that never reach the
/// controller.
const STATUS_UNKNOWN_CMD: u8 = 0x01;
const STATUS_INVALID_PARAMS: u8 = 0x12;
const STATUS_HW_FAILURE: u8 = 0x03;

fn ack_status<E>(result: &Result<(), bt_hci::cmd::Error<E>>) -> u8 {
    match result {
        Ok(()) => 0,
        Err(bt_hci::cmd::Error::Hci(e)) => e.to_status().into_inner(),
        Err(bt_hci::cmd::Error::Io(_)) => {
            error!("HCI command I/O failed");
            STATUS_HW_FAILURE
        }
    }
}

/// Synthesizes a Command Complete event: `[0x0e][plen][num_pkts=1][opcode]
/// [status][return params]`.
fn complete_ack<R: WriteHci>(opcode: u16, status: u8, ret: Option<&R>) -> Packet<EVT_PACKET_MAX> {
    let mut evt = Packet::new();
    unwrap!(evt.push(0x0e).ok());
    unwrap!(evt.push(0).ok()); // patched below
    unwrap!(evt.push(1).ok());
    unwrap!(evt.extend_from_slice(&opcode.to_le_bytes()).ok());
    unwrap!(evt.push(status).ok());

    if let Some(ret) = ret {
        let at = evt.len();
        unwrap!(evt.resize_default(at + ret.size()).ok());
        unwrap!(
            ret.write_hci(&mut evt[at..]).ok(),
            "return params too large"
        );
    }

    evt[1] = (evt.len() - 2) as u8;
    evt
}

/// The no-return-params case of [`complete_ack`].
fn plain_ack(opcode: u16, status: u8) -> Packet<EVT_PACKET_MAX> {
    complete_ack::<()>(opcode, status, None)
}

/// Synthesizes a Command Status event: `[0x0f][4][status][num_pkts=1][opcode]`.
#[cfg(any(feature = "central", feature = "peripheral"))]
fn status_ack(opcode: u16, status: u8) -> Packet<EVT_PACKET_MAX> {
    let mut evt = Packet::new();
    unwrap!(evt
        .extend_from_slice(&[
            0x0f,
            4,
            status,
            1,
            opcode.to_le_bytes()[0],
            opcode.to_le_bytes()[1]
        ])
        .ok());
    evt
}

/// Executes one sync command: raw params -> typed -> `exec` -> synthesized
/// Command Complete.
async fn sync_cmd<C, T>(controller: &C, opcode: u16, params: &[u8])
where
    C: ControllerCmdSync<T>,
    T: SyncCmd + From<<T as Cmd>::Params>,
    for<'de> <T as Cmd>::Params: FromHciBytes<'de>,
    <T as SyncCmd>::Return: WriteHci,
{
    let Ok((params, _)) = <T as Cmd>::Params::from_hci_bytes(params) else {
        error!("malformed HCI command params, opcode {:04x}", opcode);
        let evt = plain_ack(opcode, STATUS_INVALID_PARAMS);
        feed_event(evt[0], &evt[2..], false);
        return;
    };

    let evt = match controller.exec(&T::from(params)).await {
        Ok(ret) => complete_ack(opcode, 0, Some(&ret)),
        Err(e) => plain_ack(opcode, ack_status::<C::Error>(&Err(e))),
    };

    feed_event(evt[0], &evt[2..], false);
}

/// Executes one async command: raw params -> typed -> `exec` -> synthesized
/// Command Status. (Subsequent completion events arrive via the RX pump.)
#[cfg(any(feature = "central", feature = "peripheral"))]
async fn async_cmd<C, T>(controller: &C, opcode: u16, params: &[u8])
where
    C: ControllerCmdAsync<T>,
    T: AsyncCmd + From<<T as Cmd>::Params>,
    for<'de> <T as Cmd>::Params: FromHciBytes<'de>,
{
    let Ok((params, _)) = <T as Cmd>::Params::from_hci_bytes(params) else {
        error!("malformed HCI command params, opcode {:04x}", opcode);
        let evt = status_ack(opcode, STATUS_INVALID_PARAMS);
        feed_event(evt[0], &evt[2..], false);
        return;
    };

    let result = controller.exec(&T::from(params)).await;
    let evt = status_ack(opcode, ack_status(&result));
    feed_event(evt[0], &evt[2..], false);
}

/// Dispatches one raw command packet from the C host.
async fn send_cmd<C: Controller>(controller: &C, raw: &[u8]) {
    let opcode = u16::from_le_bytes([raw[0], raw[1]]);
    let params = raw.get(3..).unwrap_or(&[]);

    macro_rules! sync {
        ($t:ty) => {
            if opcode == <$t as Cmd>::OPCODE.to_raw() {
                return sync_cmd::<C, $t>(controller, opcode, params).await;
            }
        };
    }
    #[cfg(any(feature = "central", feature = "peripheral"))]
    macro_rules! nb {
        ($t:ty) => {
            if opcode == <$t as Cmd>::OPCODE.to_raw() {
                return async_cmd::<C, $t>(controller, opcode, params).await;
            }
        };
    }

    sync!(Reset);
    sync!(SetEventMask);
    sync!(SetEventMaskPage2);
    sync!(LeSetEventMask);
    sync!(ReadLocalVersionInformation);
    sync!(ReadLocalSupportedCmds);
    sync!(ReadLocalSupportedFeatures);
    sync!(ReadBdAddr);
    sync!(LeReadBufferSize);
    sync!(LeReadLocalSupportedFeatures);
    sync!(LeRand);
    sync!(LeSetRandomAddr);
    sync!(LeSetAdvEnable);
    sync!(LeSetAddrResolutionEnable);
    sync!(LeClearResolvingList);
    sync!(LeAddDeviceToResolvingList);
    sync!(LeRemoveDeviceFromResolvingList);
    sync!(LeSetPrivacyMode);
    sync!(LeSetResolvablePrivateAddrTimeout);

    #[cfg(any(feature = "central", feature = "peripheral"))]
    {
        sync!(Disconnect);
        nb!(LeConnUpdate);
        nb!(LeReadRemoteFeatures);
        nb!(ReadRemoteVersionInformation);
        sync!(LeSetDataLength);
        sync!(LeReadSuggestedDefaultDataLength);
        sync!(LeWriteSuggestedDefaultDataLength);
        sync!(LeReadChannelMap);
        sync!(LeSetHostChannelClassification);
        sync!(ReadRssi);
        sync!(LeAddDeviceToFilterAcceptList);
        sync!(LeRemoveDeviceFromFilterAcceptList);
        sync!(LeClearFilterAcceptList);
        sync!(LeReadFilterAcceptListSize);
    }

    #[cfg(feature = "broadcaster")]
    {
        sync!(LeSetAdvParams);
        sync!(LeSetAdvData);
        sync!(LeSetScanResponseData);
        sync!(LeReadAdvPhysicalChannelTxPower);
    }

    #[cfg(feature = "observer")]
    {
        sync!(LeSetScanParams);
        sync!(LeSetScanEnable);
    }

    #[cfg(feature = "central")]
    {
        nb!(LeCreateConn);
        sync!(LeCreateConnCancel);
    }

    #[cfg(any(feature = "sm", feature = "sm-sc-only"))]
    {
        nb!(LeEnableEncryption);
        sync!(LeLongTermKeyRequestReply);
        sync!(LeLongTermKeyRequestNegativeReply);
    }

    warn!("unmapped HCI command, opcode {:04x}", opcode);
    let evt = plain_ack(opcode, STATUS_UNKNOWN_CMD);
    feed_event(evt[0], &evt[2..], false);
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
    unwrap!(packet
        .extend_from_slice(core::slice::from_raw_parts(buf.cast(), len))
        .ok());

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

/// Feeds one raw HCI event (`[code][len][params]`) into the host - used for
/// controller-originated events and the synthesized command acks alike.
fn feed_event(code: u8, params: &[u8], discardable: bool) {
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

fn rx_dispatch(packet: &ControllerToHostPacket<'_>) {
    match packet {
        ControllerToHostPacket::Event(event) => {
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

            feed_event(code, params, discardable);
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
pub(crate) async fn pump<C: Controller>(controller: &C) -> core::convert::Infallible {
    join(
        async {
            // TX: commands take priority over data
            loop {
                let result = match select(CMD_QUEUE.receive(), ACL_QUEUE.receive()).await {
                    Either::First(cmd) => {
                        trace!("HCI cmd -> controller: {}", Bytes(&cmd));
                        send_cmd(controller, &cmd).await;
                        Ok(())
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
