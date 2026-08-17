// See: <https://en.cppreference.com/w/c/header/stdio.html>
#pragma once

#ifdef __cplusplus
extern "C" {
#endif

#include <stdarg.h>
#include <stddef.h>

// mbedtls uses `FILE` for one of its function declarations.
// To ensure we're not actually using `FILE` at runtime we define it as an
// opaque struct.
typedef struct __forbidden_FILE FILE;

// Declarations only: esp-nimble formats exclusively in leaf/debug functions
// (`ble_uuid_to_str` -> sprintf, the `ble_gatts_show_local` dump -> printf),
// so the symbols are demanded at link time only if the application calls
// those. Nothing in the host's core paths formats (`console_printf` is a
// no-op in this build), and nimble-rs itself never calls them.
//
// Providing the symbols when needed: `tinyrlibc` has working
// snprintf/vsnprintf; for the full printf family, vendor `nanoprintf` (the
// way openthread does) rather than relying on tinyrlibc's incomplete
// *printf.

int printf(const char* format, ...);

int sprintf(char* s, const char* format, ...);

int snprintf(char* s, size_t n, const char* format, ...);

int vsnprintf(char* s, size_t n, const char* format, va_list arg);

#ifdef __cplusplus
} // extern "C"
#endif
