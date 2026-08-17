//! M3 gate: a full GATT-server exchange against the mock controller, hermetic
//! (no HCI hardware, no privileges):
//!
//! host sync -> advertising -> (simulated) central connects -> ATT write ->
//! ATT read -> CCCD subscribe -> indication + confirmation -> disconnect.
//!
//! Exercises the whole M3 surface: the `gatt_services!` static table, the
//! register/read/write/subscribe/notify-complete events, legacy advertising,
//! the GAP event demux - plus the ACL RX/TX path of the HCI bridge.

use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use embassy_futures::select::{select, Either};

use nimble_rs::gap::{BleAdvFields, BleAdvParams, GapEvent};
use nimble_rs::gatt::server::GattsEvent;
use nimble_rs::gatt_services;
use nimble_rs::{BleDriver, BleUuid, ForTransport, HostEvent};

#[path = "common/mock.rs"]
mod mock;

use mock::MockController;

const SVC: BleUuid = BleUuid::uuid128(0xad91b201_73474047_9e173bed_82d75f9d);
const RECV: BleUuid = BleUuid::uuid128(0xb6fccb50_87be44f3_ae22f854_85ea42c4);
const IND: BleUuid = BleUuid::uuid16(0x2A37);

gatt_services!(SERVICES {
    primary(SVC) {
        chr(RECV, Write);
        chr(IND, Read | Notify | Indicate);
    }
});

static SYNCED: AtomicBool = AtomicBool::new(false);
static CONNECTED: AtomicBool = AtomicBool::new(false);
static DISCONNECTED: AtomicBool = AtomicBool::new(false);
static SUBSCRIBED: AtomicBool = AtomicBool::new(false);
static INDICATED: AtomicBool = AtomicBool::new(false);
static WRITTEN: AtomicBool = AtomicBool::new(false);

static RECV_HANDLE: AtomicU16 = AtomicU16::new(0);
static IND_HANDLE: AtomicU16 = AtomicU16::new(0);
static CCCD_HANDLE: AtomicU16 = AtomicU16::new(0);

fn on_host_event(event: HostEvent) {
    log::info!("host event: {event:?}");
    if event == HostEvent::Sync {
        SYNCED.store(true, Ordering::SeqCst);
    }
}

fn on_gap_event(event: GapEvent) -> i32 {
    match event {
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
        GapEvent::Mtu { value, .. } => log::info!("MTU: {value}"),
        _ => (),
    }
    0
}

fn on_gatts_event(event: GattsEvent) -> u8 {
    match event {
        GattsEvent::Register(register) => {
            use nimble_rs::gatt::server::BleGattRegister;
            match register {
                BleGattRegister::Characteristic {
                    uuid, val_handle, ..
                } => {
                    log::info!("registered chr {uuid:?} -> val handle {val_handle}");
                    if uuid == RECV {
                        RECV_HANDLE.store(val_handle, Ordering::SeqCst);
                    } else if uuid == IND {
                        IND_HANDLE.store(val_handle, Ordering::SeqCst);
                    }
                }
                BleGattRegister::Descriptor { uuid, handle } => {
                    log::info!("registered dsc {uuid:?} -> handle {handle}");
                    if uuid == BleUuid::uuid16(0x2902) {
                        CCCD_HANDLE.store(handle, Ordering::SeqCst);
                    }
                }
                BleGattRegister::Service { uuid, handle } => {
                    log::info!("registered svc {uuid:?} -> handle {handle}");
                }
                BleGattRegister::Other => (),
            }
        }
        GattsEvent::Read {
            attr_handle,
            mut reply,
            ..
        } => {
            log::info!("read of handle {attr_handle}");
            if attr_handle == IND_HANDLE.load(Ordering::SeqCst) {
                reply.append(b"world").unwrap();
            }
        }
        GattsEvent::Write {
            attr_handle, data, ..
        } => {
            let mut buf = [0; 32];
            let len = data.read(&mut buf).unwrap();
            log::info!("write of handle {attr_handle}: {:?}", &buf[..len]);
            if attr_handle == RECV_HANDLE.load(Ordering::SeqCst) && &buf[..len] == b"hello" {
                WRITTEN.store(true, Ordering::SeqCst);
            }
        }
        GattsEvent::SubscriptionChanged {
            attr_handle,
            cur_indicate,
            ..
        } => {
            log::info!("subscription changed: handle={attr_handle} indicate={cur_indicate}");
            if cur_indicate {
                SUBSCRIBED.store(true, Ordering::SeqCst);
            }
        }
        GattsEvent::NotifyComplete {
            attr_handle,
            indication,
            status,
            ..
        } => {
            log::info!(
                "notify complete: handle={attr_handle} indication={indication} status={status}"
            );
            if indication && status == 0 {
                INDICATED.store(true, Ordering::SeqCst);
            }
        }
    }
    0
}

async fn wait(flag: &AtomicBool) {
    while !flag.load(Ordering::SeqCst) {
        embassy_time::Timer::after_millis(5).await;
    }
}

