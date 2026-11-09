/*
 * Force-included into every C file of the harness (see build.rs).
 *
 * GCC 14 and newer reject calls to undeclared functions, even with `-w`.
 * This header declares the functions the C code calls without a prototype
 * in scope.
 */

#ifndef NIMBLE_RS_UPSTREAM_HARNESS_H
#define NIMBLE_RS_UPSTREAM_HARNESS_H

/* `ble_store_config.c` uses `qsort` without including it */
#include <stdlib.h>

/* Implemented in src/main.rs. build.rs maps `sysinit` and `os_started`
 * to the first two. */
void nimble_rs_upstream_sysinit(void);
int nimble_rs_upstream_os_started(void);
void os_time_advance(int ticks);

#endif
