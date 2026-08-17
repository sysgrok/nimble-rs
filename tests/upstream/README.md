# nimble-rs-upstream-tests

The **upstream** NimBLE host unit test suite (`esp-nimble/nimble/host/test`, ~208 cases).

It runs against nimble-rs' porting layer - NPL locks and timers, the vendored 64-bit `os_mempool`, the msys pools and the init sequence:

```sh
cargo run -p nimble-rs-upstream-tests
```
