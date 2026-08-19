//! The shared scanner example on a Raspberry Pi Pico W - see
//! `nimble_rs_examples_app::scanner`.
//!
//! Run with `cargo run --release --bin scanner`.

#![no_std]
#![no_main]

use embassy_executor::Spawner;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    nimble_rs_examples_rp::run(async |controller| {
        nimble_rs_examples_app::scanner::run(controller, None).await
    })
    .await
}
