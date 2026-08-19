//! Shared board bring-up for the Raspberry Pi Pico W examples: heap, the cyw43 radio, and its
//! Bluetooth HCI transport. No parker is injected by the examples: the built-in `WfeParker`
//! default applies.
//!
//! The transport is *not* the stock `bt_hci::controller::ExternalController` alone - see the
//! [`cyw43_adapter`] module for the wrapper it needs and why, and note that this is also the
//! reason the radio runner below is not spawned as its own task.

#![no_std]

use core::future::poll_fn;
use core::mem::MaybeUninit;
use core::pin::pin;
use core::ptr::addr_of_mut;

use bt_hci::controller::ExternalController;
use cyw43::aligned_bytes;
use cyw43_pio::PioSpi;
use embassy_futures::select::select;
use embassy_futures::select::Either::{First, Second};
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, PIO0};
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_rp::{bind_interrupts, dma};
use embedded_alloc::LlffHeap;
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _, tinyrlibc as _};

use crate::cyw43_adapter::{never_ending, Cyw43Controller, SharedRunner};

pub mod cyw43_adapter;

/// The controller every example runs on.
pub type Controller<'a> =
    Cyw43Controller<'a, ExternalController<cyw43::bluetooth::BtDriver<'static>, 1>>;

// The C heap backing `nimble-rs`'s `use-c-heap` (see the nrf example)
#[global_allocator]
static HEAP: LlffHeap = LlffHeap::empty();

const HEAP_SIZE: usize = 16 * 1024;

// ARM RTABI helpers for unaligned accesses, which LLVM emits on ARMv6-M for
// `ptr::read_unaligned`/`write_unaligned` of multi-byte values (bt-hci's
// packet parsing inlines those) but `compiler-builtins` does not provide.
mod aeabi_unaligned {
    #[no_mangle]
    extern "C" fn __aeabi_uread4(address: *const u8) -> u32 {
        let mut b = [0u8; 4];
        unsafe { core::ptr::copy_nonoverlapping(address, b.as_mut_ptr(), 4) };
        u32::from_ne_bytes(b)
    }

    #[no_mangle]
    extern "C" fn __aeabi_uwrite4(value: u32, address: *mut u8) -> u32 {
        let b = value.to_ne_bytes();
        unsafe { core::ptr::copy_nonoverlapping(b.as_ptr(), address, 4) };
        value
    }

    #[no_mangle]
    extern "C" fn __aeabi_uread8(address: *const u8) -> u64 {
        let mut b = [0u8; 8];
        unsafe { core::ptr::copy_nonoverlapping(address, b.as_mut_ptr(), 8) };
        u64::from_ne_bytes(b)
    }

    #[no_mangle]
    extern "C" fn __aeabi_uwrite8(value: u64, address: *mut u8) -> u64 {
        let b = value.to_ne_bytes();
        unsafe { core::ptr::copy_nonoverlapping(b.as_ptr(), address, 8) };
        value
    }
}

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => InterruptHandler<PIO0>;
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>, dma::InterruptHandler<DMA_CH1>;
});

/// Brings the board up and runs `scenario` with the Bluetooth controller.
///
/// The radio runner is *not* spawned as its own task: it is pinned here and shared with the
/// controller (see [`Cyw43Controller`]), because NimBLE's HCI command-ack waits withhold the
/// executor and a spawned runner would not be polled across them.
pub async fn run<S>(scenario: S)
where
    S: for<'a> AsyncFnOnce(Controller<'a>),
{
    {
        static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        unsafe { HEAP.init(addr_of_mut!(HEAP_MEM) as usize, HEAP_SIZE) }
    }

    let p = embassy_rp::init(Default::default());

    let fw = aligned_bytes!("../cyw43-firmware/43439A0.bin");
    let clm = aligned_bytes!("../cyw43-firmware/43439A0_clm.bin");
    let btfw = aligned_bytes!("../cyw43-firmware/43439A0_btfw.bin");
    let nvram = aligned_bytes!("../cyw43-firmware/nvram_rp2040.bin");

    let pwr = Output::new(p.PIN_23, Level::Low);
    let cs = Output::new(p.PIN_25, Level::High);
    let mut pio = Pio::new(p.PIO0, Irqs);
    let spi = PioSpi::new(
        &mut pio.common,
        pio.sm0,
        cyw43_pio::DEFAULT_CLOCK_DIVIDER,
        pio.irq0,
        cs,
        p.PIN_24,
        p.PIN_29,
        dma::Channel::new(p.DMA_CH0, Irqs),
        dma::Channel::new(p.DMA_CH1, Irqs),
    );

    static STATE: StaticCell<cyw43::State> = StaticCell::new();
    let state = STATE.init(cyw43::State::new());
    let (_net_device, bt_device, mut control, runner) =
        cyw43::new_with_bluetooth(state, pwr, spi, fw, btfw, nvram).await;

    let mut runner_fut = pin!(never_ending(runner.run()));
    let runner = SharedRunner::new(runner_fut.as_mut());

    // `Control` talks to the chip through the ioctl channel the runner services, so - just like
    // the HCI transport - it only completes while the runner is polled alongside it.
    let mut main = pin!(async {
        control.init(clm).await;

        scenario(Cyw43Controller::new(
            ExternalController::new(bt_device),
            &runner,
        ))
        .await
    });

    // The runner arm goes last, so that it re-registers the executor's waker with the runner's I/O
    // after every poll of the scenario - repairing the registration whenever a pump-while-pending
    // wait has polled the runner with the parker's waker instead.
    let mut runner_arm = pin!(poll_fn(|cx| runner.poll(cx)));

    match select(&mut main, &mut runner_arm).await {
        First(()) => (),
        Second(never) => match never {},
    }
}
