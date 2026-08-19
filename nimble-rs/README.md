# nimble-rs

Safe, `no_std`, cross-platform Rust wrappers for the [esp-nimble](https://github.com/espressif/esp-nimble)
BLE **host** (Espressif's fork of [Apache NimBLE](https://github.com/apache/mynewt-nimble)),
running over any [`bt-hci`](https://crates.io/crates/bt-hci) controller.

## Highlights

- **Any bt-hci controller - with a caveat.** The host is generic over `bt_hci::controller::Controller`. See [Caveat](#caveat).
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
And... the author of `nimble-rs` is a contributor to `trouble` as well.

The hope is that `nimble-rs` might be easier to certify with Bluetooth Sig - at least on Espressif chips - in that it is based on `esp-nimble` [which is already certified for ESP-IDF](https://qualification.bluetooth.com/ListingDetails/310315).

## Caveat

**The controller MUST be able to make progress from inside NimBLE's blocking waits.** 

The C host sends an HCI command and then *blocks* the caller until the controller acks it. 

To workaround this, `nimble-rs` turns that into "pump-while-pending" - it drives the HCI bridge from within the wait and then parks 
on that bridge's I/O. But for the duration of the wait the executor belongs to the **blocked caller**, and nothing else on it is polled!

Consequences:
- Controllers that do their I/O in their own futures or in interrupts are unaffected: an in-process controller such as `nrf-sdc` or `esp-radio`, or a
UART/USB HCI transport;
- A controller whose transport is a *separate* future, however, **deadlocks**!

A typical case of a deadlocking controller is `cyw43`, where `BtDriver` is a mere pair of channels and every byte on the wire is moved by
`cyw43::Runner::run` - conventionally spawned as its own task, which is precisely what will **not** be polled while the host waits.

Therefore, the command never reaches the chip and the ack never arrives.
The remedy is to make that future reachable *through* the controller: wrap the controller so that awaiting any HCI operation polls the transport alongside it.

The `cyw43_adapter` module of `examples/rp` is exactly that, and documents the whole trap.

Alternative: inject a `Parker` that does the equivalent before sleeping.
