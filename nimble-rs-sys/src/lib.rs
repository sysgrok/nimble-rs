//! Raw FFI bindings to the [esp-nimble](https://github.com/espressif/esp-nimble)
//! BLE host stack (Espressif's fork of Apache NimBLE).
//!
//! The C stack is compiled from source (git submodule) by `build.rs`; its
//! Mynewt *syscfg* configuration (`MYNEWT_VAL_*`) is driven by this crate's
//! Cargo features - see `gen/features.rs` for the mapping.
//!
//! # Symbols this crate expects from its consumer
//!
//! The compiled C code is deliberately OS- and transport-agnostic. The
//! following symbol families are *declared* here but must be *defined* by the
//! consuming crate (normally `nimble-rs`):
//!
//! - `ble_npl_*` - the NimBLE Porting Layer (OS abstraction: event queues,
//!   mutexes, semaphores, callouts, time, critical sections). The object
//!   layouts are fixed by `gen/glue/include/nimble/nimble_npl_os.h`.
//! - `nimble_port_init`/`nimble_port_deinit`/`nimble_port_run`/
//!   `nimble_port_stop`/`nimble_port_get_dflt_eventq` - the port entry points
//!   (the C `nimble_port.c` is not compiled; see `gen/builder.rs`).
//! - `ble_transport_ll_init`/`ble_transport_ll_deinit`/
//!   `ble_transport_to_ll_cmd_impl`/`ble_transport_to_ll_acl_impl`/
//!   `ble_transport_to_ll_iso_impl` - the HCI bridge towards the controller.
//! - `nimble_platform_mem_*` - heap hooks; unreachable with this crate's fixed
//!   `BLE_STATIC_TO_DYNAMIC=0` configuration, but they must link.
//! - The usual libc string/memory functions (`memcpy`, `memset`, `strlen`,
//!   ...) on `no_std` targets without a hosted libc.

#![no_std]
#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(unknown_lints)]
#![allow(rustdoc::all)]
#![allow(clippy::all)]

include!(env!("NIMBLE_RS_SYS_BINDINGS_FILE"));
