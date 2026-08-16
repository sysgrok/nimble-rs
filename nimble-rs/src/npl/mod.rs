//! The NimBLE Porting Layer (NPL), implemented in Rust - thread-free and
//! allocation-free.
//!
//! The C side sees opaque, inline, 8-aligned word-array shells (declared in
//! `nimble-rs-sys/gen/glue/include/nimble/nimble_npl_os.h`); the `#[repr(C)]`
//! types here assert at compile time that they fit those shells and are
//! constructed in place by the `ble_npl_*_init` functions.
//!
//! Concurrency model (see `docs/PLAN.md`, "The concurrency design"):
//! - All shared state is guarded by `critical-section`.
//! - Event queues and callouts integrate with async Rust through stored
//!   `Waker`s; the driver's `run()` future services them.
//! - The single genuinely-blocking operation in the C host is
//!   `ble_npl_sem_pend` (the HCI command-ack wait). It is implemented as
//!   **pump-while-pending**: the wait loop manually polls the registered HCI
//!   pump (TX *and* RX) so that the ack can arrive - and thus release the
//!   semaphore - while the C caller sits parked on this very stack frame.
//! - Mutexes are recursive and owner-tracked; in a single-context setup they
//!   never truly contend. Cross-thread contention (std, app calls from
//!   another thread) degrades to the same pump-park wait.

pub(crate) mod parker;

use core::ffi::{c_int, c_void};
use core::task::{Poll, Waker};

use embassy_time::Instant;

use nimble_rs_sys as sys;

use crate::hci;

// The NPL error codes, mirroring `enum ble_npl_error`.
const OK: sys::ble_npl_error_t = sys::ble_npl_error_BLE_NPL_OK;
const EINVAL: sys::ble_npl_error_t = sys::ble_npl_error_BLE_NPL_INVALID_PARAM;
const TIMEOUT: sys::ble_npl_error_t = sys::ble_npl_error_BLE_NPL_TIMEOUT;

const FOREVER: sys::ble_npl_time_t = sys::ble_npl_time_t::MAX;

/// Runs a closure inside the global critical section that guards all NPL state.
fn with_cs<R>(f: impl FnOnce() -> R) -> R {
    critical_section::with(|_| f())
}

/// Converts an NPL tick count (milliseconds) into an absolute deadline.
/// `BLE_NPL_TIME_FOREVER` means "no deadline".
fn deadline(ticks: sys::ble_npl_time_t) -> Option<Instant> {
    (ticks != FOREVER).then(|| Instant::now() + embassy_time::Duration::from_millis(ticks as _))
}

/// The pump-while-pending wait loop: waits until `ready` returns `true` or the
/// deadline passes, driving the HCI pump in between so that the very packets
/// which would satisfy `ready` can keep flowing while this (C) stack frame is
/// blocked. `register` is called with the waker that will be woken by either
/// the release path or the pump's underlying I/O.
fn wait_until(
    deadline: Option<Instant>,
    mut ready: impl FnMut() -> bool,
    mut register: impl FnMut(&Waker),
) -> bool {
    loop {
        if ready() {
            return true;
        }

        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return false;
        }

        let waker = parker::current_waker();

        // Register before re-checking, so a release between the check and the
        // park cannot be lost.
        register(&waker);

        if ready() {
            return true;
        }

        // Drive the HCI bridge; if it cannot make progress right now it will
        // have registered `waker` with its I/O, so parking is safe.
        hci::pump_manual(&waker);

        if ready() {
            return true;
        }

        parker::park(deadline);
    }
}

//
// Events
//

#[repr(C)]
struct Event {
    next: *mut Event,
    func: sys::ble_npl_event_fn,
    arg: *mut c_void,
    queued: bool,
}

const _: () = assert!(core::mem::size_of::<Event>() <= core::mem::size_of::<sys::ble_npl_event>());
const _: () = assert!(core::mem::align_of::<Event>() <= 8);

unsafe fn event(ev: *mut sys::ble_npl_event) -> *mut Event {
    ev.cast()
}

