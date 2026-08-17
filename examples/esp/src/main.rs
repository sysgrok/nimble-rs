//! The shared GATT server example on Espressif chips (default: ESP32-C6),
//! over the esp-radio Bluetooth connector wrapped in the stock
//! `bt_hci::controller::ExternalController`.
//!
//! Also demonstrates a custom [`Parker`]: `EspRtosParker` below parks the
//! calling esp-rtos task on its thread semaphore instead of busy-polling.
//!
//! Run with e.g. `cargo esp32c6` (needs `espflash` and an attached board).

#![no_std]
#![no_main]

use core::task::{RawWaker, RawWakerVTable, Waker};

use bt_hci::controller::ExternalController;
use embassy_executor::Spawner;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::ble::controller::BleConnector;
use esp_radio_rtos_driver::semaphore::SemaphoreHandle;

use nimble_rs::Parker;

esp_bootloader_esp_idf::esp_app_desc!();

/// A [`Parker`] over esp-rtos: parks the calling task on its *thread
/// semaphore* (a per-task binary semaphore the RTOS maintains), so a blocked
/// HCI-ack wait sleeps in the scheduler instead of spinning.
///
/// The semaphore latches like a counting primitive: a wake landing between
/// the caller's condition re-check and the `take` leaves it given, so the
/// `take` falls straight through - no lost wake-ups.
struct EspRtosParker;

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

#[esp_rtos::main]
async fn main(_spawner: Spawner) {
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
    let controller = ExternalController::<_, 1>::new(connector);

    static PARKER: EspRtosParker = EspRtosParker;
    nimble_rs_examples_app::gatt_server(controller, Some(&PARKER)).await
}
