//! The Rust half of the C-host log wiring: receives the fragments the
//! nanoprintf-backed shim (`nimble-rs-sys` `gen/glue/src/log.c`) formats,
//! reassembles the host's multi-call log lines, and emits them through this
//! crate's `log`/`defmt` backend.
//!
//! Compiled out entirely (dead code, no statics kept) unless the C side
//! actually calls in - which requires one of the `log-level-*` features.

use core::cell::RefCell;
use core::ffi::{c_char, c_int, c_uint};

use critical_section::Mutex;

/// One reassembled log line; longer lines are split.
const LINE_MAX: usize = 256;

struct LineBuf {
    buf: [u8; LINE_MAX],
    len: usize,
    level: c_int,
}

static LINE: Mutex<RefCell<LineBuf>> = Mutex::new(RefCell::new(LineBuf {
    buf: [0; LINE_MAX],
    len: 0,
    level: 0,
}));

/// Called by the C shim with one formatted fragment (not NUL-terminated,
/// possibly a partial line - the host composes lines from several calls).
#[no_mangle]
extern "C" fn nimble_rs_log(level: c_int, msg: *const c_char, len: c_uint) {
    if msg.is_null() {
        return;
    }

    let bytes = unsafe { core::slice::from_raw_parts(msg.cast::<u8>(), len as usize) };

    for &byte in bytes {
        // Assembled inside the critical section; emitted outside of it (the
        // sink may do I/O)
        let complete = critical_section::with(|cs| {
            let mut line = LINE.borrow_ref_mut(cs);
            line.level = line.level.max(level);

            if byte == b'\n' || line.len == LINE_MAX {
                let out = (line.level, line.buf, line.len);
                line.len = 0;
                line.level = 0;

                if byte != b'\n' {
                    let at = line.len;
                    line.buf[at] = byte;
                    line.len += 1;
                }

                Some(out)
            } else {
                let at = line.len;
                line.buf[at] = byte;
                line.len += 1;
                None
            }
        });

        if let Some((level, buf, len)) = complete {
            emit(level, &buf[..len]);
        }
    }
}

fn emit(level: c_int, line: &[u8]) {
    if line.is_empty() {
        return;
    }

    let line = core::str::from_utf8(line).unwrap_or("<non-utf8 log line>");

    // Mynewt levels (`log_common.h`): 0=DEBUG 1=INFO 2=WARN 3=ERROR 4=CRITICAL
    #[cfg(feature = "log")]
    {
        let level = match level {
            0 => ::log::Level::Debug,
            1 => ::log::Level::Info,
            2 => ::log::Level::Warn,
            _ => ::log::Level::Error,
        };
        ::log::log!(target: "nimble", level, "{line}");
    }

    #[cfg(feature = "defmt")]
    match level {
        0 => ::defmt::debug!("nimble: {=str}", line),
        1 => ::defmt::info!("nimble: {=str}", line),
        2 => ::defmt::warn!("nimble: {=str}", line),
        _ => ::defmt::error!("nimble: {=str}", line),
    }

    #[cfg(not(any(feature = "log", feature = "defmt")))]
    let _ = (level, line);
}
