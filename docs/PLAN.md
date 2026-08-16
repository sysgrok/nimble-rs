# nimble-rs — cross-platform Rust wrapper for the esp-nimble BLE host (thread-free)

## Context

Create a new project `nimble-rs` at `/home/ivan/dev/nimble-rs` (new git repo) wrapping the **esp-nimble** C BLE host (Espressif's fork of Apache NimBLE) in safe, cross-platform Rust, with the host running over the **`bt-hci`** crate's HCI abstraction instead of ESP-IDF's VHCI. Four sub-projects: `nimble-rs-sys`, `nimble-rs`, `examples/std`, `tests`. Structure and build machinery mirror the user's own `openthread` and `mbedtls-rs` repos; the safe API mirrors `esp-idf-svc`'s NimBLE module (`src/ble*`, ~2800 LoC). **No threads anywhere** — single-context async design (user requirement), which also brings no_std/baremetal within reach of the core design rather than a bolted-on backend.

**Answer to the open question ("does esp-nimble have a kconfig equivalent?"):** esp-nimble has **no Kconfig of its own** — it uses Mynewt's **syscfg** system. `$N/porting/nimble/include/syscfg/syscfg.h` is a pre-generated header where *every* value is `#ifndef`-guarded, so any `MYNEWT_VAL_*` can be overridden with `-D` compiler flags (ESP-IDF's Kconfig is just a mapping layer, `esp_nimble_cfg.h`, which we ignore). So the `cfg(esp_idf_*)` switches become **Cargo features → `-DMYNEWT_VAL_*` defines** emitted by `build.rs`.

Reference roots:
- `$N` = `/home/ivan/dev/esp-idf-svc/.embuild/espressif/esp-idf/v5.5.3/components/bt/host/nimble/nimble` (esp-nimble on disk; becomes submodule)
- `$OT` = `/home/ivan/dev/openthread`, `$MB` = `/home/ivan/dev/mbedtls-rs` (structure/gen machinery to lift)
- `$SVC` = `/home/ivan/dev/esp-idf-svc/src` (API to mirror)
- `$BT` = `~/.cargo/registry/src/.../bt-hci-0.9.0`

## Verified architecture facts (drive the design)

- **Host-only build blueprint**: `$N/porting/examples/linux/Makefile` + `$N/porting/nimble/Makefile.defs`. No CMake → use the **`cc` crate**.
- **NPL (OS abstraction)**: ~44 `ble_npl_*` functions declared in `$N/nimble/include/nimble/nimble_npl.h`; the port supplies `nimble/nimble_npl_os.h` defining concrete struct layouts. We supply our own header + implement all symbols **in Rust**.
- **HCI transport**: we implement exactly **5 symbols** from `$N/nimble/transport/include/nimble/transport_impl.h` (`ble_transport_ll_init/deinit`, `to_ll_cmd_impl`, `to_ll_acl_impl`, `to_ll_iso_impl`); RX path calls `ble_transport_alloc_evt/acl_from_ll` + `ble_transport_to_hs_evt/acl`.
- **Complete blocking inventory of the C host** (grep-verified over all compiled sources):
  - `ble_hs_hci.c:554` — `ble_npl_sem_pend(&ble_hs_hci_sem, tmo)`: wait for a command ack. **The only real blocking point.**
  - `nimble_port.c:396/403/420` — stop-sem pends + `eventq_get(FOREVER)` host loop: in the file we **exclude and rewrite in Rust** anyway.
  - No `ble_npl_time_delay`, no other `eventq_get`/`sem_pend` in host/services/util/transport sources. Mutex pends never truly contend in a single-context model (recursive, owner-tracked).
