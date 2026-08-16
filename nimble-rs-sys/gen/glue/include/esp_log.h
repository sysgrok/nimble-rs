/*
 * Minimal stand-in for ESP-IDF's `esp_log.h`.
 *
 * Required because `porting/nimble/src/os_mbuf.c` includes it unconditionally
 * (verified on esp-nimble 039d2d62); no macro from it is actually used by the
 * sources this crate compiles, so the log macros are no-ops.
 */
#ifndef NIMBLE_RS_GLUE_ESP_LOG_H
#define NIMBLE_RS_GLUE_ESP_LOG_H

#define ESP_LOGE(tag, ...)
#define ESP_LOGW(tag, ...)
#define ESP_LOGI(tag, ...)
#define ESP_LOGD(tag, ...)
#define ESP_LOGV(tag, ...)

#endif /* NIMBLE_RS_GLUE_ESP_LOG_H */
