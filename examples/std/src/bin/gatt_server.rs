//! A BLE GATT server over a Linux HCI controller (real adapter or a BlueZ
//! `btvirt` virtual one), with the service table built **statically** at
//! compile time (no heap) via the [`gatt_services!`] macro. For the same
//! server with the table built at runtime, see `gatt_server_dynamic.rs`.
//!
//! A port of esp-idf-svc's `ble_gatt_server.rs` example, minus the modem
//! peripheral (the controller comes over the vendored Linux HCI-socket transport).
//!
//! Usage: `gatt_server [hci-index]` (default 0). The HCI device must be down
//! (`sudo hciconfig hci0 down`) and the process needs `CAP_NET_ADMIN`.
//! Observe with a phone (nRF Connect), `bluetoothctl` or `btmon`.

// The example itself is Linux-only (it drives a BlueZ
// `HCI_CHANNEL_USER` device); everything it demonstrates is portable.
#![cfg_attr(not(target_os = "linux"), allow(unused))]

use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Mutex;

use log::{info, warn};

use bt_hci::controller::ExternalController;
use nimble_rs::gap::{BleAdvFields, BleAdvParams, GapEvent};
use nimble_rs::gatt::server::{BleGattRegister, GattsEvent};
use nimble_rs::gatt_services;

use nimble_rs::{Ble, BleError, BleUuid, ConnHandle, HostEvent};

const DEVICE_NAME: &str = "nimble-rs";

/// Our service UUID
pub const SERVICE_UUID: BleUuid = BleUuid::uuid128(0xad91b201734740479e173bed82d75f9d);
/// Our "recv" characteristic - i.e. where clients can send data.
pub const RECV_CHARACTERISTIC_UUID: BleUuid = BleUuid::uuid128(0xb6fccb5087be44f3ae22f85485ea42c4);
/// Our "indicate" characteristic - clients receive the counter if they subscribe.
pub const IND_CHARACTERISTIC_UUID: BleUuid = BleUuid::uuid128(0x503de214868246c4828fd59144da41be);

// The whole GATT service table, built at compile time and living in static
// storage. Reads / writes / subscribes arrive on the single `gatts_subscribe`
// hook, keyed by the value handles learned from the `Register` events below.
gatt_services!(SERVICES {
    primary(SERVICE_UUID) {
        // "recv": clients write here; the hook logs it.
        chr(RECV_CHARACTERISTIC_UUID, Write);
        // "indicate": clients subscribe and get the counter pushed from the
        // loop below. NimBLE adds the CCCD (0x2902) automatically.
        chr(IND_CHARACTERISTIC_UUID, Indicate);
    }
});

// Server state, captured from the `Register` events / connection callbacks.
static SUBSCRIBERS: Mutex<Vec<ConnHandle>> = Mutex::new(Vec::new());
static IND_VAL_HANDLE: AtomicU16 = AtomicU16::new(0);
static RECV_VAL_HANDLE: AtomicU16 = AtomicU16::new(0);
static NEEDS_ADV: AtomicBool = AtomicBool::new(false);

fn on_host_event(event: HostEvent) {
    // Advertise once the stack is in sync; re-armed on reset
    if let HostEvent::Sync = event {
        NEEDS_ADV.store(true, Ordering::Relaxed);
    }
}

fn on_gap_event(event: GapEvent) -> i32 {
    match event {
        GapEvent::Connect {
            conn_handle,
            status,
        } => info!("connected (handle {conn_handle}): {status:?}"),
        GapEvent::Disconnect {
            conn_handle,
            reason,
        } => {
            info!("disconnected ({reason}); re-advertising");
            SUBSCRIBERS.lock().unwrap().retain(|&c| c != conn_handle);
            NEEDS_ADV.store(true, Ordering::Relaxed);
        }
        _ => {}
    }

    0
}