#[no_mangle]
unsafe extern "C" fn ble_npl_event_init(
    ev: *mut sys::ble_npl_event,
    func: sys::ble_npl_event_fn,
    arg: *mut c_void,
) {
    event(ev).write(Event {
        next: core::ptr::null_mut(),
        func,
        arg,
        queued: false,
    });
}

#[no_mangle]
unsafe extern "C" fn ble_npl_event_deinit(ev: *mut sys::ble_npl_event) {
    let ev = event(ev);
    with_cs(|| {
        (*ev).func = None;
        (*ev).queued = false;
    });
}

#[no_mangle]
unsafe extern "C" fn ble_npl_event_is_queued(ev: *mut sys::ble_npl_event) -> bool {
    with_cs(|| (*event(ev)).queued)
}

#[no_mangle]
unsafe extern "C" fn ble_npl_event_get_arg(ev: *mut sys::ble_npl_event) -> *mut c_void {
    (*event(ev)).arg
}

#[no_mangle]
unsafe extern "C" fn ble_npl_event_set_arg(ev: *mut sys::ble_npl_event, arg: *mut c_void) {
    (*event(ev)).arg = arg;
}

#[no_mangle]
unsafe extern "C" fn ble_npl_event_run(ev: *mut sys::ble_npl_event) {
    if let Some(func) = (*event(ev)).func {
        func(ev);
    }
}

//
// Event queues
//

#[repr(C)]
struct EventQueue {
    /// Non-NULL exactly while the queue is initialized. This is the C-visible
    /// `eventq` member which the esp-nimble fork's `ble_hs.c` null-checks
    /// directly (see the note in `nimble_npl_os.h`).
    init_tag: *mut c_void,
    head: *mut Event,
    tail: *mut Event,
    waker: Option<Waker>,
}

const _: () =
    assert!(core::mem::size_of::<EventQueue>() <= core::mem::size_of::<sys::ble_npl_eventq>());

unsafe fn eventq(evq: *mut sys::ble_npl_eventq) -> *mut EventQueue {
    evq.cast()
}

#[no_mangle]
unsafe extern "C" fn ble_npl_eventq_init(evq: *mut sys::ble_npl_eventq) {
    eventq(evq).write(EventQueue {
        init_tag: evq.cast(),
        head: core::ptr::null_mut(),
        tail: core::ptr::null_mut(),
        waker: None,
    });
}

#[no_mangle]
unsafe extern "C" fn ble_npl_eventq_deinit(evq: *mut sys::ble_npl_eventq) {
    let q = eventq(evq);
    let waker = with_cs(|| {
        (*q).init_tag = core::ptr::null_mut();
        (*q).head = core::ptr::null_mut();
        (*q).tail = core::ptr::null_mut();
        (*q).waker.take()
    });
    if let Some(waker) = waker {
        waker.wake();
    }
}

#[no_mangle]
unsafe extern "C" fn ble_npl_eventq_put(
    evq: *mut sys::ble_npl_eventq,
    ev: *mut sys::ble_npl_event,
) {
    let q = eventq(evq);
    let ev = event(ev);

    let waker = with_cs(|| {
        if (*ev).queued || (*q).init_tag.is_null() {
            return None;
        }

        (*ev).queued = true;
        (*ev).next = core::ptr::null_mut();

        if (*q).tail.is_null() {
            (*q).head = ev;
        } else {
            (*(*q).tail).next = ev;
        }
        (*q).tail = ev;

        (*q).waker.take()
    });

    if let Some(waker) = waker {
        waker.wake();
    }
}

unsafe fn eventq_try_pop(q: *mut EventQueue) -> *mut Event {
    with_cs(|| {
        let ev = (*q).head;
        if !ev.is_null() {
            (*q).head = (*ev).next;
            if (*q).head.is_null() {
                (*q).tail = core::ptr::null_mut();
            }
            (*ev).next = core::ptr::null_mut();
            (*ev).queued = false;
        }
        ev
    })
}

