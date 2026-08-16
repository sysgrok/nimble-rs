/*
 * Minimal stand-in for ESP-IDF's `esp_nimble_mem.h`
 * (`components/bt/host/nimble/port/include`).
 *
 * Required because the esp-nimble fork includes it unconditionally from
 * `nimble/host/src/ble_gatts.c`, `nimble/host/src/ble_l2cap_sig.c` and
 * `nimble/transport/src/transport.c` (verified on master 274b98003).
 *
 * With this crate's fixed `MYNEWT_VAL_BLE_STATIC_TO_DYNAMIC=0` configuration
 * no call sites of these functions are compiled (the host uses upstream-style
 * static arrays), but the prototypes must exist. The implementations - for the
 * feature-gated corners that do allocate - are provided in Rust by `nimble-rs`.
 */
#ifndef NIMBLE_RS_GLUE_ESP_NIMBLE_MEM_H
#define NIMBLE_RS_GLUE_ESP_NIMBLE_MEM_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

void *nimble_platform_mem_malloc(size_t size);
void *nimble_platform_mem_calloc(size_t n, size_t size);
void *nimble_platform_mem_realloc(void *ptr, size_t size);
void nimble_platform_mem_free(void *ptr);

#ifdef __cplusplus
}
#endif

#endif /* NIMBLE_RS_GLUE_ESP_NIMBLE_MEM_H */
