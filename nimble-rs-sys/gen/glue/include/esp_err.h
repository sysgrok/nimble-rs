/*
 * Minimal stand-in for ESP-IDF's `esp_err.h`.
 *
 * Required because the esp-nimble fork references it outside ESP-IDF-only code
 * (verified on esp-nimble master 274b98003):
 * - `porting/nimble/include/nimble/nimble_port.h` includes it unconditionally;
 * - `nimble/transport/src/transport.c` declares `ble_buf_alloc()`/`ble_buf_free()`
 *   with an `esp_err_t` return type.
 */
#ifndef NIMBLE_RS_GLUE_ESP_ERR_H
#define NIMBLE_RS_GLUE_ESP_ERR_H

typedef int esp_err_t;

#define ESP_OK 0
#define ESP_FAIL -1

#define ESP_ERR_NO_MEM 0x101
#define ESP_ERR_INVALID_ARG 0x102
#define ESP_ERR_INVALID_STATE 0x103

#endif /* NIMBLE_RS_GLUE_ESP_ERR_H */