- **The ack path is nested-pump-safe** (verified): `ble_transport_to_hs_evt_impl` = `ble_hs_hci_rx_evt` (ble_hs.c:1037), which either **enqueues** the event for the host loop or, for command-complete/status, calls `ble_hs_hci_rx_ack` — which takes **no locks**: it stores the ack pointer and `ble_npl_sem_release`s. ACL RX likewise only enqueues. So driving HCI RX from *inside* `ble_npl_sem_pend` can only (a) enqueue work for later, (b) release the very semaphore being pended.
- **bt-hci integration point = async `bt_hci::controller::Controller` + a one-method raw-command extension.** The *base* `Controller` trait (`write_acl_data`, `read`) carries everything NimBLE needs except command submission. The *typed* command path (`ControllerCmdSync<C>`, `cmd::Cmd::OPCODE`) is provably insufficient for a C host: commands arrive from NimBLE as raw runtime packets, and — decisive — `SyncCmd::Return` is only `FromHciBytes + Copy` (verified in `$BT/src/cmd.rs:151`), so the raw Command-Complete bytes NimBLE must consume cannot be generically reconstructed from a typed `exec()`. Hence nimble-rs defines `trait NimbleController: bt_hci::controller::Controller { async fn write_cmd(&self, cmd_packet: &[u8]) -> Result<(), Self::Error>; }` with provided impls:
  1. `ForTransport<T: bt_hci::transport::Transport>` newtype — base Controller via `Transport::read/write`, `write_cmd` via a local `RawCmd<'a>(&'a [u8]): WriteHci + HostToControllerPacket { KIND = Cmd }`. Covers `SerialTransport` (H4 UART), `bt-hci-linux` 0.2, `bt-hci-usb`, esp-radio's `BleConnector` — with none of `ExternalController`'s slot machinery (NimBLE does its own ack matching).
  2. **nrf-sdc** (nrf52840/nrf54l15): `SoftdeviceController` already implements base `Controller` (ACL + read over `sdc_hci_data_put`/`sdc_hci_get`); the SDC C API has no generic raw cmd pipe (verified: `sdc_hci.h` exposes only data_put/get; commands are per-command C functions taking **wire-format parameter bytes** directly). `write_cmd` = an opcode→`sdc_hci_cmd_*` dispatch table over the bounded command set NimBLE emits, returning a synthesized raw Command-Complete — exactly the pattern Nordic ships for Zephyr's C host (`hci_internal.c` in sdk-nrf). Lives in a feature-gated adapter (or small companion crate) against `nrf-sdc-sys`.
  3. Any other native `Controller`: implement `write_cmd` (one method) — always possible in practice since every real controller speaks raw HCI at the bottom; the typed-only limitation is a bt-hci trait-shape fact, not a nimble-rs choice.
- **ESP entanglement in the fork** (all verified in-tree): `nimble_port.c` unconditionally includes `soc/soc_caps.h`/FreeRTOS/`esp_log.h` → **exclude it, re-implement in Rust** (its C consumers only use the small contract in `nimble_port.h`); `nimble_port.h` includes `esp_err.h`; `ble_gatts.c`/`ble_l2cap_sig.c`/`transport.c` include `esp_nimble_mem.h`; `ble_hs*.c` include `bt_common.h` (only `BT_HCI_LOG_INCLUDED` consumed); `ble_svc_gap.c` is gated on `MYNEWT_VAL(BLE_GATTS) && CONFIG_BT_NIMBLE_GAP_SERVICE` → must pass `-DCONFIG_BT_NIMBLE_GAP_SERVICE=1`. `$N/nimble/src` does not exist in the fork.
- **Transport topology knobs**: `MYNEWT_VAL_BLE_TRANSPORT_HS__native=1`, `BLE_TRANSPORT_LL__custom=1`, `BLE_TRANSPORT_LL__native=0` ("host here, controller external").
- esp-nimble ships `apps/bttester` (BTP for auto-PTS), `bleprph`, `blecent`, `btshell` etc. — models for the `tests` sub-project.

## The concurrency design (thread-free)

**Why threads looked like a precondition:** every host-initiated HCI command (`ble_gap_adv_start`, the ~10-command sync burst at startup, GATT procedures…) funnels through `ble_hs_hci_cmd_tx`, which sends the command and then parks on `ble_hs_hci_sem` until the controller's Command Complete/Status arrives — inside non-suspendable C (no await points, no `WANT_READ`-style early return à la mbedtls). The sem is released only by the HCI RX path. So *something* must move HCI RX (and TX!) forward while the C caller is parked. A second thread is the brute-force answer; it is not the only one.

**Thread-free resolution — "pump while pending":** since the pend site is unique and its release path is verified lock-free/enqueue-only, the NPL semaphore itself can drive the HCI bridge:

