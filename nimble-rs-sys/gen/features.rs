//! Mapping of Cargo features to NimBLE (Mynewt *syscfg*) configuration values.
//!
//! # The doctrine (same as `openthread-sys`)
//!
//! NimBLE features must be selected at *compile time*: an enabled-but-unused C
//! feature cannot be recovered by `--gc-sections` (it leaves behind vtable-like
//! dispatch, statically-sized pools, and extra branches in shared code paths).
//! Therefore:
//!
//! - [`VAL_UNIVERSE`] lists every `MYNEWT_VAL_*` knob this crate controls, with
//!   its *reset* value. Every knob in the universe is **always** passed
//!   explicitly as a `-D` flag - never left to the `#ifndef` default in
//!   esp-nimble's pre-generated `syscfg.h`. This both documents the effective
//!   configuration and turns a knob rename in a future esp-nimble bump into a
//!   loud compile failure instead of a silent behavior change.
//! - Each Cargo feature re-enables its knobs on top of the reset state.
//! - Numeric knobs (`l2cap-coc-<N>`, `max-connections-<N>`, `msys-count-<N>`)
//!   are additive feature families where the largest enabled value wins, so
//!   feature unification across a dependency graph resolves to the maximum.
//!
//! The complete, sorted list of settings doubles as the fingerprint for the
//! pre-generated bindings/libraries (see [`prebuilt_validity`]).

use std::collections::BTreeMap;
use std::env;

/// Every `MYNEWT_VAL_*` knob this crate controls, with its reset value.
///
/// Notes on individual entries:
/// - `BLE_GATTS`/`BLE_GATTC` have *no* default in esp-nimble's `syscfg.h` (they
///   are normally supplied by ESP-IDF's `esp_nimble_cfg.h`); leaving them
///   undefined would silently compile the GATT code out. They are driven by
///   the `peripheral` (server) and `central` (client) role features - see
///   [`FEATURE_VALS`].
/// - `BLE_STATIC_TO_DYNAMIC=0` keeps the C host fully statically allocated
///   (the Espressif fork's static->heap conversion is guarded on it).
/// - The `BLE_TRANSPORT_HS__*`/`BLE_TRANSPORT_LL__*` syscfg *choice* variants
///   must all be pinned: the pre-generated `syscfg.h` defaults `LL__socket=1`
///   (it was generated for the Linux sample), and two simultaneously-enabled
///   variants would compile two dispatch branches in `transport.c`.
/// - `BLE_MESH` defaults to 1 in `syscfg.h`; we never compile the mesh sources.
pub const VAL_UNIVERSE: &[(&str, &str)] = &[
    // Roles
    ("BLE_ROLE_BROADCASTER", "0"),
    ("BLE_ROLE_CENTRAL", "0"),
    ("BLE_ROLE_OBSERVER", "0"),
    ("BLE_ROLE_PERIPHERAL", "0"),
    // GATT (enabled by the `peripheral`/`central` features)
    ("BLE_GATTS", "0"),
    ("BLE_GATTC", "0"),
    ("BLE_GATT_CACHING", "0"),
    // Client-Supported-Features characteristic size; an IDF-side knob with no
    // default in the fork's syscfg.h (`esp_nimble_cfg.h` defaults it to 1)
    ("BLE_GATT_CSFC_SIZE", "1"),
    ("BLE_STORE_MAX_CSFCS", "3"),
    // Advertising
    ("BLE_EXT_ADV", "0"),
    ("BLE_PERIODIC_ADV", "0"),
    // Security Manager
    ("BLE_SM_LEGACY", "0"),
    ("BLE_SM_SC", "0"),
    ("BLE_SM_SC_ONLY", "0"),
    // IDF-side knobs with no syscfg.h default whose *definitions* are guarded
    // while their call sites are not - leaving them 0 breaks the link
    // (`ble_hs_pvcy_our_irk`, `ble_sm_incr_our_sign_counter`). IDF defaults
    // both to on (Kconfig `BT_NIMBLE_HS_PVCY`/`BT_NIMBLE_SM_SIGN_CNT`).
    ("BLE_HS_PVCY", "1"),
    ("BLE_SM_SIGN_CNT", "1"),
    // L2CAP CoC
    ("BLE_L2CAP_COC_MAX_NUM", "0"),
    // Sizing
    ("BLE_MAX_CONNECTIONS", "4"),
    ("MSYS_1_BLOCK_COUNT", "20"),
    // Host debug instrumentation; its !BLE_STATIC_TO_DYNAMIC branch in
    // ble_hs.c uses FreeRTOS `TaskHandle_t` directly, and syscfg.h defaults
    // it to 1 (the pre-generated config is a debug one)
    ("BLE_HS_DEBUG", "0"),
    // Fixed-off subsystems
    ("BLE_ISO", "0"),
    ("BLE_MESH", "0"),
    ("BLE_EATT_CHAN_NUM", "0"),
    ("BLE_HS_FLOW_CTRL", "0"),
    ("BLE_GATT_BLOB_TRANSFER", "0"),
    ("BLE_QUEUE_CONG_CHECK", "0"),
    ("BLE_CRYPTO_STACK_MBEDTLS", "0"),
    // Memory model: keep the host statically allocated (see module docs)
    ("BLE_STATIC_TO_DYNAMIC", "0"),
    ("MP_RUNTIME_ALLOC", "0"),
    // Transport topology: the host is here ("native"), the controller is
    // external, reached through the `ble_transport_ll_*` symbols implemented
    // in Rust by the `nimble-rs` crate ("custom").
    ("BLE_TRANSPORT_HS__cdc", "0"),
    ("BLE_TRANSPORT_HS__custom", "0"),
    ("BLE_TRANSPORT_HS__dialog_cmac", "0"),
    ("BLE_TRANSPORT_HS__native", "1"),
    ("BLE_TRANSPORT_HS__nrf5340", "0"),
    ("BLE_TRANSPORT_HS__uart", "0"),
    ("BLE_TRANSPORT_HS__usb", "0"),
    ("BLE_TRANSPORT_LL__apollo3", "0"),
    ("BLE_TRANSPORT_LL__custom", "1"),
    ("BLE_TRANSPORT_LL__dialog_cmac", "0"),
    ("BLE_TRANSPORT_LL__emspi", "0"),
    ("BLE_TRANSPORT_LL__native", "0"),
    ("BLE_TRANSPORT_LL__nrf5340", "0"),
    ("BLE_TRANSPORT_LL__socket", "0"),
];