fn on_gatts_event(event: GattsEvent) -> u8 {
    match event {
        GattsEvent::Register(BleGattRegister::Characteristic {
            uuid, val_handle, ..
        }) => {
            if uuid == IND_CHARACTERISTIC_UUID {
                IND_VAL_HANDLE.store(val_handle, Ordering::Relaxed);
            } else if uuid == RECV_CHARACTERISTIC_UUID {
                RECV_VAL_HANDLE.store(val_handle, Ordering::Relaxed);
            }
        }
        GattsEvent::Write {
            attr_handle, data, ..
        } if attr_handle == RECV_VAL_HANDLE.load(Ordering::Relaxed) => {
            let mut buf = [0u8; 200];
            match data.read(&mut buf) {
                Ok(n) => info!("recv {n} bytes: {:?}", &buf[..n]),
                Err(e) => warn!("recv read failed: {e}"),
            }
        }
        // Fires on a CCCD write, on connection teardown, and on a bond
        // restore alike, so mirroring `cur_indicate` keeps the list correct
        GattsEvent::SubscriptionChanged {
            conn_handle,
            attr_handle,
            cur_indicate,
            ..
        } if attr_handle == IND_VAL_HANDLE.load(Ordering::Relaxed) => {
            let mut subs = SUBSCRIBERS.lock().unwrap();
            subs.retain(|&c| c != conn_handle);
            if cur_indicate {
                subs.push(conn_handle);
            }
        }
        _ => {}
    }

    0 // ATT status (ignored for `Register` / `SubscriptionChanged`)
}

/// Configure and start a connectable legacy advertisement.
fn start_advertising<S>(driver: &Ble<S>) -> Result<(), BleError> {
    driver.set_device_name(DEVICE_NAME)?;

    driver.adv_set_fields(&BleAdvFields {
        flags: 0x06, // LE General Discoverable, BR/EDR unsupported
        name: Some(DEVICE_NAME),
        ..Default::default()
    })?;

    driver.adv_start(
        0, // BLE_OWN_ADDR_PUBLIC
        &BleAdvParams {
            conn_mode: 2,   // BLE_GAP_CONN_MODE_UND
            disc_mode: 2,   // BLE_GAP_DISC_MODE_GEN
            itvl_min: 0x30, // 30 ms, in 0.625 ms units
            itvl_max: 0x60, // 60 ms
            ..Default::default()
        },
    )
}

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    futures_lite::future::block_on(amain())
}

#[cfg(target_os = "linux")]
async fn amain() -> anyhow::Result<()> {
    env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .init();

    let dev: u16 = std::env::args()
        .nth(1)
        .map(|arg| arg.parse())
        .transpose()?
        .unwrap_or(0);

    let controller =
        ExternalController::<_, 1>::new(nimble_rs_examples_std::linux::Transport::new(dev)?);

    // The service table is `'static`, so the driver just takes a reference
    let driver = Ble::new_with_services(&SERVICES)?;
    driver.host_subscribe(&on_host_event);
    driver.gap_subscribe(&on_gap_event);
    driver.gatts_subscribe(&on_gatts_event);

    info!("NimBLE host starting on hci{dev}");

    embassy_futures::select::select(driver.run(controller), async {
        // The application loop: (re)advertise when needed and push the
        // counter to subscribers, once a second
        let mut counter: u16 = 0;
        loop {
            embassy_time::Timer::after_millis(1000).await;

            if NEEDS_ADV.swap(false, Ordering::Relaxed) {
                match start_advertising(&driver) {
                    Ok(()) => info!("advertising as {DEVICE_NAME:?}"),
                    Err(e) => warn!("failed to start advertising: {e}"),
                }
            }

            let ind_handle = IND_VAL_HANDLE.load(Ordering::Relaxed);
            if ind_handle == 0 {
                continue;
            }

            counter = counter.wrapping_add(1);

            // Copy the list out so the lock isn't held across `indicate`
            let subs = SUBSCRIBERS.lock().unwrap().clone();
            for conn in subs {
                if let Err(e) = driver.indicate(conn, ind_handle, &counter.to_le_bytes()) {
                    warn!("indicate to {conn} failed: {e}");
                }
            }
        }
    })
    .await;

    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("this example requires Linux (it drives a BlueZ HCI_CHANNEL_USER device)");
}
