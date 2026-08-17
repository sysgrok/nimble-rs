//! The shared GATT client example on an nRF52840 - see
//! `nimble_rs_examples_app::gatt_client`.
//!
//! Run with `cargo run --release --bin gatt_client`.

#![no_std]
#![no_main]

use embassy_executor::Spawner;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let sdc = nimble_rs_examples_nrf::controller(spawner);
    nimble_rs_examples_app::gatt_client::run(sdc, None).await
}
