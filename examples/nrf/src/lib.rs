//! Shared board bring-up for the nRF52840 examples: heap, MPSL, and the
//! nrf-sdc SoftDevice Controller - which implements the stock bt-hci
//! typed-command traits natively, so it satisfies `nimble_rs::Controller`
//! as-is (no transport, no adapter). No parker is injected by the examples:
//! the built-in `WfeParker` default applies.

#![no_std]

use core::mem::MaybeUninit;
use core::ptr::addr_of_mut;

use defmt::unwrap;
use embassy_executor::Spawner;
use embassy_nrf::mode::Async;
use embassy_nrf::peripherals::RNG;
use embassy_nrf::{bind_interrupts, rng};
use embedded_alloc::LlffHeap;
use nrf_sdc::mpsl::MultiprotocolServiceLayer;
use nrf_sdc::{self as sdc, mpsl};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _, tinyrlibc as _};

// The C heap backing `nimble-rs`'s `use-c-heap`: tinyrlibc's malloc family
// routes to this global allocator. NimBLE's needs are small and bounded
// (mbuf pools, GATT registry - see `nimble_rs::mem`).
#[global_allocator]
static HEAP: LlffHeap = LlffHeap::empty();

const HEAP_SIZE: usize = 16 * 1024;

bind_interrupts!(struct Irqs {
    RNG => rng::InterruptHandler<RNG>;
    EGU0_SWI0 => nrf_sdc::mpsl::LowPrioInterruptHandler;
    CLOCK_POWER => nrf_sdc::mpsl::ClockInterruptHandler;
    RADIO => nrf_sdc::mpsl::HighPrioInterruptHandler;
    TIMER0 => nrf_sdc::mpsl::HighPrioInterruptHandler;
    RTC0 => nrf_sdc::mpsl::HighPrioInterruptHandler;
});

#[embassy_executor::task]
async fn mpsl_task(mpsl: &'static MultiprotocolServiceLayer<'static>) -> ! {
    mpsl.run().await
}

/// Brings the board up and returns the SoftDevice Controller (advertising,
/// peripheral, scanning and central roles enabled - the examples use
/// different subsets).
pub fn controller(spawner: Spawner) -> nrf_sdc::SoftdeviceController<'static> {
    {
        static mut HEAP_MEM: [MaybeUninit<u8>; HEAP_SIZE] = [MaybeUninit::uninit(); HEAP_SIZE];
        unsafe { HEAP.init(addr_of_mut!(HEAP_MEM) as usize, HEAP_SIZE) }
    }

    let p = embassy_nrf::init(Default::default());

    let mpsl_p =
        mpsl::Peripherals::new(p.RTC0, p.TIMER0, p.TEMP, p.PPI_CH19, p.PPI_CH30, p.PPI_CH31);
    let lfclk_cfg = mpsl::raw::mpsl_clock_lfclk_cfg_t {
        source: mpsl::raw::MPSL_CLOCK_LF_SRC_RC as u8,
        rc_ctiv: mpsl::raw::MPSL_RECOMMENDED_RC_CTIV as u8,
        rc_temp_ctiv: mpsl::raw::MPSL_RECOMMENDED_RC_TEMP_CTIV as u8,
        accuracy_ppm: mpsl::raw::MPSL_DEFAULT_CLOCK_ACCURACY_PPM as u16,
        skip_wait_lfclk_started: mpsl::raw::MPSL_DEFAULT_SKIP_WAIT_LFCLK_STARTED != 0,
    };
    static MPSL: StaticCell<MultiprotocolServiceLayer> = StaticCell::new();
    let mpsl = MPSL.init(unwrap!(mpsl::MultiprotocolServiceLayer::new(
        mpsl_p, Irqs, lfclk_cfg
    )));
    spawner.spawn(unwrap!(mpsl_task(&*mpsl)));

    let sdc_p = sdc::Peripherals::new(
        p.PPI_CH17, p.PPI_CH18, p.PPI_CH20, p.PPI_CH21, p.PPI_CH22, p.PPI_CH23, p.PPI_CH24,
        p.PPI_CH25, p.PPI_CH26, p.PPI_CH27, p.PPI_CH28, p.PPI_CH29,
    );

    static RNG_CELL: StaticCell<rng::Rng<'static, Async>> = StaticCell::new();
    let rng = RNG_CELL.init(rng::Rng::new(p.RNG, Irqs));

    // Sized per nrf-sdc's own report for this role configuration (it warns
    // with the exact requirement when the buffer is off)
    static SDC_MEM: StaticCell<sdc::Mem<3312>> = StaticCell::new();
    let sdc_mem = SDC_MEM.init(sdc::Mem::new());

    unwrap!(sdc::Builder::new()
        .and_then(|builder| builder
            .support_adv()
            .support_peripheral()
            .support_scan()
            .support_central()
            .peripheral_count(1)?
            .central_count(1))
        .and_then(|builder| builder.build(sdc_p, rng, mpsl, sdc_mem)))
}
