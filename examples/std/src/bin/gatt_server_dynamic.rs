//! The same GATT server as `gatt_server.rs`, but with the service table built
//! **at runtime** (heap-allocated `BleGattServices`) instead of the
//! compile-time `gatt_services!` macro - useful when the table shape is not
//! known at compile time. Everything else (hooks, advertising, indications)
//! is identical; see `gatt_server.rs` for the commentary.
//!
//! Usage: `gatt_server_dynamic [hci-index]` (default 0).

use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::Mutex;

use log::{info, warn};

use enumset::enum_set;

use nimble_rs::gap::{BleAdvFields, BleAdvParams, GapEvent};
use nimble_rs::gatt::server::{
    BleGattCharacteristic, BleGattRegister, BleGattService, BleGattServices, GattsEvent,
};
use nimble_rs::gatt::BleGattCharFlag;
use nimble_rs::{Ble, BleError, BleUuid, ConnHandle, ForTransport, HostEvent};

const DEVICE_NAME: &str = "nimble-rs";

pub const SERVICE_UUID: BleUuid = BleUuid::uuid128(0xad91b201734740479e173bed82d75f9d);
pub const RECV_CHARACTERISTIC_UUID: BleUuid = BleUuid::uuid128(0xb6fccb5087be44f3ae22f85485ea42c4);
pub const IND_CHARACTERISTIC_UUID: BleUuid = BleUuid::uuid128(0x503de214868246c4828fd59144da41be);

static SUBSCRIBERS: Mutex<Vec<ConnHandle>> = Mutex::new(Vec::new());
static IND_VAL_HANDLE: AtomicU16 = AtomicU16::new(0);
static RECV_VAL_HANDLE: AtomicU16 = AtomicU16::new(0);
static NEEDS_ADV: AtomicBool = AtomicBool::new(false);

fn on_host_event(event: HostEvent) {
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

    0
}

fn start_advertising<S>(driver: &Ble<S>) -> Result<(), BleError> {
    driver.set_device_name(DEVICE_NAME)?;
    driver.adv_set_fields(&BleAdvFields {
        flags: 0x06,
        name: Some(DEVICE_NAME),
        ..Default::default()
    })?;
    driver.adv_start(
        0,
        &BleAdvParams {
            conn_mode: 2,
            disc_mode: 2,
            itvl_min: 0x30,
            itvl_max: 0x60,
            ..Default::default()
        },
    )
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

    // The runtime-built service table: same shape as `gatt_services!` in
    // `gatt_server.rs`, but constructed at runtime. The definitions are
    // borrowed only for the `new` call, which copies them into exact-size
    // C-heap allocations; the driver then owns the result.
    let characteristics = [
        BleGattCharacteristic::new(RECV_CHARACTERISTIC_UUID, enum_set!(BleGattCharFlag::Write)),
        BleGattCharacteristic::new(
            IND_CHARACTERISTIC_UUID,
            enum_set!(BleGattCharFlag::Indicate),
        ),
    ];
    let services =
        BleGattServices::new(&[BleGattService::new(true, SERVICE_UUID, &characteristics)])?;

    let driver = Ble::new_with_services(services)?;
    driver.host_subscribe(&on_host_event);
    driver.gap_subscribe(&on_gap_event);
    driver.gatts_subscribe(&on_gatts_event);

    info!("NimBLE host starting on hci{dev}");

    embassy_futures::select::select(driver.run(controller), async {
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
