//! A BLE scanner over a Linux HCI controller: prints advertisement reports
//! for 10 seconds. (No esp-idf-svc counterpart - its NimBLE wrapper has no
//! scanning support; nimble-rs adds it.)
//!
//! Usage: `scanner [hci-index]` (default 0). The HCI device must be down and
//! the process needs `CAP_NET_ADMIN`.

use log::info;

use nimble_rs::gap::{BleDiscParams, GapEvent};
use nimble_rs::{Ble, ForTransport, HostEvent};

use core::sync::atomic::{AtomicBool, Ordering};

static SYNCED: AtomicBool = AtomicBool::new(false);
static DONE: AtomicBool = AtomicBool::new(false);

fn on_host_event(event: HostEvent) {
    if event == HostEvent::Sync {
        SYNCED.store(true, Ordering::Relaxed);
    }
}

fn on_gap_event(event: GapEvent) -> i32 {
    match event {
        GapEvent::Discovery {
            event_type,
            addr,
            rssi,
            data,
        } => info!("[{rssi} dBm] {addr:?} type={event_type} data={data:02x?}"),
        GapEvent::DiscoveryComplete { reason } => {
            info!("scan complete (reason {reason})");
            DONE.store(true, Ordering::Relaxed);
        }
        _ => {}
    }

    0
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

    let driver = Ble::new()?;
    driver.host_subscribe(&on_host_event);
    driver.gap_subscribe(&on_gap_event);

    info!("scanning on hci{dev} for 10s...");

    embassy_futures::select::select(driver.run(controller), async {
        while !SYNCED.load(Ordering::Relaxed) {
            embassy_time::Timer::after_millis(10).await;
        }

        driver
            .disc(
                0, // BLE_OWN_ADDR_PUBLIC
                Some(10_000),
                &BleDiscParams {
                    passive: false,
                    filter_duplicates: true,
                    ..Default::default()
                },
            )
            .expect("disc");

        while !DONE.load(Ordering::Relaxed) {
            embassy_time::Timer::after_millis(50).await;
        }
    })
    .await;

    Ok(())
}
