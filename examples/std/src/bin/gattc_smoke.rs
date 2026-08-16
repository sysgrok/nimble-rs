//! M4 gate: the GATT *client* and L2CAP CoC surfaces against the mock
//! controller, which here plays a remote peripheral (canned ATT / L2CAP
//! signaling responses). Hermetic - no HCI hardware, no privileges:
//!
//! scan -> discovery report -> connect (central) -> discover services ->
//! discover characteristics -> read -> write -> notification -> L2CAP CoC
//! connect -> SDU echo both ways -> channel + link disconnect.

use core::cell::Cell;
use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use embassy_futures::select::{select, Either};

use nimble_rs::gap::{BleDiscParams, GapEvent};
use nimble_rs::gatt::client::GattcEvent;
use nimble_rs::l2cap::{L2capChan, L2capEvent, SendOutcome};
use nimble_rs::{BleAddr, BleDriver, ForTransport, HostEvent};

use nimble_rs_examples_std::mock::{self, MockController};

const PSM: u16 = 0x0080;

static SYNCED: AtomicBool = AtomicBool::new(false);
static DISCOVERED_PEER: AtomicBool = AtomicBool::new(false);
static CONNECTED: AtomicBool = AtomicBool::new(false);
static DISCONNECTED: AtomicBool = AtomicBool::new(false);

static SVC_START: AtomicU16 = AtomicU16::new(0);
static SVC_END: AtomicU16 = AtomicU16::new(0);
static SVC_DONE: AtomicBool = AtomicBool::new(false);
static CHR_VAL: AtomicU16 = AtomicU16::new(0);
static CHR_DONE: AtomicBool = AtomicBool::new(false);
static READ_OK: AtomicBool = AtomicBool::new(false);
static WRITE_OK: AtomicBool = AtomicBool::new(false);
static NOTIFY_OK: AtomicBool = AtomicBool::new(false);

static L2_CONNECTED: AtomicBool = AtomicBool::new(false);
static L2_RECEIVED: AtomicBool = AtomicBool::new(false);
static L2_DISCONNECTED: AtomicBool = AtomicBool::new(false);

/// A `static`-storable cell for the L2CAP channel handle (test-only; guarded
/// by the single-threaded test flow).
struct SyncCell<T>(Cell<Option<T>>);
unsafe impl<T> Sync for SyncCell<T> {}
static L2_CHAN: SyncCell<L2capChan> = SyncCell(Cell::new(None));

fn on_host_event(event: HostEvent) {
    log::info!("host event: {event:?}");
    if event == HostEvent::Sync {
        SYNCED.store(true, Ordering::SeqCst);
    }
}

fn on_gap_event(event: GapEvent) -> i32 {
    match event {
        GapEvent::Discovery {
            addr, rssi, data, ..
        } => {
            log::info!("discovered {addr:?} rssi={rssi} data={data:02x?}");
            DISCOVERED_PEER.store(true, Ordering::SeqCst);
        }
        GapEvent::DiscoveryComplete { reason } => {
            log::info!("discovery complete, reason {reason}");
        }
        GapEvent::Connect {
            conn_handle,
            status,
        } => {
            log::info!("connected: handle={conn_handle} status={status:?}");
            CONNECTED.store(true, Ordering::SeqCst);
        }
        GapEvent::Disconnect {
            conn_handle,
            reason,
        } => {
            log::info!("disconnected: handle={conn_handle} reason={reason:?}");
            DISCONNECTED.store(true, Ordering::SeqCst);
        }
        _ => (),
    }
    0
}

