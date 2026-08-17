//! Repo tooling. Run from the workspace root as `cargo xtask <command>`
//! (see `.cargo/config.toml`).
//!
//! - `e2e`: the end-to-end tier that genuinely needs orchestration - the
//!   real example binaries running as pairs over two BlueZ `btvirt` virtual
//!   controllers (two processes, an external daemon, `CAP_NET_ADMIN` on the
//!   binaries). Skipped with an explanation unless the prerequisites hold
//!   (see the README quickstart).
//! - `gen`: (future) regenerate the pre-built per-target bindings/libraries.
//!
//! Everything that *can* be a plain cargo test is one: the hermetic
//! mock-controller tests are `cargo test -p nimble-rs --features l2cap`, and
//! the upstream NimBLE host unit suite is the `tests/upstream` crate
//! (`cargo run -p nimble-rs-upstream-tests`; a workspace member that is
//! never built in the same invocation as the other members - see the root
//! manifest).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("e2e") => e2e(),
        Some("gen") => bail!("`gen` (pre-built bindings/libs) is not implemented yet"),
        other => bail!("usage: cargo xtask <e2e|gen> (got {other:?})"),
    }
}

fn root() -> PathBuf {
    // xtask lives in <root>/xtask
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
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

/// Whether the btvirt tier's prerequisites hold: two virtual, *down* HCI
/// devices and the ability to bind them (CAP_NET_ADMIN on the binaries or a
/// privileged run). Probing beats guessing: try to bind hci<idx> briefly.
fn btvirt_ready(dev: u16) -> bool {
    if !Path::new(&format!("/sys/class/bluetooth/hci{dev}")).exists() {
        println!("   (hci{dev} not present)");
        return false;
    }

    // `scanner` binds the device and errors out immediately when it cannot
    match run_captured(&bin("scanner"), &[&dev.to_string()], Duration::from_secs(3)) {
        // A clean early *exit* means a bind error was printed; a timeout kill
        // means it got as far as scanning - i.e. the bind worked
        Ok((timed_out, output)) => {
            if !timed_out
                && (output.contains("Operation not permitted") || output.contains("os error"))
            {
                println!(
                    "   (cannot bind hci{dev}: {})",
                    output.lines().last().unwrap_or("")
                );
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

    if btvirt(0, 1)? {
        println!("\ne2e PASSED (btvirt)");
    } else {
        println!("\ne2e SKIPPED (btvirt prerequisites not met)");
    }
    Ok(())
}
