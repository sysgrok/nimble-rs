# nimble-rs

> Safe, async, cross-platform Rust wrappers for the [esp-nimble](https://github.com/espressif/esp-nimble)
> Bluetooth LE host (Espressif's fork of [Apache NimBLE](https://github.com/apache/mynewt-nimble)),
> running over any [`bt-hci`](https://crates.io/crates/bt-hci) controller.

**Status: work in progress.** See [docs/PLAN.md](docs/PLAN.md) for the full design document.

## What is this?

A Rust BLE *host* stack built by wrapping the mature NimBLE C host, decoupled from ESP-IDF:

- **`nimble-rs-sys`** — raw FFI bindings to esp-nimble, compiled from source (git submodule) by
  `build.rs` via the `cc` crate. Compile-time configuration (Mynewt *syscfg*, i.e. `MYNEWT_VAL_*`)
  is driven by Cargo features.
- **`nimble-rs`** — the safe API: GAP (advertising *and* scanning), GATT server & client,
  L2CAP CoC, security manager / bonding. Modeled on the NimBLE API of
  [`esp-idf-svc`](https://github.com/esp-rs/esp-idf-svc).
- **`examples/std`** — host examples (Linux) driving a real or virtual (BlueZ `btvirt`) controller.
- **`tests`** — E2E test driver binaries (advertiser/scanner, GATT server/client pairs, L2CAP echo).

## Highlights

- **Thread-free.** The whole stack runs on a single async executor: one `run()` future plus your
  tasks. NimBLE's internal blocking (the HCI command-ack wait) is handled by an NPL port that
  pumps the HCI bridge *inside* the wait ("pump-while-pending") — no OS threads are created,
  ever. See docs/PLAN.md § "The concurrency design".
- **No allocator required.** `no_std` core with statically-allocated C (upstream-style static
  arrays) and intrusive/inline Rust data structures. An optional `alloc` feature adds boxed-closure
  conveniences.
- **Any bt-hci controller.** The driver is generic over `NimbleController`
  (= async `bt_hci::controller::Controller` + one raw-command method), with a built-in adapter
  for any `bt_hci::transport::Transport` (H4 UART, Linux HCI sockets, USB, ESP VHCI) and a
  planned adapter for `nrf-sdc`.
- **Portability surface**: an async controller + an `embassy-time` driver + `critical-section`.
  Nothing else.

## License

The Rust code in this repository is licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

### Bundled C code

The `nimble-rs-sys` crate bundles (as a git submodule) and compiles:

- [esp-nimble](https://github.com/espressif/esp-nimble) — Apache License 2.0 (see its `LICENSE`
  and `NOTICE` files);
- [tinycrypt](https://github.com/intel/tinycrypt) (vendored inside esp-nimble under `ext/`) —
  BSD-style license (see `ext/tinycrypt/LICENSE`).
