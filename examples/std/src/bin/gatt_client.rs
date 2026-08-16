//! A BLE GATT client over a Linux HCI controller: scans for the
//! `gatt_server` example by name, connects, discovers its service and
//! characteristics, subscribes to the indicate characteristic (by writing its
//! CCCD) and writes to the recv characteristic once a second, printing every
//! received indication.
//!
//! A port of esp-idf-svc's `ble_gatt_client.rs`, upgraded with scanning (the
//! reference could only connect to a hardcoded address).
//!
//! Usage: `gatt_client [hci-index]` (default 0), with `gatt_server` running
//! on another controller (e.g. the second `btvirt` device).

use core::cell::Cell;
use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use log::{info, warn};

use nimble_rs::gap::{BleDiscParams, GapEvent};
use nimble_rs::gatt::client::GattcEvent;
use nimble_rs::{BleAddr, BleDriver, BleUuid, ForTransport, HostEvent};

const PEER_NAME: &str = "nimble-rs";

pub const SERVICE_UUID: BleUuid = BleUuid::uuid128(0xad91b201734740479e173bed82d75f9d);
pub const RECV_CHARACTERISTIC_UUID: BleUuid = BleUuid::uuid128(0xb6fccb5087be44f3ae22f85485ea42c4);
pub const IND_CHARACTERISTIC_UUID: BleUuid = BleUuid::uuid128(0x503de214868246c4828fd59144da41be);

struct SyncCell<T>(Cell<Option<T>>);
unsafe impl<T> Sync for SyncCell<T> {}

static SYNCED: AtomicBool = AtomicBool::new(false);
static PEER: SyncCell<BleAddr> = SyncCell(Cell::new(None));
static CONN: AtomicU16 = AtomicU16::new(0);
static CONNECTED: AtomicBool = AtomicBool::new(false);
static DISCONNECTED: AtomicBool = AtomicBool::new(false);
static SVC_RANGE: SyncCell<(u16, u16)> = SyncCell(Cell::new(None));
static SVC_DONE: AtomicBool = AtomicBool::new(false);
static RECV_VAL: AtomicU16 = AtomicU16::new(0);
static IND_VAL: AtomicU16 = AtomicU16::new(0);
static CHR_DONE: AtomicBool = AtomicBool::new(false);

fn on_host_event(event: HostEvent) {
    if event == HostEvent::Sync {
        SYNCED.store(true, Ordering::Relaxed);
    }
}

/// Whether `data` (raw advertising payload) carries our peer's complete name.
fn adv_has_name(data: &[u8]) -> bool {
    let mut rest = data;
    while let [len, ad_type, ..] = *rest {
        let len = len as usize;
        if len == 0 || rest.len() < 1 + len {
            break;
        }
        // 0x09: Complete Local Name
        if ad_type == 0x09 && &rest[2..1 + len] == PEER_NAME.as_bytes() {
            return true;
        }
        rest = &rest[1 + len..];
    }
    false
}

fn on_gap_event(event: GapEvent) -> i32 {
    match event {
        GapEvent::Discovery { addr, data, .. } => {
            if adv_has_name(data) && PEER.0.get().is_none() {
                info!("found {PEER_NAME:?} at {addr:?}");
                PEER.0.set(Some(addr));
            }
        }
        GapEvent::Connect {
            conn_handle,
            status,
        } => {
            info!("connected (handle {conn_handle}): {status:?}");
            if status.is_ok() {
                CONN.store(conn_handle, Ordering::Relaxed);
                CONNECTED.store(true, Ordering::Relaxed);
            }
        }
        GapEvent::Disconnect { reason, .. } => {
            info!("disconnected: {reason}");
            DISCONNECTED.store(true, Ordering::Relaxed);
        }
        _ => {}
    }

    0
}

