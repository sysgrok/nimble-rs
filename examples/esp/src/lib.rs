//! Shared board bring-up for the Espressif examples (default: ESP32-C6):
//! heap, esp-rtos, and the esp-radio Bluetooth connector wrapped in the
//! stock `bt_hci::controller::ExternalController`.
//!
//! Also home of [`EspRtosParker`], an example of a custom
//! [`Parker`](nimble_rs::Parker): it parks the calling esp-rtos task on its
//! thread semaphore instead of busy-polling.

#![no_std]

use core::task::{RawWaker, RawWakerVTable, Waker};

use bt_hci::controller::ExternalController;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::ble::controller::BleConnector;
use esp_radio_rtos_driver::semaphore::SemaphoreHandle;

use nimble_rs::Parker;

/// The controller every example runs on.
pub type Controller = ExternalController<BleConnector<'static>, 1>;

/// A [`Parker`] over esp-rtos: parks the calling task on its *thread
/// semaphore* (a per-task binary semaphore the RTOS maintains), so a blocked
/// HCI-ack wait sleeps in the scheduler instead of spinning.
///
/// The semaphore latches like a counting primitive: a wake landing between
/// the caller's condition re-check and the `take` leaves it given, so the
/// `take` falls straight through - no lost wake-ups.
pub struct EspRtosParker;

impl Parker for EspRtosParker {
    fn ctx_id(&self) -> usize {
        esp_radio_rtos_driver::current_task().as_ptr() as usize
    }

    fn waker(&self) -> Waker {
        const VTABLE: RawWakerVTable =
            RawWakerVTable::new(|sem| RawWaker::new(sem, &VTABLE), give, give, |_| {});

        fn give(sem: *const ()) {
            if let Some(ptr) = core::ptr::NonNull::new(sem.cast_mut()) {
                unsafe { SemaphoreHandle::ref_from_ptr(&ptr) }.give();
            }
        }

        let sem = esp_radio_rtos_driver::current_task_thread_semaphore();
        unsafe { Waker::from_raw(RawWaker::new(sem.as_ptr(), &VTABLE)) }
    }

    fn park(&self, deadline: Option<embassy_time::Instant>) {
        let timeout_us = deadline.map(|deadline| {
            let now = embassy_time::Instant::now();
            deadline
                .saturating_duration_since(now)
                .as_micros()
                .min(u32::MAX as u64) as u32
        });

        let sem = esp_radio_rtos_driver::current_task_thread_semaphore();
        unsafe { SemaphoreHandle::ref_from_ptr(&sem) }.take(timeout_us);
    }
}

/// The parker instance the examples inject.
pub static PARKER: EspRtosParker = EspRtosParker;

/// Brings the board up (heap, esp-rtos scheduler) and returns the Bluetooth
/// controller.
pub fn controller() -> Controller {
    esp_println::logger::init_logger_from_env();

    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    // Also the C heap for `nimble-rs`'s `use-c-heap` (esp-alloc exports the
    // C malloc family)
    esp_alloc::heap_allocator!(size: 72 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let software_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);

    esp_rtos::start(timg0.timer0, software_interrupt.software_interrupt0);

    let connector = BleConnector::new(peripherals.BT, Default::default()).unwrap();
    ExternalController::new(connector)
}
