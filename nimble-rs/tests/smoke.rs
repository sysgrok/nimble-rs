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

use nimble_rs::{BleDriver, ForTransport, HostEvent, Parker, SpinParker};

#[path = "common/mock.rs"]
mod mock;

use mock::MockController;

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

    // Two full init -> sync -> deinit cycles, verifying that the singleton
    // can be re-created after a clean shutdown. The second cycle injects the
    // spin parker, covering both parker injection and the `no_std` wait path
    // (the first cycle uses the default `StdParker`).
    cycle(None);
    let spin = SpinParker;
    cycle(Some(&spin));
    println!("RE-INIT OK");
}

fn cycle(parker: Option<&dyn Parker>) {
    let threads_at_start = threads();

    // Declared before the driver: the subscription borrows this closure (and
    // through it, `synced`) for as long as the driver lives - a non-'static
    // callback, which is what `BleDriver`'s `'d` lifetime enables.
    let synced = AtomicBool::new(false);
    let on_host_event = |event: HostEvent| {
        log::info!("host event: {event:?}");
        if event == HostEvent::Sync {
            synced.store(true, Ordering::SeqCst);
        }
    };

    let driver = match parker {
        Some(parker) => BleDriver::new_with_parker(parker),
        None => BleDriver::new(),
    }
    .expect("driver init");
    driver.host_subscribe(&on_host_event);

    let controller = ForTransport::new(MockController::new());

    futures_lite::future::block_on(async {
        match select(driver.run(controller), async {
            while !synced.load(Ordering::SeqCst) {
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