fn on_gattc_event(event: GattcEvent) {
    match event {
        GattcEvent::Service { service, .. } => match service {
            Some(service) if service.uuid == SERVICE_UUID => {
                SVC_RANGE
                    .0
                    .set(Some((service.start_handle, service.end_handle)));
            }
            Some(_) => {}
            None => SVC_DONE.store(true, Ordering::Relaxed),
        },
        GattcEvent::Characteristic { chr, .. } => match chr {
            Some(chr) => {
                if chr.uuid == RECV_CHARACTERISTIC_UUID {
                    RECV_VAL.store(chr.val_handle, Ordering::Relaxed);
                } else if chr.uuid == IND_CHARACTERISTIC_UUID {
                    IND_VAL.store(chr.val_handle, Ordering::Relaxed);
                }
            }
            None => CHR_DONE.store(true, Ordering::Relaxed),
        },
        GattcEvent::WriteComplete {
            status,
            attr_handle,
            ..
        } => {
            if status != 0 {
                warn!("write to {attr_handle} failed with ATT status {status}");
            }
        }
        GattcEvent::Notify {
            attr_handle,
            indication,
            data,
            ..
        } => {
            let mut buf = [0u8; 64];
            let n = data.read(&mut buf).unwrap_or(0);
            info!(
                "{} on {attr_handle}: {:?}",
                if indication {
                    "indication"
                } else {
                    "notification"
                },
                &buf[..n]
            );
        }
        _ => {}
    }
}

async fn wait(flag: &AtomicBool) {
    while !flag.load(Ordering::Relaxed) {
        embassy_time::Timer::after_millis(10).await;
    }
}

fn main() -> anyhow::Result<()> {
    futures_lite::future::block_on(amain())
}

async fn amain() -> anyhow::Result<()> {
    env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .init();

    let dev: u16 = std::env::args()
        .nth(1)
        .map(|arg| arg.parse())
        .transpose()?
        .unwrap_or(0);

    let controller = ForTransport::new(nimble_rs_examples_std::linux::Transport::new(dev)?);

    let driver = BleDriver::new()?;
    driver.host_subscribe(&on_host_event);
    driver.gap_subscribe(&on_gap_event);
    driver.gattc_subscribe(&on_gattc_event);

    info!("GATT client starting on hci{dev}");

    embassy_futures::select::select(driver.run(controller), async {
        wait(&SYNCED).await;

        // 1. Scan until the server shows up
        driver
            .disc(0, None, &BleDiscParams::default())
            .expect("disc");
        while PEER.0.get().is_none() {
            embassy_time::Timer::after_millis(50).await;
        }
        driver.disc_cancel().ok();

        // 2. Connect
        let peer = PEER.0.get().unwrap();
        driver.connect(0, &peer).expect("connect");
        wait(&CONNECTED).await;
        let conn = CONN.load(Ordering::Relaxed);

        // 3. Discover the service, then its characteristics
        driver.discover_services(conn).expect("disc services");
        wait(&SVC_DONE).await;
        let (start, end) = SVC_RANGE.0.get().expect("service not found");

        driver
            .discover_characteristics(conn, start, end)
            .expect("disc chrs");
        wait(&CHR_DONE).await;

        let recv = RECV_VAL.load(Ordering::Relaxed);
        let ind = IND_VAL.load(Ordering::Relaxed);
        assert!(recv != 0 && ind != 0, "characteristics not found");

        // 4. Subscribe for indications: write 0x0002 to the CCCD, which
        //    NimBLE places right after the characteristic value attribute
        driver
            .write(conn, ind + 1, &[0x02, 0x00])
            .expect("subscribe");

        // 5. Write to "recv" once a second; indications arrive on the hook
        let mut counter: u32 = 0;
        while !DISCONNECTED.load(Ordering::Relaxed) {
            embassy_time::Timer::after_millis(1000).await;

            counter = counter.wrapping_add(1);
            if let Err(e) = driver.write(conn, recv, &counter.to_le_bytes()) {
                warn!("write failed: {e}");
            }
        }
    })
    .await;

    Ok(())
}
