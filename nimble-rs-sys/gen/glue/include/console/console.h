/*
 * Minimal stand-in for the Mynewt `console/console.h` (ESP-IDF supplies its
 * own printf-backed version in `components/bt/host/nimble/port`).
 *
 * Required because `nimble/host/src/ble_gatts_lcl.c` includes it
 * unconditionally for the `ble_gatts_show_local()` debug dump. Output routes
 * through the same nanoprintf-backed shim as the `MODLOG_*` macros (see
 * `gen/glue/src/log.c`) into the `nimble-rs` crate's logging.
 */
#ifndef NIMBLE_RS_GLUE_CONSOLE_CONSOLE_H
#define NIMBLE_RS_GLUE_CONSOLE_CONSOLE_H

#ifdef __cplusplus
extern "C" {
#endif

int console_printf(const char *fmt, ...);

#ifdef __cplusplus
}
#endif

#endif /* NIMBLE_RS_GLUE_CONSOLE_CONSOLE_H */
