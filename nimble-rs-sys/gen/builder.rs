//! Compiles the esp-nimble host stack with `cc` and generates bindings with
//! `bindgen`. Shared between `build.rs` (via `#[path]`) and (later) `xtask`.
//!
//! There is no CMake involved: esp-nimble has no standalone CMake build, so the
//! canonical host-only source list of `porting/nimble/Makefile.defs` (plus the
//! exclusions below) is mirrored here directly.

#[path = "features.rs"]
pub mod features;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Files under `porting/nimble/src` which are not compiled:
/// - `hal_timer.c`, `os_cputime.c`, `os_cputime_pwr2.c`: controller-only timing
///   (also on the upstream Linux sample's ignore list);
/// - `hal_uart.c`: FreeRTOS/ESP-specific UART shim;
/// - `nimble_port.c`: unconditionally includes FreeRTOS/`soc/soc_caps.h`/
///   `esp_log.h`; its small contract (`nimble_port_init/deinit/run/stop`,
///   `nimble_port_get_dflt_eventq`) is re-implemented in Rust by `nimble-rs`.
/// - `os_mempool.c`: replaced by the 64-bit-correct vendored copy in
///   `gen/glue/src` (the original truncates pointers through `uint32_t`).
const PORTING_SRC_EXCLUDES: &[&str] = &[
    "hal_timer.c",
    "hal_uart.c",
    "os_cputime.c",
    "os_cputime_pwr2.c",
    "nimble_port.c",
    "os_mempool.c",
];

pub struct NimbleBuilder {
    crate_root: PathBuf,
    target: Option<String>,
    host: Option<String>,
}

impl NimbleBuilder {
    pub const LIB_NIMBLE: &'static str = "nimble";
    pub const LIB_TINYCRYPT: &'static str = "nimble-tinycrypt";

    /// Emit `cargo::rerun-if-changed` for a path.
    pub fn track(path: &Path) {
        println!("cargo::rerun-if-changed={}", path.display());
    }

    pub const fn new(crate_root: PathBuf, target: Option<String>, host: Option<String>) -> Self {
        Self {
            crate_root,
            target,
            host,
        }
    }

    fn nimble_root(&self) -> PathBuf {
        self.crate_root.join("esp-nimble")
    }

    fn glue_include_dir(&self) -> PathBuf {
        self.crate_root.join("gen").join("glue").join("include")
    }

    /// All include directories, in search order. The glue directory comes
    /// first so that its stand-in headers (`esp_err.h`, `esp_nimble_mem.h`,
    /// `bt_common.h`, `nimble/nimble_npl_os.h`) take precedence.
    pub fn include_dirs(&self) -> Vec<PathBuf> {
        let root = self.nimble_root();

        vec![
            self.glue_include_dir(),
            root.join("nimble/include"),
            root.join("nimble/host/include"),
            root.join("nimble/host/services/gap/include"),
            root.join("nimble/host/services/gatt/include"),
            root.join("nimble/host/store/ram/include"),
            root.join("nimble/host/util/include"),
            root.join("nimble/transport/include"),
            root.join("porting/nimble/include"),
            root.join("ext/tinycrypt/include"),
        ]
    }

    /// All preprocessor defines: the full `MYNEWT_VAL_*` universe (see
    /// `features::VAL_UNIVERSE` for why every knob is passed explicitly) plus
    /// the extra `CONFIG_*` defines the esp-nimble fork requires.
    pub fn defines(&self) -> Vec<(String, String)> {
        let mut defines: Vec<(String, String)> = features::active_val_settings()
            .into_iter()
            .map(|(name, value)| (format!("MYNEWT_VAL_{name}"), value))
            .chain(
                features::EXTRA_DEFINES
                    .iter()
                    .map(|(name, value)| (name.to_string(), value.to_string())),
            )
            .collect();

        if std::env::var_os("CARGO_FEATURE_UPSTREAM_TEST").is_some() {
            defines.extend(
                features::UPSTREAM_TEST_EXTRA_DEFINES
                    .iter()
                    .map(|(name, value)| (name.to_string(), value.to_string())),
            );
        }

        defines
    }

    fn glob_c(dir: &Path, excludes: &[&str]) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();

        for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = entry?.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();

