//! An L2CAP CoC echo demo over Linux HCI controllers, replacing GATT with a
//! credit-based data pipe. A port of esp-idf-svc's `ble_l2cap.rs`.
//!
//! Run as the echo **server** (advertises, accepts a channel on the PSM and
//! echoes every SDU back):
//!
//!     l2cap server [hci-index]
//!
//! or as the **client** (scans for the server by name, connects, opens the
//! channel and sends a counter once a second, printing the echoes):
//!
//!     l2cap client [hci-index]

// The example itself is Linux-only (it drives a BlueZ
// `HCI_CHANNEL_USER` device); everything it demonstrates is portable.
#![cfg_attr(not(target_os = "linux"), allow(unused))]

use core::cell::Cell;
use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use log::{info, warn};

use bt_hci::controller::ExternalController;
use nimble_rs::gap::{BleAdvFields, BleAdvParams, BleDiscParams, GapEvent};
use nimble_rs::l2cap::{L2capChan, L2capEvent};

use nimble_rs::{Ble, BleAddr, HostEvent};

const DEVICE_NAME: &str = "nimble-rs-l2cap";
const PSM: u16 = 0x0080;
const MTU: u16 = 512;

struct SyncCell<T>(Cell<Option<T>>);
unsafe impl<T> Sync for SyncCell<T> {}

static SYNCED: AtomicBool = AtomicBool::new(false);
static SERVER: AtomicBool = AtomicBool::new(false);
static PEER: SyncCell<BleAddr> = SyncCell(Cell::new(None));
static CONN: AtomicU16 = AtomicU16::new(0);
static CONNECTED: AtomicBool = AtomicBool::new(false);
static DISCONNECTED: AtomicBool = AtomicBool::new(false);
static CHAN: SyncCell<L2capChan> = SyncCell(Cell::new(None));

fn on_host_event(event: HostEvent) {
    if event == HostEvent::Sync {
        SYNCED.store(true, Ordering::Relaxed);
    }
}

fn adv_has_name(data: &[u8]) -> bool {
    let mut rest = data;
    while let [len, ad_type, ..] = *rest {
        let len = len as usize;
        if len == 0 || rest.len() < 1 + len {
            break;
        }
        if ad_type == 0x09 && &rest[2..1 + len] == DEVICE_NAME.as_bytes() {
            return true;
        }
        rest = &rest[1 + len..];
    }
    false
}

fn on_gap_event(event: GapEvent) -> i32 {
    match event {
        GapEvent::Discovery { addr, data, .. } => {
            if adv_has_name(data) && PEER.0.get().is_none() {
                info!("found {DEVICE_NAME:?} at {addr:?}");
                PEER.0.set(Some(addr));
            }
        }
        GapEvent::Connect {
            conn_handle,
            status,
        } => {
            info!("connected (handle {conn_handle}): {status:?}");
            if status.is_ok() {
                CONN.store(conn_handle, Ordering::Relaxed);
                CONNECTED.store(true, Ordering::Relaxed);
            }
        }
        GapEvent::Disconnect { reason, .. } => {
            info!("disconnected: {reason}");
            DISCONNECTED.store(true, Ordering::Relaxed);
            CHAN.0.set(None);
        }
        _ => {}
    }

    0
}

/// One hook for both roles; only who opens the channel differs.
fn on_l2cap_event(event: L2capEvent) -> i32 {
    match event {
        // Server side: incoming channel - provide the first receive buffer
        L2capEvent::Accept {
            chan,
            peer_sdu_size,
            ..
        } => {
            info!("accepting L2CAP channel (peer SDU size {peer_sdu_size})");
            // Reject by returning non-zero instead, if desired
            if let Err(e) = server_driver_recv_ready(chan) {
                warn!("recv_ready failed: {e}");
                return 1;
            }
            CHAN.0.set(Some(chan));
        }
        // Client side: our connect finished
        L2capEvent::Connected { status, chan, .. } => {
            info!("L2CAP channel connected, status {status}");
            if status == 0 {
                CHAN.0.set(Some(chan));
            }
        }
        L2capEvent::Received { chan, data, .. } => {
            let mut buf = [0u8; MTU as usize];
            let n = data.read(&mut buf).unwrap_or(0);

            if SERVER.load(Ordering::Relaxed) {
                info!("echoing {n} bytes");
                if let Err(e) = server_driver_echo(chan, &buf[..n]) {
                    warn!("echo failed: {e}");
                }
            } else {
                info!("echo received: {:?}", &buf[..n]);
            }

            if let Err(e) = server_driver_recv_ready(chan) {
                warn!("recv_ready failed: {e}");
            }
        }
        L2capEvent::Disconnected { .. } => {
            info!("L2CAP channel disconnected");
            CHAN.0.set(None);
        }
        _ => {}
    }

    0
}

