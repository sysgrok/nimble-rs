# nimble-rs

Safe, `no_std`, cross-platform Rust wrappers for the [esp-nimble](https://github.com/espressif/esp-nimble)
BLE host (Espressif's fork of [Apache NimBLE](https://github.com/apache/mynewt-nimble)),
running over any [`bt-hci`](https://crates.io/crates/bt-hci) controller.

**Status: work in progress.** See [../docs/PLAN.md](../docs/PLAN.md) for the full design document.

## Highlights

- **Any bt-hci controller.** The host is generic over `bt_hci::controller::Controller`.
- **Baremetal-friendly.** The whole stack runs on a single async executor. NimBLE's internal blocking
  (the HCI command-ack wait) is handled by pumping the HCI bridge *inside* the wait ("pump-while-pending");
- **`no_std` first.** Statically-allocated C and intrusive/inline Rust data structures. The `alloc`
  feature (a default) covers the bounded, config-sized set of init-time allocations the esp-nimble
  fork still makes (mbuf/transport pools, GATT registry) plus boxed-closure conveniences; a static-arena
  backend making the stack fully allocator-free is planned.
- **Portability surface**: an async controller + an `embassy-time` driver + `critical-section`. Nothing else.