/// Polls the queue for the next event, registering `waker` when empty.
/// Used by the driver's `run()` future (the Rust replacement of the
/// `nimble_port_run` loop).
pub(crate) unsafe fn eventq_poll(
    evq: *mut sys::ble_npl_eventq,
    waker: &Waker,
) -> Poll<*mut sys::ble_npl_event> {
    let q = eventq(evq);

    let ev = with_cs(|| {
        let ev = (*q).head;
        if ev.is_null() {
            (*q).waker = Some(waker.clone());
        }
        ev
    });

    if ev.is_null() {
        Poll::Pending
    } else {
        // Pop outside the registration branch to keep the fast path short
        Poll::Ready(eventq_try_pop(q).cast())
    }
}

#[no_mangle]
unsafe extern "C" fn ble_npl_eventq_get(
    evq: *mut sys::ble_npl_eventq,
    tmo: sys::ble_npl_time_t,
) -> *mut sys::ble_npl_event {
    // The C host itself never calls this (verified; the only caller was the
    // excluded `nimble_port.c` loop, replaced by an async loop in Rust), but
    // implement it faithfully for completeness.
    let q = eventq(evq);
    let mut ev = eventq_try_pop(q);

    if ev.is_null() && tmo != 0 {
        wait_until(
            deadline(tmo),
            || {
                ev = eventq_try_pop(q);
                !ev.is_null()
            },
            |waker| {
                with_cs(|| (*q).waker = Some(waker.clone()));
            },
        );
    }

    ev.cast()
}

#[no_mangle]
unsafe extern "C" fn ble_npl_eventq_remove(
    evq: *mut sys::ble_npl_eventq,
    ev: *mut sys::ble_npl_event,
) {
    let q = eventq(evq);
    let ev = event(ev);

    with_cs(|| {
        if !(*ev).queued {
            return;
        }

        let mut prev: *mut Event = core::ptr::null_mut();
        let mut cur = (*q).head;
        while !cur.is_null() {
            if cur == ev {
                if prev.is_null() {
                    (*q).head = (*cur).next;
                } else {
                    (*prev).next = (*cur).next;
                }
                if (*q).tail == cur {
                    (*q).tail = prev;
                }
                (*ev).next = core::ptr::null_mut();
                (*ev).queued = false;
                break;
            }
            prev = cur;
            cur = (*cur).next;
        }
    });
}

#[no_mangle]
unsafe extern "C" fn ble_npl_eventq_is_empty(evq: *mut sys::ble_npl_eventq) -> bool {
    with_cs(|| (*eventq(evq)).head.is_null())
}

//
// Mutexes (recursive, owner-tracked)
//

#[repr(C)]
struct Mutex {
    owner: usize,
    count: u32,
}

const _: () = assert!(core::mem::size_of::<Mutex>() <= core::mem::size_of::<sys::ble_npl_mutex>());

unsafe fn mutex(mu: *mut sys::ble_npl_mutex) -> *mut Mutex {
    mu.cast()
}

#[no_mangle]
unsafe extern "C" fn ble_npl_mutex_init(mu: *mut sys::ble_npl_mutex) -> sys::ble_npl_error_t {
    mutex(mu).write(Mutex { owner: 0, count: 0 });
    OK
}

#[no_mangle]
unsafe extern "C" fn ble_npl_mutex_deinit(mu: *mut sys::ble_npl_mutex) -> sys::ble_npl_error_t {
    mutex(mu).write(Mutex { owner: 0, count: 0 });
    OK
}

#[no_mangle]
unsafe extern "C" fn ble_npl_mutex_pend(
    mu: *mut sys::ble_npl_mutex,
    tmo: sys::ble_npl_time_t,
) -> sys::ble_npl_error_t {
    let m = mutex(mu);
    let me = parker::ctx_id();

    let try_lock = || {
        with_cs(|| {
            if (*m).owner == 0 {
                (*m).owner = me;
                (*m).count = 1;
                true
            } else if (*m).owner == me {
                (*m).count += 1;
                true
            } else {
                false
            }
        })
    };

    if try_lock() {
        return OK;
    }

    // Contention: only possible when a second thread uses the driver API (std).
    // There is no per-mutex waker; rely on the pump/park loop's re-checks.
    if wait_until(deadline(tmo), try_lock, |_| ()) {
        OK
    } else {
        TIMEOUT
    }
}

