/*
 * Stand-in for the Mynewt `modlog/modlog.h` (shadowing the copy in
 * `porting/nimble/include`, which routes everything to `printf` / ESP-IDF
 * logging): every enabled `MODLOG_*` invocation funnels through
 * `nimble_rs_log_printf` (see `gen/glue/src/log.c`), which formats with the
 * vendored nanoprintf and hands the line to the `nimble-rs` crate's logging
 * (`log`/`defmt`).
 *
 * Levels use the Mynewt numbering (`log_common.h`: 0=DEBUG .. 4=CRITICAL),
 * compile-time-gated on `MYNEWT_VAL(LOG_LEVEL)` exactly like the original -
 * with the default `LOG_LEVEL=255` every call compiles out.
 */
#ifndef NIMBLE_RS_GLUE_MODLOG_H
#define NIMBLE_RS_GLUE_MODLOG_H

#include "syscfg/syscfg.h"

#include "log_common/log_common.h"

#ifdef __cplusplus
extern "C" {
#endif

int nimble_rs_log_printf(int level, const char *fmt, ...);

#ifndef IGNORE
#define IGNORE(...) (void)(0)
#endif

#define MODLOG_MODULE_DFLT 255

#if MYNEWT_VAL(LOG_LEVEL) <= LOG_LEVEL_DEBUG
#define MODLOG_DEBUG(ml_mod_, ml_msg_, ...) \
    nimble_rs_log_printf(LOG_LEVEL_DEBUG, (ml_msg_), ##__VA_ARGS__)
#else
#define MODLOG_DEBUG(ml_mod_, ...) IGNORE(__VA_ARGS__)
#endif

#if MYNEWT_VAL(LOG_LEVEL) <= LOG_LEVEL_INFO
#define MODLOG_INFO(ml_mod_, ml_msg_, ...) \
    nimble_rs_log_printf(LOG_LEVEL_INFO, (ml_msg_), ##__VA_ARGS__)
#else
#define MODLOG_INFO(ml_mod_, ...) IGNORE(__VA_ARGS__)
#endif

#if MYNEWT_VAL(LOG_LEVEL) <= LOG_LEVEL_WARN
#define MODLOG_WARN(ml_mod_, ml_msg_, ...) \
    nimble_rs_log_printf(LOG_LEVEL_WARN, (ml_msg_), ##__VA_ARGS__)
#else
#define MODLOG_WARN(ml_mod_, ...) IGNORE(__VA_ARGS__)
#endif

#if MYNEWT_VAL(LOG_LEVEL) <= LOG_LEVEL_ERROR
#define MODLOG_ERROR(ml_mod_, ml_msg_, ...) \
    nimble_rs_log_printf(LOG_LEVEL_ERROR, (ml_msg_), ##__VA_ARGS__)
#else
#define MODLOG_ERROR(ml_mod_, ...) IGNORE(__VA_ARGS__)
#endif

#if MYNEWT_VAL(LOG_LEVEL) <= LOG_LEVEL_CRITICAL
#define MODLOG_CRITICAL(ml_mod_, ml_msg_, ...) \
    nimble_rs_log_printf(LOG_LEVEL_CRITICAL, (ml_msg_), ##__VA_ARGS__)
#else
#define MODLOG_CRITICAL(ml_mod_, ...) IGNORE(__VA_ARGS__)
#endif

#define MODLOG(ml_lvl_, ml_mod_, ...) MODLOG_##ml_lvl_((ml_mod_), __VA_ARGS__)

#define MODLOG_DFLT(ml_lvl_, ...) MODLOG(ml_lvl_, LOG_MODULE_DEFAULT, __VA_ARGS__)

#ifdef __cplusplus
}
#endif

#endif /* NIMBLE_RS_GLUE_MODLOG_H */