/// Inject an ACL packet carrying one ATT PDU (as the simulated central).
fn inject_att(att: &[u8]) {
    let mut acl = heapless::Vec::<u8, 64>::new();
    // ACL header: handle 1, PB = 0b10 (first packet), len; then L2CAP: len, CID 4 (ATT)
    acl.extend_from_slice(&0x2001u16.to_le_bytes()).unwrap();
    acl.extend_from_slice(&((att.len() + 4) as u16).to_le_bytes())
        .unwrap();
    acl.extend_from_slice(&(att.len() as u16).to_le_bytes())
        .unwrap();
    acl.extend_from_slice(&0x0004u16.to_le_bytes()).unwrap();
    acl.extend_from_slice(att).unwrap();

    mock::inject_acl(&acl);
}

/// The next ATT PDU the host sent (strips the ACL + L2CAP headers).
async fn host_att() -> heapless::Vec<u8, 64> {
    let acl = mock::host_acl().await;
    heapless::Vec::from_slice(&acl[8..]).unwrap()
}

#[test]
fn gatts_smoke() {
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .is_test(true)
        .try_init();

    let driver = BleDriver::new_with_services(&SERVICES).expect("driver init");
    driver.host_subscribe(&on_host_event);
    driver.gap_subscribe(&on_gap_event);
    driver.gatts_subscribe(&on_gatts_event);

    let controller = ForTransport::new(MockController::new());

    futures_lite::future::block_on(async {
        match select(driver.run(controller), async {
            // 1. Sync, then start advertising
            wait(&SYNCED).await;

            driver.set_device_name("nimble-rs-gatts").expect("name");
            driver
                .adv_set_fields(&BleAdvFields {
                    flags: 0x06, // LE General Discoverable | BR/EDR Not Supported
                    name: Some("nimble-rs-gatts"),
                    ..Default::default()
                })
                .expect("adv fields");
            driver
                .adv_start(
                    0,
                    &BleAdvParams {
                        conn_mode: 2, // BLE_GAP_CONN_MODE_UND (undirected connectable)
                        disc_mode: 2, // BLE_GAP_DISC_MODE_GEN (general discoverable)
                        ..Default::default()
                    },
                )
                .expect("adv start");

            while !mock::advertising() {
                embassy_time::Timer::after_millis(5).await;
            }
            log::info!("advertising");

            // 2. A central connects: LE Connection Complete
            #[rustfmt::skip]
            mock::inject_event(&[
                0x3e, 19, 0x01,
                0x00,             // status
                0x01, 0x00,       // handle 1
                0x01,             // role: slave
                0x00,             // peer addr type
                0x11, 0x22, 0x33, 0x44, 0x55, 0x66, // peer addr
                0x28, 0x00,       // conn interval
                0x00, 0x00,       // latency
                0xf4, 0x01,       // supervision timeout
                0x00,             // master clock accuracy
            ]);
            wait(&CONNECTED).await;

            let recv = RECV_HANDLE.load(Ordering::SeqCst);
            let ind = IND_HANDLE.load(Ordering::SeqCst);
            assert!(recv != 0 && ind != 0);
            // NimBLE synthesizes the CCCD internally (no Descriptor register
            // event); it sits right after the characteristic value attribute
            let cccd = ind + 1;

            // 3. ATT Write Request "hello" to RECV
            let mut att = heapless::Vec::<u8, 64>::new();
            att.push(0x12).unwrap();
            att.extend_from_slice(&recv.to_le_bytes()).unwrap();
            att.extend_from_slice(b"hello").unwrap();
            inject_att(&att);

            let rsp = host_att().await;
            assert_eq!(rsp[0], 0x13, "expected ATT Write Response: {rsp:02x?}");
            assert!(WRITTEN.load(Ordering::SeqCst));

            // 4. ATT Read Request of IND -> "world"
            let mut att = heapless::Vec::<u8, 64>::new();
            att.push(0x0a).unwrap();
            att.extend_from_slice(&ind.to_le_bytes()).unwrap();
            inject_att(&att);

            let rsp = host_att().await;
            assert_eq!(rsp[0], 0x0b, "expected ATT Read Response: {rsp:02x?}");
            assert_eq!(&rsp[1..], b"world");

            // 5. Subscribe for indications (write 0x0002 to the CCCD)
            let mut att = heapless::Vec::<u8, 64>::new();
            att.push(0x12).unwrap();
            att.extend_from_slice(&cccd.to_le_bytes()).unwrap();
            att.extend_from_slice(&[0x02, 0x00]).unwrap();
            inject_att(&att);

            let rsp = host_att().await;
            assert_eq!(rsp[0], 0x13, "expected ATT Write Response: {rsp:02x?}");
            wait(&SUBSCRIBED).await;

            // 6. Indicate "ping"; confirm; expect NotifyComplete
            driver.indicate(1, ind, b"ping").expect("indicate");

            let ind_pdu = host_att().await;
            assert_eq!(ind_pdu[0], 0x1d, "expected ATT Indication: {ind_pdu:02x?}");
            assert_eq!(&ind_pdu[3..], b"ping");

            inject_att(&[0x1e]); // Handle Value Confirmation
            wait(&INDICATED).await;

            // 7. Disconnect
            mock::inject_event(&[0x05, 4, 0x00, 0x01, 0x00, 0x13]);
            wait(&DISCONNECTED).await;

            println!("GATT SMOKE OK: write/read/subscribe/indicate all verified");
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