#[no_mangle]
unsafe extern "C" fn ble_npl_mutex_release(mu: *mut sys::ble_npl_mutex) -> sys::ble_npl_error_t {
    let m = mutex(mu);
    let me = parker::ctx_id();

    with_cs(|| {
        if (*m).owner != me || (*m).count == 0 {
            EINVAL
        } else {
            (*m).count -= 1;
            if (*m).count == 0 {
                (*m).owner = 0;
            }
            OK
        }
    })
}

//
// Semaphores
//

#[repr(C)]
struct Sem {
    count: u16,
    waker: Option<Waker>,
}

const _: () = assert!(core::mem::size_of::<Sem>() <= core::mem::size_of::<sys::ble_npl_sem>());

unsafe fn sem(s: *mut sys::ble_npl_sem) -> *mut Sem {
    s.cast()
}

#[no_mangle]
unsafe extern "C" fn ble_npl_sem_init(
    s: *mut sys::ble_npl_sem,
    tokens: u16,
) -> sys::ble_npl_error_t {
    sem(s).write(Sem {
        count: tokens,
        waker: None,
    });
    OK
}

#[no_mangle]
unsafe extern "C" fn ble_npl_sem_deinit(s: *mut sys::ble_npl_sem) -> sys::ble_npl_error_t {
    let s = sem(s);
    with_cs(|| {
        (*s).count = 0;
        (*s).waker.take()
    });
    OK
}

#[no_mangle]
unsafe extern "C" fn ble_npl_sem_get_count(s: *mut sys::ble_npl_sem) -> u16 {
    with_cs(|| (*sem(s)).count)
}

#[no_mangle]
unsafe extern "C" fn ble_npl_sem_release(s: *mut sys::ble_npl_sem) -> sys::ble_npl_error_t {
    let s = sem(s);
    let waker = with_cs(|| {
        (*s).count += 1;
        (*s).waker.take()
    });
    if let Some(waker) = waker {
        waker.wake();
    }
    OK
}

#[no_mangle]
unsafe extern "C" fn ble_npl_sem_pend(
    s: *mut sys::ble_npl_sem,
    tmo: sys::ble_npl_time_t,
) -> sys::ble_npl_error_t {
    let s = sem(s);

    let try_take = || {
        with_cs(|| {
            if (*s).count > 0 {
                (*s).count -= 1;
                true
            } else {
                false
            }
        })
    };

    if try_take() {
        return OK;
    }

    if tmo == 0 {
        return TIMEOUT;
    }

    // This is *the* blocking point of the C host (the HCI command-ack wait in
    // `ble_hs_hci_wait_for_ack`); `wait_until` pumps the HCI bridge so the
    // releasing packet can arrive while we sit on this C stack frame.
    if wait_until(deadline(tmo), try_take, |waker| {
        with_cs(|| (*s).waker = Some(waker.clone()));
    }) {
        OK
    } else {
        TIMEOUT
    }
}

//
// Callouts (timers)
//

#[repr(C)]
struct Callout {
    next: *mut Callout,
    deadline: Instant,
    active: bool,
    evq: *mut sys::ble_npl_eventq,
    event: Event,
}

const _: () =
    assert!(core::mem::size_of::<Callout>() <= core::mem::size_of::<sys::ble_npl_callout>());

unsafe fn callout(co: *mut sys::ble_npl_callout) -> *mut Callout {
    co.cast()
}

/// The intrusive, deadline-sorted list of active callouts, plus the waker of
/// whoever services them (the driver's `run()` future).
struct Timers {
    head: *mut Callout,
    waker: Option<Waker>,
}

// Guarded by the global critical section.
unsafe impl Send for Timers {}

static TIMERS: critical_section::Mutex<core::cell::RefCell<Timers>> =
    critical_section::Mutex::new(core::cell::RefCell::new(Timers {
        head: core::ptr::null_mut(),
        waker: None,
    }));

