/*
 * Minimal stand-in for the Mynewt `console/console.h` (ESP-IDF supplies its
 * own printf-backed version in `components/bt/host/nimble/port`).
 *
 * Required because `nimble/host/src/ble_gatts_lcl.c` includes it
 * unconditionally for the `ble_gatts_show_local()` debug dump. Console output
 * is a no-op in nimble-rs (which must also build on freestanding targets
 * without printf); diagnostic visibility comes from the Rust side instead.
 */
#ifndef NIMBLE_RS_GLUE_CONSOLE_CONSOLE_H
#define NIMBLE_RS_GLUE_CONSOLE_CONSOLE_H

#define console_printf(...)

#endif /* NIMBLE_RS_GLUE_CONSOLE_CONSOLE_H */
