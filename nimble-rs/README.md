# nimble-rs

Safe, `no_std`, cross-platform Rust wrappers for the [esp-nimble](https://github.com/espressif/esp-nimble)
BLE **host** (Espressif's fork of [Apache NimBLE](https://github.com/apache/mynewt-nimble)),
running over any [`bt-hci`](https://crates.io/crates/bt-hci) controller.

## Highlights

- **Any bt-hci controller.** The host is generic over `bt_hci::controller::Controller`.
- **Baremetal-friendly.** The whole stack runs on a single async executor. NimBLE's internal blocking
  (the HCI command-ack wait) is handled by pumping the HCI bridge *inside* the wait ("pump-while-pending");
- **`no_std` first, no Rust allocator.** Statically-allocated C and intrusive/inline Rust data
  structures. The `use-c-heap` feature (a default) routes the bounded, config-sized set of
  init-time allocations the esp-nimble fork still makes (mbuf/transport pools, GATT registry) - and
  the runtime GATT service table builder - to the platform C heap: `libc`, `esp-alloc`,
  `tinyrlibc`, etc.
- **Portability surface**: an async controller + an `embassy-time` driver + `critical-section`. Nothing else.

## Why not [`trouble`](https://github.com/embassy-rs/trouble)?

The only reason: `trouble-host` is not (yet) certified.

Otherwise, **`trouble` is superior to this crate**, in that it is natively async and all in Rust.
And... the author of `numble-rs` is a contributor to `trouble` as well.

The hope is that `nimble-rs` might be easier to certify with Bluetooth Sig - at least on Espressif chips - in that it is based on `esp-nimble` [which is already certified for ESP-IDF](https://qualification.bluetooth.com/ListingDetails/310315).