fn with_timers<R>(f: impl FnOnce(&mut Timers) -> R) -> R {
    critical_section::with(|cs| f(&mut TIMERS.borrow_ref_mut(cs)))
}

unsafe fn timers_unlink(timers: &mut Timers, co: *mut Callout) {
    let mut prev: *mut Callout = core::ptr::null_mut();
    let mut cur = timers.head;
    while !cur.is_null() {
        if cur == co {
            if prev.is_null() {
                timers.head = (*cur).next;
            } else {
                (*prev).next = (*cur).next;
            }
            (*co).next = core::ptr::null_mut();
            break;
        }
        prev = cur;
        cur = (*cur).next;
    }
}

#[no_mangle]
unsafe extern "C" fn ble_npl_callout_init(
    co: *mut sys::ble_npl_callout,
    evq: *mut sys::ble_npl_eventq,
    func: sys::ble_npl_event_fn,
    arg: *mut c_void,
) -> c_int {
    callout(co).write(Callout {
        next: core::ptr::null_mut(),
        deadline: Instant::MIN,
        active: false,
        evq,
        event: Event {
            next: core::ptr::null_mut(),
            func,
            arg,
            queued: false,
        },
    });
    0
}

#[no_mangle]
unsafe extern "C" fn ble_npl_callout_deinit(co: *mut sys::ble_npl_callout) {
    ble_npl_callout_stop(co);
}

#[no_mangle]
unsafe extern "C" fn ble_npl_callout_reset(
    co: *mut sys::ble_npl_callout,
    ticks: sys::ble_npl_time_t,
) -> sys::ble_npl_error_t {
    let co = callout(co);
    let deadline = Instant::now() + embassy_time::Duration::from_millis(ticks as _);

    let waker = with_timers(|timers| {
        if (*co).active {
            timers_unlink(timers, co);
        }

        (*co).deadline = deadline;
        (*co).active = true;

        // Sorted insert (earliest deadline first)
        let mut prev: *mut Callout = core::ptr::null_mut();
        let mut cur = timers.head;
        while !cur.is_null() && (*cur).deadline <= deadline {
            prev = cur;
            cur = (*cur).next;
        }
        (*co).next = cur;
        if prev.is_null() {
            timers.head = co;
        } else {
            (*prev).next = co;
        }

        timers.waker.take()
    });

    if let Some(waker) = waker {
        waker.wake();
    }

    OK
}

#[no_mangle]
unsafe extern "C" fn ble_npl_callout_stop(co: *mut sys::ble_npl_callout) {
    let co = callout(co);

    with_timers(|timers| {
        if (*co).active {
            timers_unlink(timers, co);
            (*co).active = false;
        }
    });

    // Also cancel a fired-but-not-yet-run event, mirroring the OS ports
    let evq = (*co).evq;
    if !evq.is_null() {
        ble_npl_eventq_remove(evq, core::ptr::addr_of_mut!((*co).event).cast());
    }
}

#[no_mangle]
unsafe extern "C" fn ble_npl_callout_is_active(co: *mut sys::ble_npl_callout) -> bool {
    with_timers(|_| (*callout(co)).active)
}

#[no_mangle]
unsafe extern "C" fn ble_npl_callout_get_ticks(
    co: *mut sys::ble_npl_callout,
) -> sys::ble_npl_time_t {
    (*callout(co)).deadline.as_millis() as _
}

#[no_mangle]
unsafe extern "C" fn ble_npl_callout_remaining_ticks(
    co: *mut sys::ble_npl_callout,
    now: sys::ble_npl_time_t,
) -> sys::ble_npl_time_t {
    let deadline = (*callout(co)).deadline.as_millis() as u32;
    deadline.saturating_sub(now)
}

#[no_mangle]
unsafe extern "C" fn ble_npl_callout_set_arg(co: *mut sys::ble_npl_callout, arg: *mut c_void) {
    (*callout(co)).event.arg = arg;
}

/// The earliest active callout deadline, registering `waker` for changes.
/// Used by the driver's `run()` future.
pub(crate) fn timers_poll_next_deadline(waker: &Waker) -> Option<Instant> {
    with_timers(|timers| {
        timers.waker = Some(waker.clone());
        if timers.head.is_null() {
            None
        } else {
            Some(unsafe { (*timers.head).deadline })
        }
    })
}

