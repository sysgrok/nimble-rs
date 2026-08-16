use std::{env, path::PathBuf};

use anyhow::Result;

#[path = "gen/builder.rs"]
mod builder;

use builder::NimbleBuilder;

fn main() -> Result<()> {
    let crate_root_path = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());

    NimbleBuilder::track(&crate_root_path.join("gen"));
    NimbleBuilder::track(&crate_root_path.join("esp-nimble"));

    let host = env::var("HOST").unwrap();
    let target = env::var("TARGET").unwrap();

    // If `force-generate-bindings` is enabled, re-build on the fly even if
    // pre-generated bindings exist for the target triple.
    let pregen_bindings = env::var("CARGO_FEATURE_FORCE_GENERATE_BINDINGS").is_err();

    let pregen_bindings_rs_file = crate_root_path
        .join("src")
        .join("include")
        .join(format!("{target}.rs"));
    let pregen_libs_dir = crate_root_path.join("libs").join(&target);

    // Desync guard: when the `prebuilt` profile itself is active (i.e. `xtask`
    // generating the committed artifacts), the active feature set MUST equal
    // the prebuilt reference, or the `prebuilt` bundle in `Cargo.toml` and
    // `features::PREBUILT_FEATURES` have drifted apart.
    if env::var_os("CARGO_FEATURE_PREBUILT").is_some() {
        if let Err(delta) = builder::features::prebuilt_validity() {
            panic!(
                "BUG: `prebuilt` profile active but the selected knobs do not match \
                 `features::PREBUILT_FEATURES`. The `prebuilt` bundle in Cargo.toml \
                 and PREBUILT_FEATURES have drifted. Delta: {delta}"
            );
        }
    }

    // The committed prebuilt libraries and bindings are produced with the
    // `prebuilt` feature profile. They are valid only if the active features
    // select exactly the same NimBLE knobs; otherwise we must rebuild on the
    // fly (`--gc-sections` cannot recover the difference).
    let prebuilt_validity = builder::features::prebuilt_validity();

    if pregen_bindings && pregen_bindings_rs_file.exists() && prebuilt_validity.is_ok() {
        // Use the pre-generated bindings and libraries
        println!(
            "cargo::rustc-env=NIMBLE_RS_SYS_BINDINGS_FILE={}",
            pregen_bindings_rs_file.display()
        );
        println!("cargo::rustc-link-search={}", pregen_libs_dir.display());
        println!("cargo::rustc-link-lib=static={}", NimbleBuilder::LIB_NIMBLE);
        println!(
            "cargo::rustc-link-lib=static={}",
            NimbleBuilder::LIB_TINYCRYPT
        );
    } else {
        if pregen_bindings_rs_file.exists() {
            if !pregen_bindings {
                println!(
                    "cargo::warning=Forcing an on-the-fly esp-nimble build for target {target} \
                     (`force-generate-bindings` is enabled)."
                );
            } else if let Err(delta) = &prebuilt_validity {
                println!(
                    "cargo::warning=Forcing an on-the-fly esp-nimble build for {target}: the \
                     selected features differ from the prebuilt config by: {delta}."
                );
            }
        }

        // On-the-fly build and bindings' generation.
        //
        // Note: no special case for `*-espidf` targets (unlike `openthread-sys`
        // and `mbedtls-rs-sys`, which defer to `esp-idf-sys` there): the whole
        // point of this crate is a NimBLE host decoupled from ESP-IDF's BT
        // component, so esp-nimble is compiled from source everywhere.
        let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());

        let builder = NimbleBuilder::new(crate_root_path, Some(target), Some(host));

        // `cc` emits the link directives itself
        builder.compile(&out)?;

        let bindings = builder.generate_bindings(&out)?;
        println!(
            "cargo::rustc-env=NIMBLE_RS_SYS_BINDINGS_FILE={}",
            bindings.display()
        );
    }

    Ok(())
}
