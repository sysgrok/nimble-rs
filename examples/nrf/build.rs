//! This build script copies the `memory-<chip>.x` file selected by the active
//! chip feature from the crate root into a directory where the linker can
//! always find it at build time, as plain `memory.x`.
//! For many projects this is optional, as the linker always searches the
//! project root directory -- wherever `Cargo.toml` is. However, if you
//! are using a workspace or have a more complicated build setup, this
//! build script becomes required. Additionally, by requesting that
//! Cargo re-run the build script whenever the memory maps change,
//! updating one ensures a rebuild of the application with the
//! new memory settings.

use std::env;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

/// The memory map of the chip selected by the active chip feature.
///
/// Exactly one of them must be enabled: the chip features are mutually
/// exclusive all the way down (embassy-nrf, nrf-mpsl and nrf-sdc each pick a
/// different register block, peripheral set and controller library), and
/// `nrf52840` is a *default* feature, so selecting another chip means
/// `--no-default-features --features <chip>`.
///
/// The chip must be paired with its target (`--target`); nrf-mpsl-sys and
/// nrf-sdc-sys reject a mismatch themselves ("Unsupported target"), as the
/// controller they link is a per-core precompiled library.
fn linker_data() -> &'static [u8] {
    let maps: &[(&str, &[u8])] = &[
        #[cfg(feature = "nrf52840")]
        ("nrf52840", include_bytes!("memory-nrf52840.x")),
        #[cfg(feature = "nrf54l05")]
        ("nrf54l05", include_bytes!("memory-nrf54l05.x")),
        #[cfg(feature = "nrf54l10")]
        ("nrf54l10", include_bytes!("memory-nrf54l10.x")),
        #[cfg(feature = "nrf54l15")]
        ("nrf54l15", include_bytes!("memory-nrf54l15.x")),
        #[cfg(feature = "nrf54lm20")]
        ("nrf54lm20", include_bytes!("memory-nrf54lm20.x")),
    ];

    match maps {
        [(_, map)] => map,
        [] => panic!(
            "No chip feature enabled. Select exactly one of: \
             nrf52840, nrf54l05, nrf54l10, nrf54l15, nrf54lm20"
        ),
        many => panic!(
            "The chip features are mutually exclusive, but {} are enabled: {}. \
             Did you forget `--no-default-features` (nrf52840 is a default feature)?",
            many.len(),
            many.iter()
                .map(|(chip, _)| *chip)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn main() {
    // Put `memory.x` in our output directory and ensure it's
    // on the linker search path.
    let out = &PathBuf::from(env::var_os("OUT_DIR").unwrap());
    File::create(out.join("memory.x"))
        .unwrap()
        .write_all(linker_data())
        .unwrap();
    println!("cargo:rustc-link-search={}", out.display());

    // By default, Cargo will re-run a build script whenever
    // any file in the project changes. By specifying the memory maps
    // here, we ensure the build script is only re-run when one of
    // them is changed.
    for chip in ["nrf52840", "nrf54l05", "nrf54l10", "nrf54l15", "nrf54lm20"] {
        println!("cargo:rerun-if-changed=memory-{chip}.x");
    }

    println!("cargo:rustc-link-arg-bins=--nmagic");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    println!("cargo:rustc-link-arg-bins=-Tdefmt.x");
}