// The hooks are plain `fn`s, so reach the driver through a static handle.
// (The driver methods used here don't touch the `S` type parameter.)
static DRIVER: SyncCell<&'static Ble> = SyncCell(Cell::new(None));

fn server_driver_recv_ready(chan: L2capChan) -> Result<(), nimble_rs::BleError> {
    DRIVER.0.get().unwrap().l2cap_recv_ready(chan, MTU)
}

fn server_driver_echo(chan: L2capChan, data: &[u8]) -> Result<(), nimble_rs::BleError> {
    DRIVER.0.get().unwrap().l2cap_send(chan, data).map(|_| ())
}

async fn wait(flag: &AtomicBool) {
    while !flag.load(Ordering::Relaxed) {
        embassy_time::Timer::after_millis(10).await;
    }
}

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    futures_lite::future::block_on(amain())
}

#[cfg(target_os = "linux")]
async fn amain() -> anyhow::Result<()> {
    env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .init();

    let mut args = std::env::args().skip(1);
    let role = args.next().unwrap_or_else(|| "server".into());
    let dev: u16 = args.next().map(|a| a.parse()).transpose()?.unwrap_or(0);
    let server = match role.as_str() {
        "server" => true,
        "client" => false,
        other => anyhow::bail!("unknown role {other:?}; use `server` or `client`"),
    };
    SERVER.store(server, Ordering::Relaxed);

    let controller =
        ExternalController::<_, 1>::new(nimble_rs_examples_std::linux::Transport::new(dev)?);

    let driver = Box::leak(Box::new(Ble::new()?));
    DRIVER.0.set(Some(driver));
    driver.host_subscribe(&on_host_event);
    driver.gap_subscribe(&on_gap_event);
    driver.l2cap_subscribe(&on_l2cap_event);

    info!(
        "L2CAP {} starting on hci{dev}",
        if server { "server" } else { "client" }
    );

    embassy_futures::select::select(driver.run(controller), async {
        wait(&SYNCED).await;

        if server {
            driver.l2cap_create_server(PSM, MTU).expect("create server");

            driver.set_device_name(DEVICE_NAME).expect("name");
            driver
                .adv_set_fields(&BleAdvFields {
                    flags: 0x06,
                    name: Some(DEVICE_NAME),
                    ..Default::default()
                })
                .expect("adv fields");
            driver
                .adv_start(
                    0,
                    &BleAdvParams {
                        conn_mode: 2,
                        disc_mode: 2,
                        ..Default::default()
                    },
                )
                .expect("adv start");
            info!("advertising as {DEVICE_NAME:?}; echoing on PSM {PSM:#06x}");

            // Everything else happens in the hooks
            core::future::pending::<()>().await;
        } else {
            // Client: find the server, connect, open the channel, send
            driver
                .disc(0, None, &BleDiscParams::default())
                .expect("disc");
            while PEER.0.get().is_none() {
                embassy_time::Timer::after_millis(50).await;
            }
            driver.disc_cancel().ok();

            driver.connect(0, &PEER.0.get().unwrap()).expect("connect");
            wait(&CONNECTED).await;

            let conn = CONN.load(Ordering::Relaxed);
            driver.l2cap_connect(conn, PSM, MTU).expect("l2cap connect");
            while CHAN.0.get().is_none() {
                embassy_time::Timer::after_millis(10).await;
            }

            let mut counter: u32 = 0;
            while !DISCONNECTED.load(Ordering::Relaxed) {
                embassy_time::Timer::after_millis(1000).await;

                let Some(chan) = CHAN.0.get() else { break };
                counter = counter.wrapping_add(1);
                if let Err(e) = driver.l2cap_send(chan, &counter.to_le_bytes()) {
                    warn!("send failed: {e}");
                }
            }
        }
    })
    .await;

    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("this example requires Linux (it drives a BlueZ HCI_CHANNEL_USER device)");
}
