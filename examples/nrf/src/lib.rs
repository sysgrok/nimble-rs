//! Shared board bring-up for the nRF examples: heap, MPSL, and the nrf-sdc
//! SoftDevice Controller - which implements the stock bt-hci typed-command
//! traits natively, so it satisfies `nimble_rs::Controller` as-is (no
//! transport, no adapter). No parker is injected by the examples: the
//! built-in `WfeParker` default applies.
//!
//! Exactly one chip feature is active at a time. `nrf52840` is the default,
//! so that one needs no extra flags:
//!
//! ```sh
//! cargo run --release --bin gatt_server
//! ```
//!
//! The nRF54L family runs on a different core (Cortex-M33) and hence a
//! different target, which cargo cannot derive from a feature - pass both,
//! and turn the default chip off:
//!
//! ```sh
//! cargo run --release --bin gatt_server \
//!     --no-default-features --features nrf54l15 \
//!     --target thumbv8m.main-none-eabihf
//! ```
//!
//! `nrf54l05`, `nrf54l10` and `nrf54lm20` build the same way; within the
//! family only the memory map (and the `probe-rs` chip name - see
//! `.cargo/config.toml`) differs.

#![no_std]

use core::mem::MaybeUninit;
use core::ptr::addr_of_mut;

use defmt::unwrap;
use embassy_executor::Spawner;
use embassy_nrf::bind_interrupts;
use embassy_nrf::config::Config;
#[cfg(feature = "_nrf54l")]
use embassy_nrf::cracen;
#[cfg(feature = "_nrf52")]
use embassy_nrf::mode::Async;
#[cfg(feature = "_nrf54l")]
use embassy_nrf::mode::Blocking;
#[cfg(feature = "_nrf52")]
use embassy_nrf::peripherals::RNG;
#[cfg(feature = "_nrf52")]
use embassy_nrf::rng;
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

/// The entropy source the controller is seeded from: a dedicated peripheral
/// on the nRF52, the CRACEN cryptocell on the nRF54L (which is why only the
/// secure domain - `-app-s` - is offered: embassy-nrf exposes CRACEN there).
#[cfg(feature = "_nrf52")]
type Rng = rng::Rng<'static, Async>;
#[cfg(feature = "_nrf54l")]
type Rng = cracen::Cracen<'static, Blocking>;

#[cfg(feature = "_nrf52")]
bind_interrupts!(struct Irqs {
    RNG => rng::InterruptHandler<RNG>;
    EGU0_SWI0 => nrf_sdc::mpsl::LowPrioInterruptHandler;
    CLOCK_POWER => nrf_sdc::mpsl::ClockInterruptHandler;
    RADIO => nrf_sdc::mpsl::HighPrioInterruptHandler;
    TIMER0 => nrf_sdc::mpsl::HighPrioInterruptHandler;
    RTC0 => nrf_sdc::mpsl::HighPrioInterruptHandler;
});

#[cfg(feature = "_nrf54l")]
bind_interrupts!(struct Irqs {
    SWI00 => nrf_sdc::mpsl::LowPrioInterruptHandler;
    CLOCK_POWER => nrf_sdc::mpsl::ClockInterruptHandler;
    RADIO_0 => nrf_sdc::mpsl::HighPrioInterruptHandler;
    TIMER10 => nrf_sdc::mpsl::HighPrioInterruptHandler;
    GRTC_3 => nrf_sdc::mpsl::HighPrioInterruptHandler;
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

    let p = embassy_nrf::init(config());

    #[cfg(feature = "_nrf52")]
    let mpsl_p =
        mpsl::Peripherals::new(p.RTC0, p.TIMER0, p.TEMP, p.PPI_CH19, p.PPI_CH30, p.PPI_CH31);
    #[cfg(feature = "_nrf54l")]
    let mpsl_p = mpsl::Peripherals::new(
        p.GRTC_CH7,
        p.GRTC_CH8,
        p.GRTC_CH9,
        p.GRTC_CH10,
        p.GRTC_CH11,
        p.TIMER10,
        p.TIMER20,
        p.TEMP,
        p.PPI10_CH0,
        p.PPI20_CH1,
        p.PPIB11_CH0,
        p.PPIB21_CH0,
    );

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

    #[cfg(feature = "_nrf52")]
    let sdc_p = sdc::Peripherals::new(
        p.PPI_CH17, p.PPI_CH18, p.PPI_CH20, p.PPI_CH21, p.PPI_CH22, p.PPI_CH23, p.PPI_CH24,
        p.PPI_CH25, p.PPI_CH26, p.PPI_CH27, p.PPI_CH28, p.PPI_CH29,
    );
    #[cfg(feature = "_nrf54l")]
    let sdc_p = sdc::Peripherals::new(
        p.PPI00_CH1,
        p.PPI00_CH3,
        p.PPI10_CH1,
        p.PPI10_CH2,
        p.PPI10_CH3,
        p.PPI10_CH4,
        p.PPI10_CH5,
        p.PPI10_CH6,
        p.PPI10_CH7,
        p.PPI10_CH8,
        p.PPI10_CH9,
        p.PPI10_CH10,
        p.PPI10_CH11,
        p.PPIB00_CH1,
        p.PPIB00_CH2,
        p.PPIB00_CH3,
        p.PPIB10_CH1,
        p.PPIB10_CH2,
        p.PPIB10_CH3,
    );

    static RNG_CELL: StaticCell<Rng> = StaticCell::new();
    #[cfg(feature = "_nrf52")]
    let rng = RNG_CELL.init(rng::Rng::new(p.RNG, Irqs));
    #[cfg(feature = "_nrf54l")]
    let rng = RNG_CELL.init(cracen::Cracen::new_blocking(p.CRACEN));

    // Sized per nrf-sdc's own report for this role configuration: it logs the
    // exact requirement whenever the buffer is off - a buffer that is too
    // small is a hard error, too big only a warning, so the nRF54L figure
    // (where the controller library is a different build) carries headroom
    // until a board reports the real number.
    #[cfg(feature = "_nrf52")]
    const SDC_MEM_SIZE: usize = 3312;
    #[cfg(feature = "_nrf54l")]
    const SDC_MEM_SIZE: usize = 4096;

    static SDC_MEM: StaticCell<sdc::Mem<SDC_MEM_SIZE>> = StaticCell::new();
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

/// The clock setup handed to `embassy_nrf::init`.
///
/// The nRF52840 is happy with embassy's defaults (internal oscillators; MPSL
/// starts the HFXO itself when the radio needs it). The nRF54L application
/// core is brought up the way nrf-sdc's own examples bring it up: 128 MHz off
/// the external crystals, both of which the DKs carry. On a board without a
/// 32 kHz crystal, drop the `lfclk_source` line (MPSL is told to run its own
/// low-frequency clock off the RC oscillator either way, see `lfclk_cfg`).
fn config() -> Config {
    #[allow(unused_mut)]
    let mut config = Config::default();

    #[cfg(feature = "_nrf54l")]
    {
        use embassy_nrf::config::{ClockSpeed, HfclkSource, LfclkSource};

        config.clock_speed = ClockSpeed::CK128;
        config.hfclk_source = HfclkSource::ExternalXtal;
        config.lfclk_source = LfclkSource::ExternalXtal;
    }

    config
}