fn on_gattc_event(event: GattcEvent) {
    match event {
        GattcEvent::Service {
            status, service, ..
        } => match service {
            Some(service) => {
                log::info!(
                    "service {:?} range {}..{}",
                    service.uuid,
                    service.start_handle,
                    service.end_handle
                );
                SVC_START.store(service.start_handle, Ordering::SeqCst);
                SVC_END.store(service.end_handle, Ordering::SeqCst);
            }
            None => {
                log::info!("service discovery done, status {status}");
                SVC_DONE.store(true, Ordering::SeqCst);
            }
        },
        GattcEvent::Characteristic { status, chr, .. } => match chr {
            Some(chr) => {
                log::info!(
                    "chr {:?} val handle {} props {:#04x}",
                    chr.uuid,
                    chr.val_handle,
                    chr.properties
                );
                CHR_VAL.store(chr.val_handle, Ordering::SeqCst);
            }
            None => {
                log::info!("chr discovery done, status {status}");
                CHR_DONE.store(true, Ordering::SeqCst);
            }
        },
        GattcEvent::ReadComplete {
            status,
            attr_handle,
            data,
            ..
        } => {
            let mut buf = [0; 32];
            let len = data.read(&mut buf).unwrap();
            log::info!(
                "read of {attr_handle} done, status {status}: {:?}",
                &buf[..len]
            );
            if status == 0 && &buf[..len] == b"remote" {
                READ_OK.store(true, Ordering::SeqCst);
            }
        }
        GattcEvent::WriteComplete {
            status,
            attr_handle,
            ..
        } => {
            log::info!("write of {attr_handle} done, status {status}");
            if status == 0 {
                WRITE_OK.store(true, Ordering::SeqCst);
            }
        }
        GattcEvent::Notify {
            attr_handle,
            indication,
            data,
            ..
        } => {
            let mut buf = [0; 32];
            let len = data.read(&mut buf).unwrap();
            log::info!(
                "notify on {attr_handle} (indication={indication}): {:?}",
                &buf[..len]
            );
            if &buf[..len] == b"note" {
                NOTIFY_OK.store(true, Ordering::SeqCst);
            }
        }
    }
}

fn on_l2cap_event(event: L2capEvent) -> i32 {
    match event {
        L2capEvent::Connected { status, chan, .. } => {
            log::info!("l2cap connected, status {status}");
            if status == 0 {
                L2_CHAN.0.set(Some(chan));
                L2_CONNECTED.store(true, Ordering::SeqCst);
            }
        }
        L2capEvent::Received { data, .. } => {
            let mut buf = [0; 64];
            let len = data.read(&mut buf).unwrap();
            log::info!("l2cap received: {:?}", &buf[..len]);
            if &buf[..len] == b"echo!" {
                L2_RECEIVED.store(true, Ordering::SeqCst);
            }
        }
        L2capEvent::Disconnected { .. } => {
            log::info!("l2cap disconnected");
            L2_DISCONNECTED.store(true, Ordering::SeqCst);
        }
        _ => (),
    }
    0
}

async fn wait(flag: &AtomicBool) {
    while !flag.load(Ordering::SeqCst) {
        embassy_time::Timer::after_millis(5).await;
    }
}

/// Inject an ACL packet carrying one L2CAP frame on `cid`.
fn inject_l2cap(cid: u16, payload: &[u8]) {
    let mut acl = heapless::Vec::<u8, 128>::new();
    acl.extend_from_slice(&0x2001u16.to_le_bytes()).unwrap();
    acl.extend_from_slice(&((payload.len() + 4) as u16).to_le_bytes())
        .unwrap();
    acl.extend_from_slice(&(payload.len() as u16).to_le_bytes())
        .unwrap();
    acl.extend_from_slice(&cid.to_le_bytes()).unwrap();
    acl.extend_from_slice(payload).unwrap();

    mock::inject_acl(&acl);
}

/// The next L2CAP frame the host sent, as `(cid, payload)`.
async fn host_l2cap() -> (u16, heapless::Vec<u8, 128>) {
    let acl = mock::host_acl().await;
    let cid = u16::from_le_bytes([acl[6], acl[7]]);
    (cid, heapless::Vec::from_slice(&acl[8..]).unwrap())
}

/// The next ATT PDU the host sent, transparently answering an MTU exchange.
async fn host_att() -> heapless::Vec<u8, 128> {
    loop {
        let (cid, pdu) = host_l2cap().await;
        assert_eq!(cid, 4, "expected an ATT PDU: cid={cid} {pdu:02x?}");

        if pdu[0] == 0x02 {
            // ATT Exchange MTU Request -> Response (MTU 247)
            inject_l2cap(4, &[0x03, 247, 0]);
            continue;
        }

        return pdu;
    }
}

