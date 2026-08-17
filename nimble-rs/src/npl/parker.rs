//! The waiting primitive behind blocking NPL operations ("pump-while-pending").
//!
//! A blocked C caller (e.g. `ble_npl_sem_pend` awaiting an HCI command ack)
//! needs three things, abstracted by the [`Parker`] trait: an identity for
//! ownership tracking, a `Waker` it can hand out, and a way to sleep until
//! that waker is woken (or a deadline passes).
//!
//! Two implementations ship with the crate: [`SpinParker`] (the universal
//! `no_std` fallback) and, with the `std` feature, [`StdParker`] (real
//! `thread::park_timeout`, no spinning). Platforms with better primitives
//! (WFE/SEV, an esp-rtos semaphore, ...) implement [`Parker`] themselves and
//! inject it at driver construction
//! ([`Ble::new_with_parker`](crate::Ble::new_with_parker)); the
//! active parker lives in a process-wide slot because the NPL entry points
//! are reached from C with no driver reference in hand.

use core::task::{RawWaker, RawWakerVTable, Waker};

use embassy_time::Instant;

/// How a blocked NPL operation sleeps and is woken.
///
/// Implementations must be cheap to call repeatedly: the pump-while-pending
/// loop consults the parker on every iteration.
///
/// Methods take `&self` (and the trait requires `Sync`) because several
/// contexts can be parked at once - e.g. one thread blocked in a command-ack
/// wait while another contends the host mutex, whose pend also parks.
/// Implementations that need mutable state keep it in a
/// `critical_section::Mutex<Cell<...>>` or atomics.
pub trait Parker: Sync {
    /// An opaque identity of the *calling* execution context (thread, RTOS
    /// task, ...), used for recursive-mutex ownership tracking. Never 0, and
    /// unique among contexts that call into the driver concurrently.
    fn ctx_id(&self) -> usize;

    /// A `Waker` that unparks the *calling* context. Wakes may arrive from
    /// interrupt handlers or other threads.
    fn waker(&self) -> Waker;

    /// Sleeps the calling context until its waker is woken or `deadline`
    /// passes (`None`: no deadline).
    ///
    /// The deadline is an *upper* bound: implementations must not sleep past
    /// it, but may return early - even immediately - and spuriously; callers
    /// re-check their condition (and the deadline) in a loop.
    fn park(&self, deadline: Option<Instant>);
}

impl<P: Parker + ?Sized> Parker for &P {
    fn ctx_id(&self) -> usize {
        (**self).ctx_id()
    }

    fn waker(&self) -> Waker {
        (**self).waker()
    }

    fn park(&self, deadline: Option<Instant>) {
        (**self).park(deadline)
    }
}

/// The universal fallback parker: never sleeps, making the wait loop a busy
/// poll with a [`spin_loop`](core::hint::spin_loop) hint.
///
/// Correct on every target, at the cost of burning CPU for the duration of a
/// wait (bounded by the HCI command-ack round-trip). Ignoring the deadline
/// is sound: `park` may return immediately by contract, and the caller's
/// loop enforces the timeout.
pub struct SpinParker;

impl Parker for SpinParker {
    fn ctx_id(&self) -> usize {
        1
    }

    fn waker(&self) -> Waker {
        // The wait loop re-polls unconditionally, so the waker need not do
        // anything.
        noop_waker()
    }

    fn park(&self, _deadline: Option<Instant>) {
        core::hint::spin_loop();
    }
}

const NOOP_VTABLE: RawWakerVTable = RawWakerVTable::new(
    |_| RawWaker::new(core::ptr::null(), &NOOP_VTABLE),
    |_| {},
    |_| {},
    |_| {},
);

fn noop_waker() -> Waker {
    unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &NOOP_VTABLE)) }
}

/// The hosted parker: parks the calling *thread* (`thread::park_timeout`),
/// woken by the transport's reactor/callback side. No spinning.
#[cfg(feature = "std")]
pub struct StdParker;

#[cfg(feature = "std")]
impl Parker for StdParker {
    fn ctx_id(&self) -> usize {
        std::thread_local! {
            static CTX: u8 = const { 0 };
        }

        CTX.with(|ctx| ctx as *const _ as usize)
    }

    fn waker(&self) -> Waker {
        use std::sync::Arc;
        use std::task::Wake;
        use std::thread::{self, Thread};

        struct ThreadWaker(Thread);

        impl Wake for ThreadWaker {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }

            fn wake_by_ref(self: &Arc<Self>) {
                self.0.unpark();
            }
        }

        Waker::from(Arc::new(ThreadWaker(thread::current())))
    }

    fn park(&self, deadline: Option<Instant>) {
        match deadline {
            Some(deadline) => {
                let now = Instant::now();
                if deadline > now {
                    let remaining = deadline - now;
                    std::thread::park_timeout(core::time::Duration::from_micros(
                        remaining.as_micros(),
                    ));
                }
            }
            None => std::thread::park(),
        }
    }
}

//
// The active parker
//

static mut ACTIVE: Option<&'static dyn Parker> = None;

fn default_parker() -> &'static dyn Parker {
    #[cfg(feature = "std")]
    {
        static DEFAULT: StdParker = StdParker;
        &DEFAULT
    }
    #[cfg(not(feature = "std"))]
    {
        static DEFAULT: SpinParker = SpinParker;
        &DEFAULT
    }
}

/// Installs (or, with `None`, resets) the active parker. Called from driver
/// construction/teardown.
pub(crate) fn set_active(parker: Option<&'static dyn Parker>) {
    super::with_cs(|| unsafe { *core::ptr::addr_of_mut!(ACTIVE) = parker });
}

fn active() -> &'static dyn Parker {
    super::with_cs(|| unsafe { *core::ptr::addr_of!(ACTIVE) }).unwrap_or_else(default_parker)
}

pub(crate) fn ctx_id() -> usize {
    active().ctx_id()
}

pub(crate) fn current_waker() -> Waker {
    active().waker()
}

pub(crate) fn park(deadline: Option<Instant>) {
    active().park(deadline)
}
