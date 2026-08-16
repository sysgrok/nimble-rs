//! The Rust replacement of esp-nimble's `nimble_port.c` (which is not
//! compiled: it hard-depends on FreeRTOS/ESP-IDF - see `gen/builder.rs` in
//! `nimble-rs-sys`).
//!
//! Provides the small port contract the C host links against
//! (`nimble_port_get_dflt_eventq` plus the init/deinit sequencing), with the
//! `nimble_port_run` event loop replaced by an async one (`run_events`),
//! polled from the driver's `run()` future.

use core::cell::UnsafeCell;
use core::future::{poll_fn, Future};
use core::task::Poll;

use nimble_rs_sys as sys;

use crate::npl;

extern "C" {
    // Declared only inside C sources (no public header); signatures per
    // esp-nimble 039d2d62 `porting/nimble/src/*.c`.
    fn os_mempool_module_init();
    fn os_msys_init();
}

struct DflEventQueue(UnsafeCell<sys::ble_npl_eventq>);

// Internally synchronized (all access to the queue state is under the global
// critical section in the `npl` module).
unsafe impl Sync for DflEventQueue {}

/// The default event queue, the Rust counterpart of `nimble_port.c`'s
/// `g_eventq_dflt` static.
static DFLT_EVQ: DflEventQueue = DflEventQueue(UnsafeCell::new(unsafe { core::mem::zeroed() }));

#[no_mangle]
extern "C" fn nimble_port_get_dflt_eventq() -> *mut sys::ble_npl_eventq {
    DFLT_EVQ.0.get()
}

/// Initializes the NimBLE stack, mirroring the controller-less branch of the
/// C `nimble_port_init`:
/// mempools -> transport buffers -> transport -> default eventq -> msys ->
/// host (`ble_transport_hs_init`) -> our HCI bridge (`ble_transport_ll_init`).
pub(crate) fn init() -> Result<(), ()> {
    unsafe {
        os_mempool_module_init();

        if sys::ble_buf_alloc() != 0 {
            sys::ble_buf_free();
            return Err(());
        }

        sys::ble_transport_init();

        sys::ble_npl_eventq_init(DFLT_EVQ.0.get());

        os_msys_init();

        sys::ble_transport_hs_init();
        sys::ble_transport_ll_init();
    }

    Ok(())
}

/// Deinitializes the NimBLE stack, mirroring the C `nimble_port_deinit`.
pub(crate) fn deinit() {
    unsafe {
        sys::ble_npl_eventq_deinit(DFLT_EVQ.0.get());

        sys::ble_hs_deinit();

        sys::ble_transport_ll_deinit();
        sys::ble_transport_deinit();
        sys::ble_buf_free();
    }
}

/// The async replacement of the C `nimble_port_run` loop: processes events
/// from the default queue forever. All host callbacks (GAP/GATT/... events,
/// including the sync/reset callbacks) run from inside this future's poll.
pub(crate) async fn run_events() -> ! {
    loop {
        let ev = poll_fn(
            |cx| match unsafe { npl::eventq_poll(DFLT_EVQ.0.get(), cx.waker()) } {
                Poll::Ready(ev) => Poll::Ready(ev),
                Poll::Pending => Poll::Pending,
            },
        )
        .await;

        if !ev.is_null() {
            unsafe {
                sys::ble_npl_event_run(ev);
            }
        }
    }
}

/// The async replacement of the callout servicing that OS ports do with timer
/// tasks: sleeps until the earliest active callout expires, then fires all due
/// callouts (by enqueueing their events onto their queues).
pub(crate) async fn run_timers() -> ! {
    loop {
        // Wait until there is a deadline, and it passes. `timers_poll_next_deadline`
        // registers the waker, so a `ble_npl_callout_reset` with an earlier
        // deadline re-polls this future immediately.
        poll_fn(|cx| {
            match npl::timers_poll_next_deadline(cx.waker()) {
                Some(deadline) => {
                    if embassy_time::Instant::now() >= deadline {
                        Poll::Ready(())
                    } else {
                        // Arm an embassy timer for the deadline; re-polled when
                        // it fires or when the deadline set changes.
                        let mut timer = embassy_time::Timer::at(deadline);
                        match core::pin::Pin::new(&mut timer).poll(cx) {
                            Poll::Ready(()) => Poll::Ready(()),
                            Poll::Pending => Poll::Pending,
                        }
                    }
                }
                None => Poll::Pending,
            }
        })
        .await;

        npl::timers_fire_due();
    }
}