            if path.extension().is_some_and(|ext| ext == "c") && !excludes.contains(&&*name) {
                files.push(path);
            }
        }

        files.sort();

        Ok(files)
    }

    /// The host-stack C source list (everything except tinycrypt).
    pub fn sources(&self) -> Result<Vec<PathBuf>> {
        let root = self.nimble_root();

        let mut files = Vec::new();
        files.extend(Self::glob_c(
            &root.join("porting/nimble/src"),
            PORTING_SRC_EXCLUDES,
        )?);
        files.extend(Self::glob_c(&root.join("nimble/host/src"), &[])?);
        files.extend(Self::glob_c(&root.join("nimble/host/util/src"), &[])?);
        files.extend(Self::glob_c(
            &root.join("nimble/host/services/gap/src"),
            &[],
        )?);
        files.extend(Self::glob_c(
            &root.join("nimble/host/services/gatt/src"),
            &[],
        )?);
        files.extend(Self::glob_c(&root.join("nimble/host/store/ram/src"), &[])?);
        // Only the generic dispatch + pools; per-chip transport backends are
        // replaced by the Rust `ble_transport_ll_*` implementation.
        files.push(root.join("nimble/transport/src/transport.c"));
        // The 64-bit-correct replacement of `porting/nimble/src/os_mempool.c`
        files.push(self.crate_root.join("gen/glue/src/os_mempool.c"));

        Ok(files)
    }

    fn configure(&self, build: &mut cc::Build, out_dir: &Path) {
        build.out_dir(out_dir);

        if let Some(target) = &self.target {
            build.target(target);
        }
        if let Some(host) = &self.host {
            build.host(host);
        }

        build
            .flag_if_supported("-ffunction-sections")
            .flag_if_supported("-fdata-sections")
            .warnings(false);
    }

    /// Compiles `libnimble.a` and `libnimble-tinycrypt.a` into `out_dir`.
    ///
    /// `cc` emits the `cargo::rustc-link-lib`/`link-search` directives itself,
    /// in call order (dependents before dependencies, as the linker requires).
    pub fn compile(&self, out_dir: &Path) -> Result<PathBuf> {
        let root = self.nimble_root();

        let mut build = cc::Build::new();
        self.configure(&mut build, out_dir);

        // `nimble/transport.h` uses `esp_err_t` without including `esp_err.h`
        // itself (it normally arrives via `nimble/nimble_port.h`); force-include
        // the glue header so every translation unit sees it.
        build.flag("-include").flag("esp_err.h");

        for dir in self.include_dirs() {
            build.include(dir);
        }
        for (name, value) in self.defines() {
            build.define(&name, value.as_str());
        }
        for file in self.sources()? {
            build.file(file);
        }

        build.compile(Self::LIB_NIMBLE);

        // Export the include paths and the exact configuration to dependent
        // build scripts (DEP_NIMBLE_INCLUDE / DEP_NIMBLE_DEFINES): anything
        // compiling more C against this build (e.g. the upstream test
        // harness) must use the same config or the ABI diverges.
        println!(
            "cargo::metadata=include={}",
            self.include_dirs()
                .iter()
                .map(|dir| dir.display().to_string())
                .collect::<Vec<_>>()
                .join(";")
        );
        println!(
            "cargo::metadata=defines={}",
            self.defines()
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join(";")
        );

        // tinycrypt (SM pairing crypto), as its own archive, linked after the
        // host stack which depends on it.
        let mut build = cc::Build::new();
        self.configure(&mut build, out_dir);

        build.include(root.join("ext/tinycrypt/include"));
        build.flag_if_supported("-std=c99");
        for file in Self::glob_c(&root.join("ext/tinycrypt/src"), &[])? {
            build.file(file);
        }

        build.compile(Self::LIB_TINYCRYPT);

        Ok(out_dir.to_path_buf())
    }

    /// Generates `bindings.rs` into `out_dir` and returns its path.
    pub fn generate_bindings(&self, out_dir: &Path) -> Result<PathBuf> {
        let mut builder = bindgen::Builder::default()
            .header(
                self.crate_root
                    .join("gen")
                    .join("include")
                    .join("include.h")
                    .display()
                    .to_string(),
            )
            .use_core()
            .derive_debug(false)
            .derive_default(true)
            .layout_tests(false)
            .allowlist_item("ble_.*")
            .allowlist_item("BLE_.*")
            .allowlist_item("os_.*")
            .allowlist_item("OS_.*")
            .allowlist_item("nimble_port_.*")
            .allowlist_item("nimble_platform_mem_.*")
            .allowlist_item("esp_err_t")
            .allowlist_item("MYNEWT_VAL_.*");

        for dir in self.include_dirs() {
            builder = builder.clang_arg(format!("-I{}", dir.display()));
        }
        for (name, value) in self.defines() {
            builder = builder.clang_arg(format!("-D{name}={value}"));
        }

        // Cross-compilation: point clang at the Rust target triple, so that
        // type layouts (and the MYNEWT_VAL constants) match the target.
        if let (Some(target), Some(host)) = (&self.target, &self.host) {
            if target != host {
                builder = builder.clang_arg(format!("--target={target}"));
            }
        }

        let bindings = builder
            .generate()
            .context("bindgen failed for esp-nimble")?;

        let path = out_dir.join("bindings.rs");
        bindings
            .write_to_file(&path)
            .with_context(|| format!("writing {}", path.display()))?;

        // bindgen emits `MYNEWT_VAL_*` constants only for knobs defaulted in
        // `syscfg.h`; the ones this crate controls arrive as command-line
        // defines, which bindgen skips. Append those explicitly - consumers
        // size buffers and pools from them (there is no collision: a knob is
        // either `-D`-defined here or defaulted in the header, never both).
        {
            use std::io::Write;

            let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;

            writeln!(file)?;
            for (name, value) in features::active_val_settings() {
                writeln!(file, "pub const MYNEWT_VAL_{name}: u32 = {value};")?;
            }
        }

        Ok(path)
    }
}