1. **`HciPump`** (in `nimble-rs`): one manually-pollable state machine owning the `NimbleController` and both directions — TX drain (cmd channel with priority → `write_cmd`, then ACL channel → `Controller::write_acl_data`) and RX ingest (`Controller::read` → alloc + `ble_transport_to_hs_*`). Stores the latest waker; safe to poll from two places (poll sites are mutually exclusive by construction in a single-context model; a lock + waker forwarding makes it robust anyway).
2. **`run()` future** polls: the host event loop (async eventq get → `ble_npl_event_run`), the callout timer queue (`embassy-time` `Timer::at` next deadline → `eventq_put`), and `HciPump`. All C execution happens inside `run()`'s poll or inside app API calls on the same executor; C never yields mid-call.
3. **`ble_npl_sem_pend`** = if tokens available, take; else a **scoped `block_on`**: repeatedly poll `HciPump` (TX too — the just-queued command must actually leave!) until the sem releases or the HCI timeout deadline passes (checked against `embassy_time::Instant`). The universal *fallback* mechanism is a **portable spin-poll loop** (`embassy_futures::block_on` shape: poll, `spin_loop` hint, poll again) — pure core Rust, zero platform code, correct on every OS/arch identically. The wait window equals the transport's ack latency (µs for on-chip controllers/sockets, ~0.6–2 ms for 115200-baud H4 UART) and occurs only on command exchanges, never on ACL data flow. A **`Parker` hook** (tiny trait: `park(deadline)` + unpark-from-waker) upgrades the spin to a proper sleep wherever the platform has one, and the common platforms all do:
   - **std** (Linux/macOS/ESP-IDF): `thread::park_timeout` impl ships built-in and is selected by the `std` feature — *no spinning on std, ever*; wakes are delivered by the transport's reactor/ISR side.
   - **esp-hal + esp-radio baremetal**: esp-radio already requires the **esp-rtos** scheduler for its internal tasks — a `Parker` over an esp-rtos semaphore (~20 lines, feature `parker-esp-rtos`) parks the current task while the radio's tasks/ISRs keep producing HCI and firing wakers.
   - **nRF + nrf-sdc / pure-embassy cortex-m**: SDC/MPSL run from ISRs, so a WFE/SEV parker is ideal (feature `parker-cortex-m`).
   - Anything else: user-pluggable impl, or the spin fallback — same portable-trait-with-impls shape as `critical-section`/`embassy-time-driver`, but with a correct universal default, so no platform ever *requires* porting work.
   (Note: `embassy_futures::yield_now` is not an option inside the pend — it only reschedules within an executor task and cannot be used under a blocked C frame.) Callouts don't fire during a pend — acceptable for ms-scale ack round-trips.
