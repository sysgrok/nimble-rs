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

## Quickstart (Linux)

The examples run over any Linux HCI controller - a real adapter or a virtual
one from BlueZ's `btvirt`:

```sh
sudo apt install bluez-test-tools     # provides btvirt
sudo modprobe hci_vhci
sudo btvirt -l2 &                     # two virtual LE controllers: hci0, hci1
```

The transport binds `HCI_CHANNEL_USER`, which needs the device *down* and
`CAP_NET_ADMIN` (run via `sudo`, or
`sudo setcap cap_net_admin+ep target/debug/<bin>`; for real adapters also
`sudo hciconfig hciX down` first - `btvirt` devices start down).

```sh
cargo build -p nimble-rs-examples-std

sudo ./target/debug/gatt_server 0     # advertise + serve GATT on hci0
sudo ./target/debug/gatt_client 1     # scan, connect and subscribe from hci1
sudo ./target/debug/scanner 0         # just scan
sudo ./target/debug/l2cap server 0    # L2CAP CoC echo server ...
sudo ./target/debug/l2cap client 1    # ... and its client
```

Watch the HCI traffic with `sudo btmon`. The `*_smoke` binaries are hermetic
self-tests against an in-process mock controller - no hardware, no
privileges, used as the CI gates:

```sh
./target/debug/smoke                  # host boots + syncs, thread-free
./target/debug/gatts_smoke            # full GATT server exchange
./target/debug/gattc_smoke            # scan/connect/client + L2CAP echo
```

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
