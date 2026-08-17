// See: <https://en.cppreference.com/w/c/header/assert.html>
#pragma once

#ifdef __cplusplus
extern "C" {
#endif

#ifdef NDEBUG
# define assert(...) ((void)0)
#else
// Delegates to a hook implemented by the `nimble-rs` crate (a Rust panic
// carrying the file/line).
_Noreturn void nimble_rs_assert_fail(const char *file, int line);
# define assert(...) ((__VA_ARGS__) ? ((void)0) : nimble_rs_assert_fail(__FILE__, __LINE__))
#endif

#ifdef __cplusplus
} // extern "C"
#endif
