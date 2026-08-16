//! Compiles the upstream NimBLE host test suite (`nimble/host/test/src`) and
//! the vendored mynewt testutil framework, against the exact configuration
//! and include paths of the `nimble-rs-sys` build (via DEP_NIMBLE_*).

use std::env;
use std::path::PathBuf;

fn main() {
    // esp-nimble reads the remote version before delivering the connect event
    // (master: after the features read; slave: right at conn-complete) -
    // upstream NimBLE does neither, and the upstream-authored tests' phony-ack
    // sequences would desync on the extra command. `ble_gap_rd_rem_ver_tx`
    // itself can't be wrapped (its callers live in the same translation unit,
    // so the calls bind section-relative), but its `ble_hs_hci_cmd_tx` call
    // crosses into ble_hs_hci.c - wrap that and swallow just this opcode
    // (`__wrap_ble_hs_hci_cmd_tx` in src/main.rs).
    println!("cargo::rustc-link-arg=-Wl,--wrap=ble_hs_hci_cmd_tx");
    // The fork's startup command sequence also differs from the canned
    // phony-ack table (extra LE Rand, no LE Set Adv Enable); the wrapper
    // installs a corrected table (`__wrap_ble_hs_test_util_hci_ack_set_startup`).
    println!("cargo::rustc-link-arg=-Wl,--wrap=ble_hs_test_util_hci_ack_set_startup");
    println!("cargo::rustc-link-arg=-Wl,--wrap=ble_hs_test_util_hci_startup_seq_cnt");
    // The privacy suite stages ack sequences for upstream's preempt/apply-IRK
    // HCI flows, which the fork reworked wholesale; skipped (the privacy
    // module itself is still exercised via startup and the RPA own-address
    // types in the GAP suites).
    println!("cargo::rustc-link-arg=-Wl,--wrap=ble_hs_pvcy_test_suite_irk");
    // The fork emits a duplicate BLE_GAP_EVENT_LINK_ESTAB after every
    // BLE_GAP_EVENT_CONNECT; the upstream test callbacks assert on unknown
    // event types. The wrappers interpose a filtering trampoline between the
    // host and the test-supplied GAP event callbacks.
    println!("cargo::rustc-link-arg=-Wl,--wrap=ble_gap_connect");
    println!("cargo::rustc-link-arg=-Wl,--wrap=ble_gap_adv_start");
    // The fork's new `disable_observer_mode` discovery param defaults to
    // observer mode (no Flags-AD filtering); upstream filters by default and
    // its tests rely on that. The wrapper re-enables filtering.
    println!("cargo::rustc-link-arg=-Wl,--wrap=ble_gap_disc");
    // With no host task, host-loop work (e.g. the deferred notification TX
    // event `ble_hs_notifications_sched` enqueues) sits on the event queue
    // until someone drains it; do that whenever a test starts inspecting the
    // captured TX traffic.
    println!("cargo::rustc-link-arg=-Wl,--wrap=ble_hs_test_util_prev_tx_dequeue");
    // The COC-multi l2cap case leaves a torn-down channel's rx_buf dangling
    // in a still-registered connection (fork bug: freed, not NULLed); the SM
    // suite's crypto vector cases run without re-init and their mbuf-leak
    // accounting walks that garbage. Re-init the stack before the suite.
    println!("cargo::rustc-link-arg=-Wl,--wrap=ble_sm_gen_test_suite");
    // The fork stamps its new `bond_count` field into stored security
    // entries; upstream's store tests memcmp whole structs. Zero the field
    // on read so values round-trip upstream-shaped.
    println!("cargo::rustc-link-arg=-Wl,--wrap=ble_store_read_our_sec");
    println!("cargo::rustc-link-arg=-Wl,--wrap=ble_store_read_peer_sec");
    println!("cargo::rustc-link-arg=-Wl,--wrap=ble_hs_test_util_prev_tx_dequeue_pullup");

    let sys_root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("../../nimble-rs-sys")
        .canonicalize()
        .unwrap();
    let test_src = sys_root.join("esp-nimble/nimble/host/test/src");
    let testutil = sys_root.join("gen/vendored/testutil");

    println!("cargo::rerun-if-changed={}", test_src.display());
    println!("cargo::rerun-if-changed={}", testutil.display());

    let mut build = cc::Build::new();

    for dir in env::var("DEP_NIMBLE_INCLUDE").unwrap().split(';') {
        build.include(dir);
    }
    for def in env::var("DEP_NIMBLE_DEFINES").unwrap().split(';') {
        let (name, value) = def.split_once('=').unwrap();
        build.define(name, value);
    }

    build.include(testutil.join("include"));
    build.include(&test_src); // the *_test_util.h headers
                              // The tests reach into the host's private headers (ble_hs_priv.h etc.)
    build.include(sys_root.join("esp-nimble/nimble/host/src"));
    // ...and use the config-based store (RAM-backed here: no FCB/NFFS/NVS),
    // which the sys build does not compile
    build.include(sys_root.join("esp-nimble/nimble/host/store/config/include"));
    build.file(sys_root.join("esp-nimble/nimble/host/store/config/src/ble_store_config.c"));

    // The suite aggregator (`ble_hs_test.c`) defines the C `main`; rename it
    // so the Rust main can invoke it.
    build.define("main", "nimble_upstream_test_main");
    // Mynewt's `sysinit()` (full package re-init between test cases) is
    // provided by src/main.rs; route the C call sites to it.
    build.define("sysinit", "nimble_rs_upstream_sysinit");
    // Mynewt kernel tick rate; the nimble-rs NPL uses millisecond ticks
    build.define("OS_TICKS_PER_SEC", "1000");
    // Map the mynewt kernel semaphore (used directly by ble_hs_stop_test.c)
    // onto the NPL one - identical shapes
    build.define("os_sem", "ble_npl_sem");
    build.define("os_sem_init", "ble_npl_sem_init");
    build.define("os_sem_pend", "ble_npl_sem_pend");
    build.define("os_sem_release", "ble_npl_sem_release");
    build.define("OS_TIMEOUT_NEVER", "BLE_NPL_TIME_FOREVER");
    // ...and the mynewt kernel time API
    build.define("os_time_ms_to_ticks", "ble_npl_time_ms_to_ticks");
    build.define("os_time_ms_to_ticks32", "ble_npl_time_ms_to_ticks32");
    // Not `ble_npl_os_started` (always true in nimble-rs): the test util
    // processes injected HCI events inline only while `!os_started()`, the
    // way Mynewt's selftest environment does - there is no host task/loop
    // draining the event queue here.
    build.define("os_started", "nimble_rs_upstream_os_started");
    build.define("min(a,b)", "(((a) < (b)) ? (a) : (b))");
    build.define("max(a,b)", "(((a) > (b)) ? (a) : (b))");

    for entry in std::fs::read_dir(&test_src).unwrap() {
        let path = entry.unwrap().path();
        // `ble_os_test.c` exercises the host under a real Mynewt scheduler
        // (os_task/os_start) - the very layer nimble-rs replaces; its suite
        // is stubbed as skipped in src/main.rs.
        if path.extension().is_some_and(|e| e == "c")
            && path.file_name().is_some_and(|f| f != "ble_os_test.c")
        {
            build.file(path);
        }
    }
    for f in ["case.c", "suite.c", "testutil.c"] {
        build.file(testutil.join("src").join(f));
    }

    build
        .flag("-include")
        .flag("esp_err.h")
        .warnings(false)
        .compile("nimble-upstream-tests");
}
