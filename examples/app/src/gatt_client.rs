//! The GATT client scenario: scan for the [`gatt_server`](crate::gatt_server)
//! by name, connect, discover the service and its characteristics, subscribe
//! to the indications and write a counter back once a second.

use core::cell::Cell;
use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use critical_section::Mutex;

use nimble_rs::gap::{BleDiscParams, GapEvent};
use nimble_rs::gatt::client::GattcEvent;
use nimble_rs::{Ble, BleAddr, Controller, HostEvent, Parker};

use crate::{DEVICE_NAME, IND_CHARACTERISTIC_UUID, RECV_CHARACTERISTIC_UUID, SERVICE_UUID};

static SYNCED: AtomicBool = AtomicBool::new(false);
static PEER: Mutex<Cell<Option<BleAddr>>> = Mutex::new(Cell::new(None));
static CONN: AtomicU16 = AtomicU16::new(0);
static CONNECTED: AtomicBool = AtomicBool::new(false);
static DISCONNECTED: AtomicBool = AtomicBool::new(false);
static SVC_RANGE: Mutex<Cell<Option<(u16, u16)>>> = Mutex::new(Cell::new(None));
static SVC_DONE: AtomicBool = AtomicBool::new(false);
static RECV_VAL: AtomicU16 = AtomicU16::new(0);
static IND_VAL: AtomicU16 = AtomicU16::new(0);
static CHR_DONE: AtomicBool = AtomicBool::new(false);

fn peer() -> Option<BleAddr> {
    critical_section::with(|cs| PEER.borrow(cs).get())
}

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
        if ad_type == 0x09 && &rest[2..1 + len] == DEVICE_NAME.as_bytes() {
            return true;
        }
        rest = &rest[1 + len..];
    }
    false
}

fn on_gap_event(event: GapEvent) -> i32 {
    match event {
        GapEvent::Discovery { addr, data, .. } => {
            if adv_has_name(data) && peer().is_none() {
                info!("found the server");
                critical_section::with(|cs| PEER.borrow(cs).set(Some(addr)));
            }
        }
        GapEvent::Connect { conn_handle, .. } => {
            info!("connected (handle {})", conn_handle);
            CONN.store(conn_handle, Ordering::Relaxed);
            CONNECTED.store(true, Ordering::Relaxed);
        }
        GapEvent::Disconnect { reason, .. } => {
            info!("disconnected (reason {})", reason);
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
                critical_section::with(|cs| {
                    SVC_RANGE
                        .borrow(cs)
                        .set(Some((service.start_handle, service.end_handle)))
                });
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
                warning!("write to {} failed with ATT status {}", attr_handle, status);
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
            if indication {
                info!("indication on {}: {} bytes", attr_handle, n);
            } else {
                info!("notification on {}: {} bytes", attr_handle, n);
            }
        }
        _ => {}
    }
}

/// Runs the GATT client over the given controller, forever (panics when the
/// server disconnects). See [`gatt_server::run`](crate::gatt_server::run) for
/// the `parker` parameter.
pub async fn run<C: Controller>(controller: C, parker: Option<&dyn Parker>) -> ! {
    let ble = match parker {
        Some(parker) => Ble::new_with_parker(parker),
        None => Ble::new(),
    };
    let ble = match ble {
        Ok(ble) => ble,
        Err(e) => panic!("BLE init failed: {}", e.code()),
    };

    ble.host_subscribe(&on_host_event);
    ble.gap_subscribe(&on_gap_event);
    ble.gattc_subscribe(&on_gattc_event);

    info!("GATT client starting");

    embassy_futures::select::select(ble.run(controller), async {
        crate::wait(&SYNCED).await;

        // Infers the own-address type - public where the controller has one
        // (Linux, ESP), static random otherwise (nRF)
        let own_addr_type = match ble.address() {
            Ok((_, addr_type)) => addr_type,
            Err(e) => panic!("no identity address: {}", e.code()),
        };

        // 1. Scan until the server shows up
        if let Err(e) = ble.disc(own_addr_type, None, &BleDiscParams::default()) {
            panic!("disc failed: {}", e.code());
        }
        while peer().is_none() {
            embassy_time::Timer::after_millis(50).await;
        }
        let _ = ble.disc_cancel();

        // 2. Connect
        let peer = peer().unwrap();
        if let Err(e) = ble.connect(own_addr_type, &peer) {
            panic!("connect failed: {}", e.code());
        }
        crate::wait(&CONNECTED).await;
        let conn = CONN.load(Ordering::Relaxed);

        // 3. Discover the service, then its characteristics
        if let Err(e) = ble.discover_services(conn) {
            panic!("service discovery failed: {}", e.code());
        }
        crate::wait(&SVC_DONE).await;
        let Some((start, end)) = critical_section::with(|cs| SVC_RANGE.borrow(cs).get()) else {
            panic!("service not found");
        };

        if let Err(e) = ble.discover_characteristics(conn, start, end) {
            panic!("characteristic discovery failed: {}", e.code());
        }
        crate::wait(&CHR_DONE).await;

        let recv = RECV_VAL.load(Ordering::Relaxed);
        let ind = IND_VAL.load(Ordering::Relaxed);
        assert!(recv != 0 && ind != 0, "characteristics not found");

        // 4. Subscribe for indications: write 0x0002 to the CCCD, which
        //    NimBLE places right after the characteristic value attribute
        if let Err(e) = ble.write(conn, ind + 1, &[0x02, 0x00]) {
            panic!("subscribe failed: {}", e.code());
        }

        // 5. Write to "recv" once a second; indications arrive on the hook
        let mut counter: u32 = 0;
        while !DISCONNECTED.load(Ordering::Relaxed) {
            embassy_time::Timer::after_millis(1000).await;

            counter = counter.wrapping_add(1);
            if let Err(e) = ble.write(conn, recv, &counter.to_le_bytes()) {
                warning!("write failed (error {})", e.code());
            }
        }
    })
    .await;

    panic!("BLE host stopped unexpectedly");
}