/// Per-feature knob overrides, applied on top of [`VAL_UNIVERSE`].
///
/// The first element is the Cargo feature name as it appears in the
/// `CARGO_FEATURE_*` environment (uppercase, `-` replaced by `_`).
pub const FEATURE_VALS: &[(&str, &[(&str, &str)])] = &[
    // The GATT roles ride on the GAP roles: a connectable peripheral serves
    // its attributes (`BLE_GATTS`), a central consumes its peers' (`BLE_GATTC`).
    (
        "PERIPHERAL",
        &[("BLE_ROLE_PERIPHERAL", "1"), ("BLE_GATTS", "1")],
    ),
    ("BROADCASTER", &[("BLE_ROLE_BROADCASTER", "1")]),
    ("CENTRAL", &[("BLE_ROLE_CENTRAL", "1"), ("BLE_GATTC", "1")]),
    ("OBSERVER", &[("BLE_ROLE_OBSERVER", "1")]),
    ("EXT_ADV", &[("BLE_EXT_ADV", "1")]),
    // `sm-sc-only` first, so that `sm` wins when both are enabled (documented
    // in Cargo.toml): the later entry overwrites the earlier one.
    (
        "SM_SC_ONLY",
        &[
            ("BLE_SM_LEGACY", "0"),
            ("BLE_SM_SC", "1"),
            ("BLE_SM_SC_ONLY", "1"),
        ],
    ),
    (
        "SM",
        &[
            ("BLE_SM_LEGACY", "1"),
            ("BLE_SM_SC", "1"),
            ("BLE_SM_SC_ONLY", "0"),
        ],
    ),
];

/// The upstream `nimble/host/test` suite's configuration (its `syscfg.yml`),
/// as an overlay. One deviation: `CONFIG_FCB` is not set (the FCB-backed
/// config persistence is not compiled; the config store runs RAM-backed).
/// `BLE_HS_DEBUG=1` (needed by the SM suites' `ble_*_dbg_*` key-injection
/// hooks) works because [`UPSTREAM_TEST_EXTRA_DEFINES`] maps the fork's
/// FreeRTOS task-handle usage onto the NPL.
pub const UPSTREAM_TEST_VALS: &[(&str, &str)] = &[
    ("SELFTEST", "1"),
    ("BLE_HS_PHONY_HCI_ACKS", "1"),
    ("BLE_HS_REQUIRE_OS", "0"),
    ("BLE_MAX_CONNECTIONS", "8"),
    ("BLE_GATT_MAX_PROCS", "16"),
    ("BLE_SM", "1"),
    ("BLE_SM_LEGACY", "1"),
    ("BLE_SM_SC", "1"),
    ("MSYS_1_BLOCK_COUNT", "100"),
    ("BLE_L2CAP_COC_MAX_NUM", "2"),
    ("BLE_VERSION", "52"),
    ("BLE_L2CAP_ENHANCED_COC", "1"),
    ("BLE_GATTS", "1"),
    ("BLE_GATTC", "1"),
    ("BLE_HS_DEBUG", "1"),
];

/// Plain defines accompanying [`UPSTREAM_TEST_VALS`]: the fork's
/// `BLE_HS_DEBUG` lock-tracking uses FreeRTOS task handles directly; the NPL
/// task-identity API is a drop-in.
pub const UPSTREAM_TEST_EXTRA_DEFINES: &[(&str, &str)] = &[
    ("TaskHandle_t", "void *"),
    (
        "xTaskGetCurrentTaskHandle()",
        "ble_npl_get_current_task_id()",
    ),
];

