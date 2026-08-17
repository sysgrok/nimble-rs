//! The shared GATT client example on a Raspberry Pi Pico W - see
//! `nimble_rs_examples_app::gatt_client`.
//!
//! Run with `cargo run --release --bin gatt_client`.

#![no_std]
#![no_main]

use embassy_executor::Spawner;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let controller = nimble_rs_examples_rp::controller(spawner).await;
    nimble_rs_examples_app::gatt_client::run(controller, None).await
}