/// Fires every expired callout by enqueueing its event (or running it inline
/// when the callout has no queue). Used by the driver's `run()` future.
pub(crate) fn timers_fire_due() {
    loop {
        let co = with_timers(|timers| {
            let co = timers.head;
            if !co.is_null() && unsafe { (*co).deadline } <= Instant::now() {
                unsafe {
                    timers.head = (*co).next;
                    (*co).next = core::ptr::null_mut();
                    (*co).active = false;
                }
                co
            } else {
                core::ptr::null_mut()
            }
        });

        if co.is_null() {
            break;
        }

        unsafe {
            let ev = core::ptr::addr_of_mut!((*co).event);
            let evq = (*co).evq;
            if evq.is_null() {
                ble_npl_event_run(ev.cast());
            } else {
                ble_npl_eventq_put(evq, ev.cast());
            }
        }
    }
}

//
// Time
//

#[no_mangle]
extern "C" fn ble_npl_time_get() -> sys::ble_npl_time_t {
    Instant::now().as_millis() as _
}

#[no_mangle]
unsafe extern "C" fn ble_npl_time_ms_to_ticks(
    ms: u32,
    out_ticks: *mut sys::ble_npl_time_t,
) -> sys::ble_npl_error_t {
    *out_ticks = ms;
    OK
}

#[no_mangle]
unsafe extern "C" fn ble_npl_time_ticks_to_ms(
    ticks: sys::ble_npl_time_t,
    out_ms: *mut u32,
) -> sys::ble_npl_error_t {
    *out_ms = ticks;
    OK
}

#[no_mangle]
extern "C" fn ble_npl_time_ms_to_ticks32(ms: u32) -> sys::ble_npl_time_t {
    ms
}

#[no_mangle]
extern "C" fn ble_npl_time_ticks_to_ms32(ticks: sys::ble_npl_time_t) -> u32 {
    ticks
}

#[no_mangle]
extern "C" fn ble_npl_time_delay(ticks: sys::ble_npl_time_t) {
    wait_until(deadline(ticks), || false, |_| ());
}

//
// Critical sections & misc
//

struct CsState {
    owner: usize,
    depth: u32,
    restore: Option<critical_section::RestoreState>,
}

// Only mutated by the critical-section owner.
static mut CS_STATE: CsState = CsState {
    owner: 0,
    depth: 0,
    restore: None,
};

#[no_mangle]
unsafe extern "C" fn ble_npl_hw_enter_critical() -> u32 {
    let me = parker::ctx_id();
    let state = &raw mut CS_STATE;

    if (*state).owner == me {
        (*state).depth += 1;
    } else {
        let restore = critical_section::acquire();
        (*state).owner = me;
        (*state).depth = 1;
        (*state).restore = Some(restore);
    }

    0
}

#[no_mangle]
unsafe extern "C" fn ble_npl_hw_exit_critical(_ctx: u32) {
    let state = &raw mut CS_STATE;

    debug_assert!((*state).owner == parker::ctx_id() && (*state).depth > 0);

    (*state).depth -= 1;
    if (*state).depth == 0 {
        (*state).owner = 0;
        if let Some(restore) = (*state).restore.take() {
            critical_section::release(restore);
        }
    }
}

#[no_mangle]
unsafe extern "C" fn ble_npl_hw_is_in_critical() -> bool {
    let state = &raw mut CS_STATE;
    (*state).owner == parker::ctx_id()
}

#[no_mangle]
extern "C" fn ble_npl_hw_set_isr(_irqn: c_int, _addr: u32) {
    // Controller-only; never called by the host.
    unreachable!("ble_npl_hw_set_isr is a controller-side API");
}

#[no_mangle]
extern "C" fn ble_npl_os_started() -> bool {
    true
}

#[no_mangle]
extern "C" fn ble_npl_get_current_task_id() -> *mut c_void {
    parker::ctx_id() as _
}
