//! The portable part of the examples - the same scenarios for every target;
//! only the controller (and optionally a [`Parker`](nimble_rs::Parker))
//! differ:
//!
//! - [`gatt_server`]: advertises; a write and an indicate characteristic,
//!   pushing a counter to subscribers once a second.
//! - [`gatt_client`]: scans for the server above, connects, discovers,
//!   subscribes to the indications and writes a counter back.
//! - [`scanner`]: logs advertisement reports, forever.

#![no_std]

use core::sync::atomic::Ordering;

use nimble_rs::BleUuid;

// Logging front-end: `log` and/or `defmt`, whichever the target example
// enables; nothing otherwise. Values must be plain integers/strings (the
// lowest common denominator of both backends); richer data goes through
// `defmt::Debug2Format` in per-backend blocks instead.
macro_rules! info {
    ($s:literal $(, $arg:expr)* $(,)?) => {{
        #[cfg(feature = "log")]
        ::log::info!($s $(, $arg)*);
        #[cfg(feature = "defmt")]
        ::defmt::info!($s $(, $arg)*);
        #[cfg(not(any(feature = "log", feature = "defmt")))]
        let _ = ($($arg,)*);
    }};
}

macro_rules! warning {
    ($s:literal $(, $arg:expr)* $(,)?) => {{
        #[cfg(feature = "log")]
        ::log::warn!($s $(, $arg)*);
        #[cfg(feature = "defmt")]
        ::defmt::warn!($s $(, $arg)*);
        #[cfg(not(any(feature = "log", feature = "defmt")))]
        let _ = ($($arg,)*);
    }};
}

/// The device name the server advertises and the client scans for.
pub const DEVICE_NAME: &str = "nimble-rs";

/// Our service UUID
pub const SERVICE_UUID: BleUuid = BleUuid::uuid128(0xad91b201734740479e173bed82d75f9d);
/// Our "recv" characteristic - i.e. where clients can send data.
pub const RECV_CHARACTERISTIC_UUID: BleUuid = BleUuid::uuid128(0xb6fccb5087be44f3ae22f85485ea42c4);
/// Our "indicate" characteristic - clients receive the counter if they subscribe.
pub const IND_CHARACTERISTIC_UUID: BleUuid = BleUuid::uuid128(0x503de214868246c4828fd59144da41be);

pub mod gatt_client;
pub mod gatt_server;
pub mod scanner;

/// Polls a flag every few milliseconds until it becomes true.
async fn wait(flag: &core::sync::atomic::AtomicBool) {
    while !flag.load(Ordering::Relaxed) {
        embassy_time::Timer::after_millis(10).await;
    }
}
