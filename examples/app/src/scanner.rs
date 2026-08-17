//! The scanner scenario: log advertisement reports, forever (10-second scan
//! rounds with duplicate filtering).

use core::sync::atomic::{AtomicBool, Ordering};

use nimble_rs::gap::{BleDiscParams, GapEvent};
use nimble_rs::{Ble, Controller, HostEvent, Parker};

static SYNCED: AtomicBool = AtomicBool::new(false);
static ROUND_DONE: AtomicBool = AtomicBool::new(false);

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
        } => {
            #[cfg(feature = "log")]
            ::log::info!("[{rssi} dBm] {addr:?} type={event_type} data={data:02x?}");
            #[cfg(feature = "defmt")]
            ::defmt::info!(
                "[{} dBm] {} type={} data={}",
                rssi,
                defmt::Debug2Format(&addr),
                event_type,
                defmt::Debug2Format(&data),
            );
            #[cfg(not(any(feature = "log", feature = "defmt")))]
            let _ = (event_type, addr, rssi, data);
        }
        GapEvent::DiscoveryComplete { reason } => {
            info!("scan round complete (reason {})", reason);
            ROUND_DONE.store(true, Ordering::Relaxed);
        }
        _ => {}
    }

    0
}

/// Runs the scanner over the given controller, forever. See
/// [`gatt_server::run`](crate::gatt_server::run) for the `parker` parameter.
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

    embassy_futures::select::select(ble.run(controller), async {
        crate::wait(&SYNCED).await;

        // Infers the own-address type - public where the controller has one
        // (Linux, ESP), static random otherwise (nRF)
        let own_addr_type = match ble.address() {
            Ok((_, addr_type)) => addr_type,
            Err(e) => panic!("no identity address: {}", e.code()),
        };

        loop {
            info!("scanning for 10s...");
            ROUND_DONE.store(false, Ordering::Relaxed);

            if let Err(e) = ble.disc(
                own_addr_type,
                Some(10_000),
                &BleDiscParams {
                    passive: false,
                    filter_duplicates: true,
                    ..Default::default()
                },
            ) {
                panic!("disc failed: {}", e.code());
            }

            crate::wait(&ROUND_DONE).await;
        }
    })
    .await;

    panic!("BLE host stopped unexpectedly");
}
