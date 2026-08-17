/*
 * The C-side half of nimble-rs' host-log wiring: printf-style entry points
 * (the `MODLOG_*` macros from the glue `modlog/modlog.h`, plus Mynewt's
 * `console_printf` used by debug dumps) format into a stack buffer with the
 * vendored nanoprintf and forward the bytes to `nimble_rs_log`, implemented
 * by the `nimble-rs` crate on top of `log`/`defmt`.
 *
 * nanoprintf (`gen/vendored/nanoprintf`, tag v0.7.0 from
 * https://github.com/charlesnicholson/nanoprintf, dual-licensed 0BSD /
 * Unlicense) is a single-header, malloc-free printf implementation; the
 * namespaced `npf_*` API avoids any collision with a hosted libc.
 * Configuration mirrors `openthread-sys`: widths, precision, `hh`..`ll`/`z`
 * modifiers; no floating point or binary conversions (the NimBLE host
 * formats neither).
 */

#include <stdarg.h>

#define NANOPRINTF_IMPLEMENTATION
#define NANOPRINTF_USE_FIELD_WIDTH_FORMAT_SPECIFIERS 1
#define NANOPRINTF_USE_PRECISION_FORMAT_SPECIFIERS 1
#define NANOPRINTF_USE_LARGE_FORMAT_SPECIFIERS 1
#define NANOPRINTF_USE_SMALL_FORMAT_SPECIFIERS 1
#define NANOPRINTF_USE_FLOAT_FORMAT_SPECIFIERS 0
#define NANOPRINTF_USE_BINARY_FORMAT_SPECIFIERS 0
#define NANOPRINTF_USE_WRITEBACK_FORMAT_SPECIFIERS 0

#include "nanoprintf.h"

#include "syscfg/syscfg.h"

#include "log_common/log_common.h"

/* Implemented in Rust (`nimble-rs`); receives raw (not NUL-terminated)
 * fragments - the Rust side reassembles the host's multi-call log lines. */
extern void nimble_rs_log(int level, const char *msg, unsigned int len);

/* One fragment; NimBLE composes longer lines from several calls. */
#define LOG_BUF_SIZE 128

static int
log_vprintf(int level, const char *fmt, va_list args)
{
    char buf[LOG_BUF_SIZE];
    int n;

    n = npf_vsnprintf(buf, sizeof(buf), fmt, args);
    if (n > 0) {
        nimble_rs_log(level, buf, (unsigned int)n < sizeof(buf) - 1 ? (unsigned int)n : sizeof(buf) - 1);
    }

    return n;
}

int
nimble_rs_log_printf(int level, const char *fmt, ...)
{
    va_list args;
    int n;

    va_start(args, fmt);
    n = log_vprintf(level, fmt, args);
    va_end(args);

    return n;
}

int
console_printf(const char *fmt, ...)
{
    va_list args;
    int n;

    va_start(args, fmt);
    n = log_vprintf(LOG_LEVEL_INFO, fmt, args);
    va_end(args);

    return n;
}
