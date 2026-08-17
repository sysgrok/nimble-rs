//! Shared board bring-up for the Raspberry Pi Pico W examples: heap, the
//! cyw43 radio, and its Bluetooth HCI transport wrapped in the stock
//! `bt_hci::controller::ExternalController`. No parker is injected by the
//! examples: the built-in `WfeParker` default applies.

#![no_std]

use core::mem::MaybeUninit;
use core::ptr::addr_of_mut;

use bt_hci::controller::ExternalController;
use cyw43::aligned_bytes;
use cyw43_pio::PioSpi;
use defmt::unwrap;
use embassy_executor::Spawner;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, PIO0};
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_rp::{bind_interrupts, dma};
use embedded_alloc::LlffHeap;
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _, tinyrlibc as _};

/// The controller every example runs on.
pub type Controller = ExternalController<cyw43::bluetooth::BtDriver<'static>, 1>;

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

#[embassy_executor::task]
async fn cyw43_task(
    runner: cyw43::Runner<
        'static,
        cyw43::SpiBus<Output<'static>, PioSpi<'static, PIO0, 0>>,
        cyw43::Cyw43439,
    >,
) -> ! {
    runner.run().await
}

/// Brings the board up and returns the Bluetooth controller.
pub async fn controller(spawner: Spawner) -> Controller {
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
    spawner.spawn(unwrap!(cyw43_task(runner)));
    control.init(clm).await;

    ExternalController::new(bt_device)
}
