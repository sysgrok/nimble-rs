/*
 * Minimal stand-in for ESP-IDF's `bt_osi_mem.h` (`components/bt/common/include`).
 *
 * Required because `nimble/host/src/ble_hs_iso.c` includes it unconditionally
 * (verified on esp-nimble 039d2d62). With this crate's fixed
 * `MYNEWT_VAL_BLE_ISO=0` configuration nothing in that file is compiled, so
 * only the prototypes are needed. Should an allocating configuration ever be
 * enabled, these are implemented in Rust by `nimble-rs` alongside
 * `nimble_platform_mem_*`.
 */
#ifndef NIMBLE_RS_GLUE_BT_OSI_MEM_H
#define NIMBLE_RS_GLUE_BT_OSI_MEM_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

void *bt_osi_mem_malloc(size_t size);
void *bt_osi_mem_calloc(size_t n, size_t size);
void bt_osi_mem_free(void *ptr);

#ifdef __cplusplus
}
#endif

#endif /* NIMBLE_RS_GLUE_BT_OSI_MEM_H */
