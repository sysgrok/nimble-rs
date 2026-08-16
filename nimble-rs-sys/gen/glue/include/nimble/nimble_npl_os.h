/*
 * The nimble-rs NPL (NimBLE Porting Layer) ABI.
 *
 * `nimble/nimble_npl.h` declares the `ble_npl_*` API and includes this header
 * for the concrete object layouts. In nimble-rs, every NPL object is an opaque,
 * inline, 8-aligned array of pointer-sized words. The real implementations live
 * in Rust (the `npl` module of the `nimble-rs` crate), whose `#[repr(C)]` types
 * statically assert that they fit these shells.
 *
 * Objects are embedded by value inside C structs and statics and are
 * constructed in place by `ble_npl_*_init` - no allocation is involved, and the
 * layouts scale with the target word size, keeping this header target-portable.
 */
#ifndef NIMBLE_RS_GLUE_NIMBLE_NPL_OS_H
#define NIMBLE_RS_GLUE_NIMBLE_NPL_OS_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define BLE_NPL_OS_ALIGNMENT 8
#define BLE_NPL_TIME_FOREVER UINT32_MAX

/* Milliseconds (1000 ticks per second; the tick conversion functions in the
 * Rust NPL implementation are identities). */
typedef uint32_t ble_npl_time_t;
typedef int32_t ble_npl_stime_t;

struct ble_npl_event {
    _Alignas(8) void *v[6];
};

/*
 * The first member is named (and null-checked!) by the fork's host code:
 * `ble_hs.c` (`ble_hs_enqueue_hci_event`) tests `ble_hs_evq->eventq` directly
 * to see whether the queue exists. The Rust implementation therefore
 * guarantees: `eventq` is non-NULL exactly while the queue is initialized.
 */
struct ble_npl_eventq {
    _Alignas(8) void *eventq;
    void *v[7];
};

struct ble_npl_callout {
    _Alignas(8) void *v[16];
};

struct ble_npl_mutex {
    _Alignas(8) void *v[6];
};

struct ble_npl_sem {
    _Alignas(8) void *v[8];
};

#ifdef __cplusplus
}
#endif

#endif /* NIMBLE_RS_GLUE_NIMBLE_NPL_OS_H */