/// Additive numeric feature families; the largest enabled value wins.
///
/// The first element is the `CARGO_FEATURE_` prefix of the family, the second
/// the `MYNEWT_VAL_*` knob it sets.
pub const NUMERIC_FAMILIES: &[(&str, &str)] = &[
    ("L2CAP_COC_", "BLE_L2CAP_COC_MAX_NUM"),
    ("MAX_CONNECTIONS_", "BLE_MAX_CONNECTIONS"),
    ("MSYS_COUNT_", "MSYS_1_BLOCK_COUNT"),
];

/// The feature set the committed pre-generated bindings and libraries are
/// produced with (must match the `prebuilt` bundle in `Cargo.toml`).
pub const PREBUILT_FEATURES: &[&str] = &["PERIPHERAL", "BROADCASTER", "CENTRAL", "OBSERVER", "SM"];

/// Extra plain (non-`MYNEWT_VAL`) defines the esp-nimble fork requires.
///
/// - `CONFIG_BT_NIMBLE_GAP_SERVICE`: `ble_svc_gap.c` wraps its whole body in
///   `#if MYNEWT_VAL(BLE_GATTS) && CONFIG_BT_NIMBLE_GAP_SERVICE` - without
///   this define the mandatory GAP service silently compiles to nothing.
/// - `CONFIG_BT_NIMBLE_ENABLED`: `os_msys_init.c` keys the msys mbuf pool
///   sizes on it (`MYNEWT_VAL(MSYS_*)` when set, `CONFIG_BT_LE_MSYS_*`
///   otherwise) - without it the msys pools are empty and the first ATT
///   response allocation fails with `BLE_HS_ENOMEM`.
pub const EXTRA_DEFINES: &[(&str, &str)] = &[
    ("CONFIG_BT_NIMBLE_GAP_SERVICE", "1"),
    ("CONFIG_BT_NIMBLE_ENABLED", "1"),
];

/// Computes the effective `MYNEWT_VAL_*` settings for an explicit feature set
/// (feature names in `CARGO_FEATURE_*` form).
pub fn val_settings_for(features: &[&str]) -> BTreeMap<&'static str, String> {
    let mut vals: BTreeMap<&'static str, String> = VAL_UNIVERSE
        .iter()
        .map(|(name, value)| (*name, value.to_string()))
        .collect();

    for (feature, overrides) in FEATURE_VALS {
        if features.contains(feature) {
            for (name, value) in *overrides {
                vals.insert(name, value.to_string());
            }
        }
    }

    if features.contains(&"UPSTREAM_TEST") {
        for (name, value) in UPSTREAM_TEST_VALS {
            vals.insert(name, value.to_string());
        }
    }

    for (prefix, val_name) in NUMERIC_FAMILIES {
        let max = features
            .iter()
            .filter_map(|feature| feature.strip_prefix(prefix))
            .filter_map(|suffix| suffix.parse::<u32>().ok())
            .max();
        if let Some(max) = max {
            let reset = vals
                .get(val_name)
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(0);
            vals.insert(val_name, max.max(reset).to_string());
        }
    }

    vals
}

/// The features currently active for this build, in `CARGO_FEATURE_*` form.
pub fn active_features() -> Vec<String> {
    let mut features = env::vars()
        .filter_map(|(key, _)| key.strip_prefix("CARGO_FEATURE_").map(str::to_string))
        .collect::<Vec<_>>();
    features.sort();
    features
}

/// Computes the effective `MYNEWT_VAL_*` settings for the currently-active
/// Cargo feature set.
pub fn active_val_settings() -> BTreeMap<&'static str, String> {
    let features = active_features();
    let features = features.iter().map(String::as_str).collect::<Vec<_>>();

    val_settings_for(&features)
}

/// Checks whether the currently-active feature set selects exactly the same
/// NimBLE configuration as [`PREBUILT_FEATURES`] (with which the committed
/// pre-generated bindings/libraries are produced).
///
/// On mismatch, returns a `+KNOB=v` / `-KNOB=v` / `~KNOB=v1->v2` delta string.
pub fn prebuilt_validity() -> Result<(), String> {
    let active = active_val_settings();
    let reference = val_settings_for(PREBUILT_FEATURES);

    let mut delta = Vec::new();

    for (name, reference_value) in &reference {
        match active.get(name) {
            Some(value) if value == reference_value => (),
            Some(value) => delta.push(format!("~{name}={reference_value}->{value}")),
            None => delta.push(format!("-{name}={reference_value}")),
        }
    }

    for (name, value) in &active {
        if !reference.contains_key(name) {
            delta.push(format!("+{name}={value}"));
        }
    }

    if delta.is_empty() {
        Ok(())
    } else {
        Err(delta.join(", "))
    }
}
