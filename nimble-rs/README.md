# nimble-rs

Safe, `no_std`, cross-platform Rust wrappers for the [esp-nimble](https://github.com/espressif/esp-nimble)
BLE host (Espressif's fork of [Apache NimBLE](https://github.com/apache/mynewt-nimble)),
running over any [`bt-hci`](https://crates.io/crates/bt-hci) controller.

**Status: work in progress.** See [../docs/PLAN.md](docs/PLAN.md) for the full design document.

## Highlights

- **Any bt-hci controller.** The host is generic over `bt_hci::controller::Controller`.
- **Baremetal-friendly.** The whole stack runs on a single async executor. NimBLE's internal blocking
  (the HCI command-ack wait) is handled by pumping the HCI bridge *inside* the wait ("pump-while-pending");
- **No allocator required.** `no_std` core with statically-allocated C and intrusive/inline Rust data structures;
  An optional `alloc` feature adds boxed-closure conveniences.
- **Portability surface**: an async controller + an `embassy-time` driver + `critical-section`. Nothing else.
