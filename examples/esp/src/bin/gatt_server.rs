//! The shared GATT server example on Espressif chips - see
//! `nimble_rs_examples_app::gatt_server`.
//!
//! Run with e.g. `cargo esp32c6 --bin gatt_server` (see .cargo/config.toml).

#![no_std]
#![no_main]

use embassy_executor::Spawner;

use esp_backtrace as _;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(_spawner: Spawner) {
    let controller = nimble_rs_examples_esp::controller();
    nimble_rs_examples_app::gatt_server::run(controller, Some(&nimble_rs_examples_esp::PARKER))
        .await
}