4. **`ble_npl_mutex`** = owner-tracked recursive counter. Single-context ⇒ pends always succeed (on `std` builds it's backed by a real recursive lock so multi-threaded *app* use of the driver API remains sound — the pender inside a pump holds locks the RX path provably never takes).
5. **eventq / callout / time** = intrusive event list + waker signal (openthread `signal.rs` pattern; the `ble_npl_event` struct is its own list node, like NimBLE's `os_event` STAILQ), intrusive sorted list of active callouts (no heap), `embassy_time::Instant` ms ticks. `eventq_get(FOREVER)` exists only in our Rust loop (async); C host code never calls it (verified).

**Contract (documented loudly):** while a host API call awaits its command ack, the executor is stalled for that round-trip (µs–ms, bounded by the HCI command timeout). This is the same latency the C design imposes on its calling task anyway — it's just not hidden behind a context switch. No threads are created anywhere; the mandatory platform surface is exactly: an async bt-hci controller (`NimbleController`), an `embassy-time` driver, and `critical-section` — all pre-existing portable-trait ecosystems; this crate adds **no new mandatory porting point** (the `Parker` is optional) and **requires no allocator**.

**No-alloc story (verified):** the whole stack is statically allocated by default.
- *C side*: `BLE_STATIC_TO_DYNAMIC=0` (already pinned) compiles out the fork's static→heap conversion — 25 of the 26 host files using `nimble_platform_mem_*` guard those sites on that knob (with upstream-style static-array `#else` paths, all sized by `MYNEWT_VAL_*`); the remainder sit behind off-by-default feature knobs (EATT `BLE_EATT_CHAN_NUM=0`, `BLE_GATT_CACHING=0`, ATT signed write). The `esp_nimble_mem.h` stub declares `nimble_platform_mem_*` as extern prototypes implemented in Rust: default = panic-with-message (unreachable in the default config), optional static-arena or global-allocator backends behind features for future knobs that need them.
- *Rust side*: NPL objects use **inline fixed-word layouts**, not Box — each `ble_npl_*` struct in our header is `{ void *v[N]; }` (8-aligned), with `#[repr(C)]` Rust impl types + compile-time size/align asserts; this is portable across targets because everything scales with the word size, and per-target pregen bindings capture the exact layout anyway. (The earlier pointer-to-Box rationale died with the threads design — impls are now small POD + `Waker` slots, not `std::sync` objects.) The callout timer queue is an intrusive list threaded through the callout structs (no `BinaryHeap`); HCI bridge queues are static `embassy-sync` channels + `heapless`. Callback subscription in the core takes `&'static` closures/fn-pointers (`StaticCell`-friendly, the no_std idiom); an **optional `alloc` feature** adds the `Box<dyn FnMut>` conveniences and the runtime GATT-service-table builder (esp-idf-svc parity), plus routes `nimble_platform_mem_*` to the global allocator.

**Alternatives considered for the in-C wait (kept out of v0.1):** (a) an *internal* worker thread on std as an opt-in convenience mode — excluded to keep one thread-free code path everywhere; (b) a stackful fiber/coroutine for the C host (own stack; NPL blocking ops switch context back to the async caller; `run()` resumes on wake) — the only design that makes C execution *truly* suspendable with zero busy-wait AND zero threads, but costs per-arch context-switch code (e.g. `corosensei`: no xtensa), C-stack sizing, and FFI-unwinding care; documented as a possible future backend behind the same NPL internals, not v0.1; (c) C-side escape hatches — `BLE_HS_PHONY_HCI_ACKS` (test hook; still needs the ack synchronously) and `BLE_HS_REQUIRE_OS=0` (unit-test-simulator only, gates one code path) — both verified dead ends; (d) rewriting `ble_hs_hci_cmd_tx` to continuation style — a fork-maintenance burden rejected outright.

**Consequence:** no_std/baremetal is the *same* code path (different parker + embassy-time driver), not a future backend — bringing it into scope early is now cheap.

**Prior art (benbrittain/apache-nimble-sys, `port-layer-embassy` — examined):** validates several of our mechanisms but does **not** solve the host-blocking problem: his shipped, working configuration is the NimBLE **controller** (LL) under embassy exposed as a `bt_hci::Controller` for trouble; the `host` feature is README-marked TODO and his port implements `ble_npl_sem_*`/`ble_npl_mutex_*` as `unimplemented!()` (any C-host HCI command would panic in `ble_hs_hci_wait_for_ack`). Shared techniques we adopt with confidence: async Rust rewrite of the `eventq_get(FOREVER)` loops (his `host.rs::nimble_port_run` is exactly our planned loop), not compiling `nimble_port.c`, callouts on `embassy-time` (his RawWaker-on-`schedule_wake` trick is a neat alternative to a timer queue), eventq as channel + waker, nesting-aware critical sections, NPL struct layouts owned by the Rust side (he generates the C header from Rust via cbindgen; we keep a trivial hand-written `{void *impl;}` header + `repr(C)` mirror + size asserts). Our pump-while-pending semaphore is precisely the piece his host TODO is missing.

## Decisions

1. **API style**: mirror `$SVC/ble.rs` (per user: "good enough and fairly complete") — `BleDriver<'ble, S>` + singleton with 5 mutex-guarded callback slots (host/gap/gatts/gattc/l2cap), `subscribe`/`subscribe_nonstatic`, `BleError(c_int)`, `gatt_services!` macro, same module split. Fix its known gaps: **add scanning** (`ble_gap_disc` + `BLE_GAP_EVENT_DISC`), **wire bond store** (`ble_store_ram_init` default + pluggable trait), **surface passkey events**.
2. **`BleDriver::new(config)` takes no modem peripheral**; HCI comes in via `async fn run<C: NimbleController>(&self, controller: C) -> Result<Infallible, BleError>` (openthread `run()` precedent) — any async bt-hci controller: Transport-shaped ones via `ForTransport`, native ones like nrf-sdc via the raw-cmd adapter.
3. **Config**: Cargo features → universe-reset `-DMYNEWT_VAL_*` table (doctrine + fingerprinting cloned from `$OT/openthread-sys/gen/features.rs`; gc-sections cannot remove enabled-but-unused C features). Numerics as additive largest-wins features (`l2cap-coc-<N>`, `max-connections-<N>`, `msys-count-<N>`) — openthread `heap-int-<N>` precedent.
4. **No espidf special case** in nimble-rs-sys (deliberate divergence from openthread-sys/mbedtls-rs-sys): we compile our own esp-nimble everywhere; ESP-IDF's BT component stays off/controller-only.
5. **Crypto**: tinycrypt (`$N/ext/tinycrypt`) always compiled as a second archive; future opt-in mbedtls backend via `MYNEWT_VAL_BLE_CRYPTO_STACK_MBEDTLS`.
6. **Submodule pin**: `https://github.com/espressif/esp-nimble.git` at `039d2d62` (the exact commit IDF v5.5.3 ships, and the tree all design facts were verified against). Master HEAD was tried first but a 2026-05 restructuring moved `porting/nimble/include/os/os_mempool.h` out of esp-nimble into ESP-IDF's `components/bt/porting` — i.e. master currently cannot build standalone without lifting IDF-side headers (risk R2 realized); revisit on the next bump.
7. Licenses: crate `MIT OR Apache-2.0`; README "bundled C code" section (esp-nimble Apache-2.0 + NOTICE, tinycrypt BSD-style) — openthread-sys precedent.

## Repo scaffold

```
nimble-rs/
├── Cargo.toml            # members=[nimble-rs-sys, nimble-rs, examples/std]; exclude=[tests, xtask]
│                         # workspace.deps: bt-hci 0.9, embassy-sync/futures/time, heapless, log, defmt,
│                         # bindgen, cc; edition 2021, rust-version 1.85
├── .gitmodules           # nimble-rs-sys/esp-nimble → espressif/esp-nimble
├── LICENSE-MIT, LICENSE-APACHE, README.md, .github/workflows/ci.yml
├── nimble-rs-sys/
│   ├── Cargo.toml        # links="nimble"; features below; package.exclude prunes submodule
│   ├── build.rs          # clone of $OT/openthread-sys/build.rs flow (track, pregen check,
│   │                     # on-the-fly compile+bindgen, rustc-env NIMBLE_RS_SYS_BINDINGS_FILE)
│   ├── esp-nimble/       # git submodule
│   ├── gen/
│   │   ├── builder.rs    # NimbleBuilder: cc-based compile() + bindgen (lift generate_bindings
│   │   │                 # from $OT/openthread-sys/gen/builder.rs; swap cmake→cc)
│   │   ├── features.rs   # VAL_UNIVERSE reset table + feature map + numerics + prebuilt_validity()
│   │   ├── glue/include/ # esp_err.h, esp_nimble_mem.h, bt_common.h, nimble/nimble_npl_os.h
│   │   ├── include/include.h        # bindgen surface header
│   │   └── sysroot/include/         # fake libc headers copied from $OT (for baremetal clang)
│   ├── src/lib.rs        # #![no_std]; include!(env!("NIMBLE_RS_SYS_BINDINGS_FILE"))
│   ├── src/include/      # pregen per-target bindings (future, via xtask gen)
│   └── libs/             # pregen per-target .a (future)
├── nimble-rs/
│   └── src/{lib,fmt,port,hci,gap,gatt,mbuf,store,l2cap}.rs + npl/mod.rs (+ npl/parker.rs)
│                         # + gatt/{server,client}.rs
├── examples/std/src/bin/ # gatt_server, gatt_server_dynamic, gatt_client, l2cap  (1:1 ports of
│                         # $SVC/../examples/ble_*.rs) + scanner.rs (new); bt-hci-linux + embassy-executor std
├── tests/                # excluded member, publish=false: advertiser/scanner, gatts/gattc,
│                         # l2cap_srv/l2cap_cli paired E2E driver bins
└── xtask/                # `gen <target>` (pregen harvest, lifted from $OT/xtask), `itest` stub
```

## nimble-rs-sys

**Source list** (mirrors `Makefile.defs`, exclusions verified):
`porting/nimble/src/*.c` minus {`hal_timer.c`, `os_cputime*.c`, `hal_uart.c`, `nimble_port.c`}; `nimble/host/src/*.c`; `nimble/host/util/src/*.c`; `nimble/host/services/{gap,gatt}/src/*.c`; `nimble/host/store/ram/src/*.c`; `nimble/transport/src/transport.c` only. Second archive: `ext/tinycrypt/src/*.c` (`-std=c99`). Include order: **glue first** (shadowing), then nimble include dirs, tinycrypt last. Flags: `-ffunction-sections -fdata-sections`, all `-DMYNEWT_VAL_*`, `-DCONFIG_BT_NIMBLE_GAP_SERVICE=1`. **No `-m32`** (see risks).

**Features** (universe-reset; VAL mappings):

| feature | MYNEWT_VALs |
|---|---|
| `peripheral` (→`broadcaster`), `central` (→`observer`), `broadcaster`, `observer` | `BLE_ROLE_*` 0→1 |
| `sm` / `sm-sc-only` | `BLE_SM_LEGACY`+`BLE_SM_SC` / SC only |
| `ext-adv` | `BLE_EXT_ADV=1` |
| `l2cap-coc-{1,2,4,8}` | `BLE_L2CAP_COC_MAX_NUM` (largest wins) |
| `max-connections-{1..32}` | `BLE_MAX_CONNECTIONS` (largest wins; reset 4) |
| `msys-count-{12..64}` | `MSYS_1_BLOCK_COUNT` (largest wins; reset 20) |
| `prebuilt`, `force-generate-bindings`, `use-gcc` | infra (as in $OT) |

Fixed resets: `BLE_ISO=0`, `BLE_MESH=0`, `BLE_HS_FLOW_CTRL=0`, `BLE_STATIC_TO_DYNAMIC=0` (keeps the C host fully statically allocated — see no-alloc story), `BLE_EATT_CHAN_NUM=0`, `MP_RUNTIME_ALLOC=0`, `BLE_QUEUE_CONG_CHECK=0`, `BLE_GATT_CACHING=0`, `BLE_CRYPTO_STACK_MBEDTLS=0`, `BLE_TRANSPORT_HS__native=1`, `BLE_TRANSPORT_LL__custom=1`, `BLE_TRANSPORT_LL__native=0`. default = `["peripheral","central","sm"]`.

**Glue headers** (each stub commented with the C consumer that forces it):
- `esp_err.h`: `typedef int esp_err_t;` + `ESP_OK/ESP_FAIL/ESP_ERR_NO_MEM`
- `esp_nimble_mem.h`: `nimble_platform_mem_{malloc,calloc,realloc,free}` → extern prototypes implemented in Rust; unreachable in the default config (`BLE_STATIC_TO_DYNAMIC=0`): default impl panics with a clear message, optional static-arena / global-allocator backends behind features
- `bt_common.h`: `BT_HCI_LOG_INCLUDED 0` (+`TRUE/FALSE`)
- `nimble/nimble_npl_os.h`: **our NPL ABI** — every `ble_npl_*` struct is an inline word array `{ _Alignas(8) void *v[N]; }` (per-type N, e.g. event≈4, eventq≈6, callout≈10 — final counts pinned by compile-time asserts against the `#[repr(C)]` Rust impl types), `ble_npl_time_t = uint32_t` ms ticks, `BLE_NPL_TIME_FOREVER = UINT32_MAX`. Inline-words chosen over pointer-to-Box: objects are embedded by value in C statics and init'd in place via `ble_npl_*_init`; impls are small POD + `Waker` slots (no `std::sync` types since the thread-free redesign); word-scaling keeps the header target-portable (per-target pregen bindings capture exact layouts anyway); and it needs **no allocator**.

**Bindgen** (`gen/include/include.h`: syscfg, os/os_mbuf/os_mempool, nimble/ble+hci_common+npl+port+transport(+_impl), host/ble_hs umbrella, ble_l2cap, ble_store, ble_hs_stop, util, svc gap/gatt, store/ram): allowlist `ble_.*`, `BLE_.*`, `os_.*`, `OS_.*`, `nimble_port_.*`, `MYNEWT_VAL_.*`, `esp_err_t`; same `-DMYNEWT_VAL_*` passed to clang so bound constants match the compiled config. `use_core()`, `derive_default`, no layout tests, rustfmt — lifted from `$OT` builder.

## nimble-rs

- **`npl/mod.rs`**: all ~44 `#[no_mangle] extern "C" fn ble_npl_*` symbols, thread-free and alloc-free per the concurrency design: inline `#[repr(C)]` impl types matching the header word-arrays (size/align static asserts); mutex = owner-tracked recursive counter (std: real recursive lock underneath); sem = counter + waker + **pump-while-pending** via scoped `block_on` over `HciPump` with deadline; eventq = intrusive event list + waker signal (async get used only by our run loop; sync put from C); callout = intrusive sorted deadline list + `embassy-time`; `critical_enter/exit` = `critical-section`; `now_ms` = `embassy_time::Instant`. `npl/parker.rs`: portable spin-poll default + optional `Parker` trait (std thread-park impl built-in; others pluggable).
- **`port.rs`**: Rust `#[no_mangle]` replacements for excluded `nimble_port.c` — `nimble_port_init` (`os_mempool_module_init` → `ble_buf_alloc` → `ble_transport_init` → dflt eventq → `ble_transport_hs_init` → `ble_transport_ll_init`), `nimble_port_get_dflt_eventq`, and async Rust equivalents of run/stop (the C `nimble_port_run`/`stop` symbols exist for link completeness but the real loop is the async one inside `run()`; stop = `ble_hs_stop` + async join on the stop signal, mirroring the non-ESP branches of `$N/porting/nimble/src/nimble_port.c:203-260`).
- **`hci.rs`**: the `NimbleController` trait + `ForTransport<T>` adapter + the 5 `ble_transport_ll_*` symbols + **`HciPump`**. TX: copy cmd (`heapless::Vec<u8,258>`, depth 2 = cmd pool) / flattened ACL (depth = `MYNEWT_VAL_BLE_TRANSPORT_ACL_FROM_HS_COUNT`) into `embassy_sync` channels, free C buffers immediately (try_send is total: mempool depths bound the producers). `HciPump` drains channels → `write_cmd` (raw, cmd priority) / `Controller::write_acl_data` (`AclPacket::from_hci_bytes`) and ingests `Controller::read` → Event: `ble_transport_alloc_evt` (discardable for adv reports; bounded yield-retry for non-discardable) → `to_hs_evt`; ACL: `alloc_acl_from_ll` + `os_mbuf_append` → `to_hs_acl`; ISO: unsupported. Polled from `run()` and from `sem_pend` (mutually exclusive by construction; lock + waker forwarding regardless).
- **`lib.rs`**: `BleDriver<'ble, S = ()> where S: AsRef<[ble_gatt_svc_def]>`, `static SINGLETON` with 5 slots, trampolines into `ble_hs_cfg` at construction, `call()` clones-Arc-then-drops-guard, `BleError(c_int)` — all mirroring `$SVC/ble.rs`. `new()` → singleton take, `nimble_port_init`, `ble_svc_gap/gatt_init`, `ble_store_ram_init`, gatts add for `S`. `run(controller)` → select(host event loop, callout timers, `HciPump`). Drop → stop/deinit/release.
- **`gap.rs`/`gatt*.rs`/`l2cap.rs`/`mbuf.rs`**: 1:1 ports of the `$SVC/ble/` counterparts (legacy vs ext-adv APIs feature-switched like the cfg'd originals), **plus** `disc()/disc_cancel()` + `GapEvent::Discovery`, `GapEvent::PasskeyAction` + `ble_sm_inject_io` helper, and `store.rs` (`BleStore` trait, RAM default).
- Crate: `#![no_std]`, **no allocator required**; core callback subscription takes `&'static` closures/fn-pointers (`StaticCell`-friendly); optional `alloc` feature adds `Box<dyn FnMut>` subscribe conveniences + the runtime GATT-service-table builder (`BleGattServices`) + global-allocator backend for `nimble_platform_mem_*`. Features `default = ["std","alloc","peripheral","central","gatt-server","gatt-client","sm"]` where `std` adds conveniences (std parker, `std::error::Error`) and implies `alloc`; `gatt-client = ["central"]`, `l2cap = ["nimble-rs-sys/l2cap-coc-2"]`, `ext-adv`, `defmt`/`log` (fmt.rs shim from `$OT`).
- **Documented contract**: host API calls stall the executor for the command-ack round-trip (bounded by the HCI cmd timeout); callbacks run inside `run()`'s poll and may call host APIs.

## Milestones & verification

- **M1 — sys builds/links/binds** (x86_64-linux): full scaffold; `cargo build -p nimble-rs-sys`; `nm` audit — undefined ⊆ {ble_npl_*, nimble_port_*, ble_transport_ll_*, nimble_platform_mem_*, libc, tinycrypt}, zero C++ symbols; feature-matrix compile (default / roles-only / `ext-adv,l2cap-coc-8,max-connections-16`).
- **M2 — host boots & syncs, thread-free**: npl + port + hci + minimal driver; smoke bin against BlueZ `btvirt` (or real hci0) via `bt-hci-linux` reaches `HostEvent::Sync` (this exercises pump-while-pending ~10× during the sync burst), `ble_hs_util_ensure_addr` + `ble_hs_id_copy_addr` yields a valid address; clean stop + re-new; **assert no threads spawned by us** (inspect `/proc/self/task` before/after, modulo reactor threads of the transport's runtime); `cargo check -p nimble-rs --no-default-features --features peripheral,central,sm` proves the **no-std/no-alloc core compiles**; run green under ASAN (64-bit-assumption gate).
- **M3 — GAP adv + GATT server**: `gatt_server` example visible in `bluetoothctl`; connect/read/write/subscribe from nRF Connect; indication round-trip.
- **M4 — GATT client, scanning, L2CAP, SM**: paired processes over `btvirt -l2`; client discovers/reads/writes server; l2cap echo w/ backpressure; just-works + passkey pairing; bond survives reconnect (RAM store).
- **M5 — examples parity + docs**: all five bins run on Linux; ESP-IDF std target compile-check; README quickstart (btvirt, `CAP_NET_ADMIN`).
- **M6 — tests + CI**: `xtask itest` spawns btvirt + paired driver bins, asserts on structured stdout; CI: fmt/clippy, feature matrix, macOS build leg, `i686-unknown-linux-gnu` build leg (32-bit canary), examples, itest (vhci module; continue-on-error initially), ASAN job, `cargo package --list` size check.
- **M7 — nrf-sdc adapter (first native-Controller target)**: the opcode→`sdc_hci_cmd_*` dispatch table (mirroring sdk-nrf's `hci_internal.c` command list, restricted to what NimBLE emits per enabled features) as a feature-gated adapter or companion crate; nrf52840 example bin (adv + GATT server) with the WFE parker; verifies the `NimbleController` abstraction against a non-Transport controller.
- **Future (documented, not v0.1)**: broader baremetal proof-out (esp-hal + esp-radio + `parker-esp-rtos`; fake sysroot + pregen bindings via `xtask gen`); nrf54l15 leg; Windows; mbedtls crypto backend; **bttester/BTP bin for auto-PTS** (the "upstream E2E tests" hook); DIS/BAS wrappers; periodic-adv/EATT/ISO; fiber-based NPL backend if the executor-stall trade ever matters.

## Risks

| Risk | Mitigation |
|---|---|
| Pump-while-pending re-entrancy: RX processed inside a C call stack | Verified safe today (ack path lock-free, all else enqueue-only); add debug assertions (no nested pends, sem released by pump only); M2 sync burst + ASAN is the gate; re-audit `ble_hs_hci_rx_evt`/`rx_ack` on every submodule bump |
| Executor stalled during command acks (latency for co-located tasks); spin-poll burns CPU during that window (power) | Bounded by HCI cmd timeout, occurs only on command exchanges; document at `run()`; itest watchdog (sync < 5 s); power-sensitive targets opt into a `Parker` impl (std one ships built-in); fiber backend or internal-worker mode possible later without changing the core |
| NimBLE 4-byte-pointer assumptions (upstream forces `-m32` for its linux sim) | Keep NPL/port/bridge state in Rust; M2 ASAN gate; permanent i686 CI leg; grep-audit pointer→u32 casts in M1; patch via glue + upstream issue if hit |
| ESP-only includes grow on submodule bumps | Minimal error-driven stub set, each commented with its consumer; `nimble_port.c` replaced by a small Rust contract; bump procedure = diff syscfg.h + rerun nm audit |
| syscfg knob drift between fork versions | Universe always passed explicitly via `-D` → renames fail the C compile loudly; prebuilt fingerprint rejects stale artifacts |
| libstdc++ leakage (fork's linux NPL port is C++) | We compile zero files from `porting/npl/*`; nm check for `_ZSt/_ZN` |
| Non-discardable evt pool exhaustion in RX pump | Cmd path bounded (≤2); discardable path sheds; bounded yield-retry + warn; scan-flood itest |
| SDC has no generic raw cmd pipe → nrf-sdc dispatch table to maintain | Bounded set (only commands NimBLE emits per feature set); mirror sdk-nrf `hci_internal.c`'s proven mapping; table is mechanical and unit-testable (raw bytes → C struct is a cast, params are wire-format by construction); consider upstreaming a generic `raw_hci_cmd_put` dispatcher to nrf-sdc |
| crates.io package size | Aggressive `package.exclude` (controller, mesh, apps, per-chip transports, npl ports, docs); CI check |
