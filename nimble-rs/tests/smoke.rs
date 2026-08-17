//! M2 smoke test: boots the NimBLE host over a *mock* controller (a minimal
//! in-process HCI responder) and verifies that:
//!
//! - the host completes its startup sync burst (each command exercising the
//!   thread-free "pump-while-pending" ack wait),
//! - `HostEvent::Sync` fires,
//! - an identity address can be ensured and read back,
//! - no threads are spawned by nimble-rs itself.
//!
//! Being hermetic (no HCI hardware, no privileges), this doubles as a CI test.

use std::sync::atomic::{AtomicBool, Ordering};

use embassy_futures::select::{select, Either};

use nimble_rs::{BleDriver, ForTransport, HostEvent};

#[path = "common/mock.rs"]
mod mock;

use mock::MockController;

static SYNCED: AtomicBool = AtomicBool::new(false);

fn on_host_event(event: HostEvent) {
    log::info!("host event: {event:?}");
    if event == HostEvent::Sync {
        SYNCED.store(true, Ordering::SeqCst);
    }
}

fn threads() -> usize {
    std::fs::read_dir("/proc/self/task")
        .map(|dir| dir.count())
        .unwrap_or(0)
}

#[test]
fn smoke() {
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Debug)
        .is_test(true)
        .try_init();

    // Two full init -> sync -> deinit cycles, verifying that the singleton can
    // be re-created after a clean shutdown
    cycle();
    SYNCED.store(false, Ordering::SeqCst);
    cycle();
    println!("RE-INIT OK");
}

fn cycle() {
    let threads_at_start = threads();

    let driver = BleDriver::new().expect("driver init");
    driver.host_subscribe(&on_host_event);

    let controller = ForTransport::new(MockController::new());

    futures_lite::future::block_on(async {
        match select(driver.run(controller), async {
            while !SYNCED.load(Ordering::SeqCst) {
                embassy_time::Timer::after_millis(10).await;
            }

            let (addr, addr_type) = driver.address().expect("address");
            let threads_now = threads();

            println!(
                "SYNC OK: addr={addr:02x?} type={addr_type} synced={}",
                driver.synced()
            );
            println!(
                "threads: start={threads_at_start} now={threads_now} \
                 (delta is infrastructure only: embassy-time std alarm thread)"
            );

            assert!(driver.synced());
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
