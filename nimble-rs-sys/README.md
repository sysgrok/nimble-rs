# nimble-rs-sys

Raw FFI bindings to the [esp-nimble](https://github.com/espressif/esp-nimble) BLE host.

Compile-time configuration (Mynewt *syscfg*, i.e. `MYNEWT_VAL_*`) is driven by Cargo features.

## Bundled C code

The crate bundles (as a git submodule) and compiles:

- [esp-nimble](https://github.com/espressif/esp-nimble) — Apache License 2.0 (see its `LICENSE`
  and `NOTICE` files);
- [tinycrypt](https://github.com/intel/tinycrypt) (vendored inside esp-nimble under `ext/`) —
  BSD-style license (see `ext/tinycrypt/LICENSE`).
