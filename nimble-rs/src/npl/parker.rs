//! The waiting primitive behind blocking NPL operations ("pump-while-pending").
//!
//! A blocked C caller (e.g. `ble_npl_sem_pend` awaiting an HCI command ack)
//! needs two things: an identity for ownership tracking, and a way to sleep
//! until a `Waker` it handed out is woken (or a deadline passes).
//!
//! - With the `std` feature, parking is a real `thread::park_timeout` (no
//!   spinning): wakes arrive from the transport's reactor/callback side or
//!   from another thread releasing the semaphore.
//! - Without `std`, the universal fallback is a spin loop (`spin_loop` hint),
//!   correct on every target. Platform-specific parkers (WFE, esp-rtos
//!   semaphores) can be added later behind features without touching callers.

use core::task::Waker;

use embassy_time::Instant;

/// An opaque identity of the current execution context (thread on `std`,
/// the single context otherwise). Never 0.
pub fn ctx_id() -> usize {
    imp::ctx_id()
}

/// A `Waker` that unparks the *current* context when woken.
pub fn current_waker() -> Waker {
    imp::current_waker()
}

/// Parks the current context until [`current_waker`] is woken or the deadline
/// passes. May also return spuriously; callers must re-check their condition.
pub fn park(deadline: Option<Instant>) {
    imp::park(deadline)
}

#[cfg(feature = "std")]
mod imp {
    use core::task::Waker;

    use std::sync::Arc;
    use std::task::Wake;
    use std::thread::{self, Thread};

    use embassy_time::Instant;

    std::thread_local! {
        static CTX: u8 = const { 0 };
    }

    pub fn ctx_id() -> usize {
        CTX.with(|ctx| ctx as *const _ as usize)
    }

    struct ThreadWaker(Thread);

    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    pub fn current_waker() -> Waker {
        Waker::from(Arc::new(ThreadWaker(thread::current())))
    }

    pub fn park(deadline: Option<Instant>) {
        match deadline {
            Some(deadline) => {
                let now = Instant::now();
                if deadline > now {
                    let remaining = deadline - now;
                    thread::park_timeout(core::time::Duration::from_micros(remaining.as_micros()));
                }
            }
            None => thread::park(),
        }
    }
}

#[cfg(not(feature = "std"))]
mod imp {
    use core::task::{RawWaker, RawWakerVTable, Waker};

    use embassy_time::Instant;

    pub fn ctx_id() -> usize {
        1
    }

    const NOOP_VTABLE: RawWakerVTable = RawWakerVTable::new(
        |_| RawWaker::new(core::ptr::null(), &NOOP_VTABLE),
        |_| {},
        |_| {},
        |_| {},
    );

    pub fn current_waker() -> Waker {
        // The spin-parker re-polls unconditionally, so the waker need not do
        // anything; platform parkers (WFE etc.) will provide a real one.
        unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &NOOP_VTABLE)) }
    }

    pub fn park(_deadline: Option<Instant>) {
        core::hint::spin_loop();
    }
}
