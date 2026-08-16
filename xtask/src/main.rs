//! Repo tooling. Run from the workspace root as `cargo xtask <command>`
//! (see `.cargo/config.toml`).
//!
//! - `itest`: build and run the **upstream** NimBLE host test suite
//!   (`esp-nimble/nimble/host/test`: 36 suites / ~250 cases) against this
//!   crate's porting layer, via the excluded `tests/upstream` harness crate.
//! - `e2e`: this repo's own end-to-end tests, in two tiers:
//!   - **hermetic** (always): the `*_smoke` binaries against the in-process
//!     mock controller - no hardware, no privileges;
//!   - **btvirt** (when possible): the real example pairs over two BlueZ
//!     `btvirt` virtual controllers. Skipped with an explanation unless the
//!     prerequisites hold (see the README quickstart).
//! - `gen`: (future) regenerate the pre-built per-target bindings/libraries.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("itest") => itest(),
        Some("e2e") => e2e(),
        Some("gen") => bail!("`gen` (pre-built bindings/libs) is not implemented yet"),
        other => bail!("usage: cargo xtask <itest|e2e|gen> (got {other:?})"),
    }
}

fn root() -> PathBuf {
    // xtask lives in <root>/xtask
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn bin(name: &str) -> PathBuf {
    root().join("target/debug").join(name)
}

/// Runs `program` capturing combined output, killing it after `timeout`.
/// Returns (killed-by-timeout, output).
fn run_captured(program: &Path, args: &[&str], timeout: Duration) -> Result<(bool, String)> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning {}", program.display()))?;

    let timed_out = !wait_timeout(&mut child, timeout)?;
    if timed_out {
        let _ = child.kill();
        let _ = child.wait();
    }

    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        stdout.read_to_string(&mut output).ok();
    }
    if let Some(mut stderr) = child.stderr.take() {
        stderr.read_to_string(&mut output).ok();
    }

    Ok((timed_out, output))
}

/// Waits for the child up to `timeout`; true if it exited by itself.
fn wait_timeout(child: &mut Child, timeout: Duration) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn build_examples() -> Result<()> {
    println!("== building examples");
    let status = Command::new("cargo")
        .current_dir(root())
        .args(["build", "-p", "nimble-rs-examples-std"])
        .status()?;
    if !status.success() {
        bail!("build failed");
    }
    Ok(())
}

/// Tier 1: the hermetic smoke gates (mock controller; CI-safe).
fn hermetic() -> Result<()> {
    for (name, markers) in [
        ("smoke", &["SYNC OK", "RE-INIT OK"] as &[&str]),
        ("gatts_smoke", &["GATT SMOKE OK"]),
        ("gattc_smoke", &["GATTC SMOKE OK"]),
    ] {
        print!("== hermetic: {name} ... ");
        let (timed_out, output) = run_captured(&bin(name), &[], Duration::from_secs(30))?;

        if timed_out {
            bail!("{name} timed out\n--- output ---\n{output}");
        }
        for marker in markers {
            if !output.contains(marker) {
                bail!("{name}: marker {marker:?} missing\n--- output ---\n{output}");
            }
        }
        println!("ok");
    }
    Ok(())
}

/// Whether the btvirt tier's prerequisites hold: two virtual, *down* HCI
/// devices and the ability to bind them (CAP_NET_ADMIN on the binaries or a
/// privileged run). Probing beats guessing: try to bind hci<idx> briefly.
fn btvirt_ready(dev: u16) -> bool {
    if !Path::new(&format!("/sys/class/bluetooth/hci{dev}")).exists() {
        println!("   (hci{dev} not present)");
        return false;
    }

    // `scanner` binds the device and errors out immediately when it cannot
    match run_captured(
        &bin("scanner"),
        &[&dev.to_string()],
        Duration::from_secs(3),
    ) {
        // A clean early *exit* means a bind error was printed; a timeout kill
        // means it got as far as scanning - i.e. the bind worked
        Ok((timed_out, output)) => {
            if !timed_out && (output.contains("Operation not permitted") || output.contains("os error")) {
                println!("   (cannot bind hci{dev}: {})", output.lines().last().unwrap_or(""));
                false
            } else {
                true
            }
        }
        Err(e) => {
            println!("   ({e})");
            false
        }
    }
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Runs a server/client example pair over hci<a>/hci<b> and asserts that at
/// least `min_hits` of `client_marker` / `server_marker` appear.
fn pair(
    server_cmd: &[&str],
    client_cmd: &[&str],
    server_marker: &str,
    client_marker: &str,
    min_hits: usize,
) -> Result<()> {
    let server = Command::new(bin(server_cmd[0]))
        .args(&server_cmd[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    std::thread::sleep(Duration::from_secs(2));
    let mut server = KillOnDrop(server);

    let (_, client_output) = run_captured(
        &bin(client_cmd[0]),
        &client_cmd[1..],
        Duration::from_secs(12),
    )?;

    let _ = server.0.kill();
    let _ = server.0.wait();
    let mut server_output = String::new();
    if let Some(mut stdout) = server.0.stdout.take() {
        stdout.read_to_string(&mut server_output).ok();
    }
    if let Some(mut stderr) = server.0.stderr.take() {
        stderr.read_to_string(&mut server_output).ok();
    }

    let client_hits = client_output.matches(client_marker).count();
    let server_hits = server_output.matches(server_marker).count();

    if client_hits < min_hits || server_hits < min_hits {
        bail!(
            "expected >={min_hits}x {client_marker:?} (got {client_hits}) and \
             >={min_hits}x {server_marker:?} (got {server_hits})\n\
             --- client ---\n{client_output}\n--- server ---\n{server_output}"
        );
    }

    Ok(())
}

/// Tier 2: the real example pairs over btvirt.
fn btvirt(dev_a: u16, dev_b: u16) -> Result<bool> {
    println!("== btvirt tier (hci{dev_a} <-> hci{dev_b})");
    if !btvirt_ready(dev_a) || !btvirt_ready(dev_b) {
        println!("== btvirt tier SKIPPED (see the README quickstart to enable it)");
        return Ok(false);
    }

    let a = dev_a.to_string();
    let b = dev_b.to_string();

    print!("== btvirt: gatt_server <-> gatt_client ... ");
    pair(
        &["gatt_server", &a],
        &["gatt_client", &b],
        "recv 4 bytes",
        "indication on",
        3,
    )?;
    println!("ok");

    print!("== btvirt: l2cap server <-> client ... ");
    pair(
        &["l2cap", "server", &a],
        &["l2cap", "client", &b],
        "echoing",
        "echo received",
        3,
    )?;
    println!("ok");

    Ok(true)
}

fn e2e() -> Result<()> {
    build_examples()?;
    hermetic()?;
    let ran_btvirt = btvirt(0, 1)?;

    println!(
        "\ne2e PASSED (hermetic{})",
        if ran_btvirt { " + btvirt" } else { "; btvirt skipped" }
    );
    Ok(())
}

/// Builds and runs the upstream NimBLE host test suite through the excluded
/// `tests/upstream` harness crate.
fn itest() -> Result<()> {
    println!("== building + running the upstream NimBLE host test suite");
    let status = Command::new("cargo")
        .current_dir(root().join("tests/upstream"))
        .args(["run"])
        .status()?;
    if !status.success() {
        bail!("upstream test suite FAILED");
    }
    println!("\nitest PASSED (upstream host test suite)");
    Ok(())
}