fn main() {
    env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .init();

    let driver = BleDriver::new().expect("driver init");
    driver.host_subscribe(&on_host_event);
    driver.gap_subscribe(&on_gap_event);
    driver.gattc_subscribe(&on_gattc_event);
    driver.l2cap_subscribe(&on_l2cap_event);

    let controller = ForTransport::new(MockController::new());

    futures_lite::future::block_on(async {
        match select(driver.run(controller), async {
            wait(&SYNCED).await;

            // 1. Scan; the mock is made to "advertise"
            driver
                .disc(
                    0,
                    None,
                    &BleDiscParams {
                        passive: true,
                        ..Default::default()
                    },
                )
                .expect("disc");

            // LE Advertising Report: 1 report, ADV_IND, public addr, 3 bytes
            // of data (flags AD), RSSI -42
            #[rustfmt::skip]
            mock::inject_event(&[
                0x3e, 15, 0x02, 1,
                0x00,                                // ADV_IND
                0x00,                                // public
                0x66, 0x55, 0x44, 0x33, 0x22, 0x11,  // addr
                3, 0x02, 0x01, 0x06,                 // adv data: flags
                0xd6,                                // RSSI
            ]);
            wait(&DISCOVERED_PEER).await;

            driver.disc_cancel().expect("disc cancel");

            // 2. Connect as central
            let peer = BleAddr::new(0, [0x66, 0x55, 0x44, 0x33, 0x22, 0x11]);
            driver.connect(0, &peer).expect("connect");
            wait(&CONNECTED).await;

            // 3. Service discovery: Read By Group Type -> one 128-bit service
            driver.discover_services(1).expect("disc svcs");

            let req = host_att().await;
            assert_eq!(req[0], 0x10, "expected Read By Group Type: {req:02x?}");
            let mut rsp = heapless::Vec::<u8, 64>::new();
            rsp.extend_from_slice(&[0x11, 20]).unwrap(); // one 20-byte entry
            rsp.extend_from_slice(&0x0010u16.to_le_bytes()).unwrap();
            rsp.extend_from_slice(&0x0018u16.to_le_bytes()).unwrap();
            rsp.extend_from_slice(&0xad91b201_73474047_9e173bed_82d75f9du128.to_le_bytes())
                .unwrap();
            inject_l2cap(4, &rsp);

            let req = host_att().await;
            assert_eq!(req[0], 0x10, "expected continuation: {req:02x?}");
            inject_l2cap(4, &[0x01, 0x10, req[1], req[2], 0x0a]); // Attribute Not Found
            wait(&SVC_DONE).await;

            let (start, end) = (
                SVC_START.load(Ordering::SeqCst),
                SVC_END.load(Ordering::SeqCst),
            );
            assert_eq!((start, end), (0x0010, 0x0018));

            // 4. Characteristic discovery: Read By Type -> one 128-bit chr
            driver
                .discover_characteristics(1, start, end)
                .expect("disc chrs");

            let req = host_att().await;
            assert_eq!(req[0], 0x08, "expected Read By Type: {req:02x?}");
            let mut rsp = heapless::Vec::<u8, 64>::new();
            rsp.extend_from_slice(&[0x09, 21]).unwrap(); // one 21-byte entry
            rsp.extend_from_slice(&0x0011u16.to_le_bytes()).unwrap(); // decl handle
            rsp.push(0x1a).unwrap(); // props: read | write | notify
            rsp.extend_from_slice(&0x0012u16.to_le_bytes()).unwrap(); // val handle
            rsp.extend_from_slice(&0xb6fccb50_87be44f3_ae22f854_85ea42c4u128.to_le_bytes())
                .unwrap();
            inject_l2cap(4, &rsp);

            let req = host_att().await;
            assert_eq!(req[0], 0x08, "expected continuation: {req:02x?}");
            inject_l2cap(4, &[0x01, 0x08, req[1], req[2], 0x0a]);
            wait(&CHR_DONE).await;

            let val = CHR_VAL.load(Ordering::SeqCst);
            assert_eq!(val, 0x0012);

            // 5. Read -> "remote"
            driver.read(1, val).expect("read");
            let req = host_att().await;
            assert_eq!(req[0], 0x0a, "expected Read Request: {req:02x?}");
            inject_l2cap(4, b"\x0bremote");
            wait(&READ_OK).await;

            // 6. Write -> Write Response
            driver.write(1, val, b"hi!").expect("write");
            let req = host_att().await;
            assert_eq!(req[0], 0x12, "expected Write Request: {req:02x?}");
            inject_l2cap(4, &[0x13]);
            wait(&WRITE_OK).await;

            // 7. Peer notification
            inject_l2cap(4, b"\x1b\x12\x00note");
            wait(&NOTIFY_OK).await;

            // 8. L2CAP CoC connect
            driver.l2cap_connect(1, PSM, 64).expect("l2cap connect");

            let (cid, req) = host_l2cap().await;
            assert_eq!(cid, 5, "expected LE signaling: {req:02x?}");
            assert_eq!(
                req[0], 0x14,
                "expected LE CoC Connection Request: {req:02x?}"
            );
            let id = req[1];
            let scid = u16::from_le_bytes([req[6], req[7]]);

            // Response: dcid 0x0060, mtu 64, mps 64, 5 credits, success
            let mut rsp = heapless::Vec::<u8, 64>::new();
            rsp.extend_from_slice(&[0x15, id, 10, 0]).unwrap();
            rsp.extend_from_slice(&0x0060u16.to_le_bytes()).unwrap();
            rsp.extend_from_slice(&64u16.to_le_bytes()).unwrap();
            rsp.extend_from_slice(&64u16.to_le_bytes()).unwrap();
            rsp.extend_from_slice(&5u16.to_le_bytes()).unwrap();
            rsp.extend_from_slice(&0u16.to_le_bytes()).unwrap();
            inject_l2cap(5, &rsp);
            wait(&L2_CONNECTED).await;

            let chan = L2_CHAN.0.get().unwrap();

            // 9. Send an SDU; the "peer" verifies it and echoes it back
            assert_eq!(
                driver.l2cap_send(chan, b"echo!").expect("send"),
                SendOutcome::Sent
            );

            let (cid, frame) = host_l2cap().await;
            assert_eq!(cid, 0x0060, "expected CoC data: {frame:02x?}");
            assert_eq!(&frame[..2], &5u16.to_le_bytes()); // SDU length prefix
            assert_eq!(&frame[2..], b"echo!");

            let mut sdu = heapless::Vec::<u8, 64>::new();
            sdu.extend_from_slice(&5u16.to_le_bytes()).unwrap();
            sdu.extend_from_slice(b"echo!").unwrap();
            inject_l2cap(scid, &sdu);
            wait(&L2_RECEIVED).await;

            driver.l2cap_recv_ready(chan, 64).expect("recv ready");

            // 10. Channel disconnect, then link disconnect
            driver.l2cap_disconnect(chan).expect("l2cap disconnect");
            // Skip LE Flow Control Credit updates (0x16) triggered by
            // `recv_ready` on the way to the Disconnection Request
            let req = loop {
                let (cid, req) = host_l2cap().await;
                assert_eq!(cid, 5);
                if req[0] != 0x16 {
                    break req;
                }
            };
            assert_eq!(req[0], 0x06, "expected Disconnection Request: {req:02x?}");
            // Response echoes dcid/scid
            inject_l2cap(5, &[0x07, req[1], 4, 0, req[4], req[5], req[6], req[7]]);
            wait(&L2_DISCONNECTED).await;

            driver.disconnect(1).expect("disconnect");
            let _ = host_l2cap; // (link-level terminate goes out as an HCI command)
            mock::inject_event(&[0x05, 4, 0x00, 0x01, 0x00, 0x16]);
            wait(&DISCONNECTED).await;

            println!(
                "GATTC SMOKE OK: scan/connect/discover/read/write/notify + l2cap echo all verified"
            );
        })
        .await
        {
            Either::First(result) => {
                result.expect("run failed");
                unreachable!()
            }
            Either::Second(()) => (),
        }
    });
}
