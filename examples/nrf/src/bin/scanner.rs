//! The shared scanner example on the nRF - see
//! `nimble_rs_examples_app::scanner`.
//!
//! Run with `cargo run --release --bin scanner` on the default nRF52840;
//! see `nimble_rs_examples_nrf` for the other chips.

#![no_std]
#![no_main]

use embassy_executor::Spawner;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let sdc = nimble_rs_examples_nrf::controller(spawner);
    nimble_rs_examples_app::scanner::run(sdc, None).await
}
