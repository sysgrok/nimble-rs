//! The GATT server scenario.

use core::cell::{Cell, RefCell};
use core::sync::atomic::{AtomicU16, Ordering};

use critical_section::Mutex;

use nimble_rs::gap::{BleAdvFields, BleAdvParams, GapEvent};
use nimble_rs::gatt::server::{BleGattRegister, GattsEvent};
use nimble_rs::{gatt_services, Ble, BleError, ConnHandle, Controller, HostEvent, Parker};

use crate::{DEVICE_NAME, IND_CHARACTERISTIC_UUID, RECV_CHARACTERISTIC_UUID, SERVICE_UUID};

// The whole GATT service table, built at compile time and living in static
// storage.
gatt_services!(SERVICES {
    primary(SERVICE_UUID) {
        chr(RECV_CHARACTERISTIC_UUID, Write);
        chr(IND_CHARACTERISTIC_UUID, Indicate);
    }
});

const MAX_SUBSCRIBERS: usize = 4;

// Server state, captured from the `Register` events / connection callbacks.
static SUBSCRIBERS: Mutex<RefCell<heapless::Vec<ConnHandle, MAX_SUBSCRIBERS>>> =
    Mutex::new(RefCell::new(heapless::Vec::new()));
static IND_VAL_HANDLE: AtomicU16 = AtomicU16::new(0);
static RECV_VAL_HANDLE: AtomicU16 = AtomicU16::new(0);
// Not an atomic bool + swap: RMW atomics do not exist on e.g. thumbv6-m
static NEEDS_ADV: Mutex<Cell<bool>> = Mutex::new(Cell::new(false));

fn needs_adv_swap(new: bool) -> bool {
    critical_section::with(|cs| NEEDS_ADV.borrow(cs).replace(new))
}

fn on_host_event(event: HostEvent) {
    // Advertise once the stack is in sync; re-armed on reset
    if let HostEvent::Sync = event {
        needs_adv_swap(true);
    }
}

fn on_gap_event(event: GapEvent) -> i32 {
    match event {
        GapEvent::Connect { conn_handle, .. } => {
            info!("connected (handle {})", conn_handle);
        }
        GapEvent::Disconnect {
            conn_handle,
            reason,
        } => {
            info!("disconnected (reason {}); re-advertising", reason);
            critical_section::with(|cs| {
                SUBSCRIBERS.borrow_ref_mut(cs).retain(|&c| c != conn_handle)
            });
            needs_adv_swap(true);
        }
        _ => {}
    }

    0
}

fn on_gatts_event(event: GattsEvent) -> u8 {
    match event {
        GattsEvent::Register(BleGattRegister::Characteristic {
            uuid, val_handle, ..
        }) => {
            if uuid == IND_CHARACTERISTIC_UUID {
                IND_VAL_HANDLE.store(val_handle, Ordering::Relaxed);
            } else if uuid == RECV_CHARACTERISTIC_UUID {
                RECV_VAL_HANDLE.store(val_handle, Ordering::Relaxed);
            }
        }
        GattsEvent::Write {
            attr_handle, data, ..
        } if attr_handle == RECV_VAL_HANDLE.load(Ordering::Relaxed) => {
            let mut buf = [0u8; 64];
            match data.read(&mut buf) {
                Ok(n) => info!("recv {} bytes", n),
                Err(e) => warning!("recv read failed (error {})", e.code()),
            }
        }
        GattsEvent::SubscriptionChanged {
            conn_handle,
            attr_handle,
            cur_indicate,
            ..
        } if attr_handle == IND_VAL_HANDLE.load(Ordering::Relaxed) => {
            critical_section::with(|cs| {
                let mut subs = SUBSCRIBERS.borrow_ref_mut(cs);
                subs.retain(|&c| c != conn_handle);
                if cur_indicate {
                    let _ = subs.push(conn_handle);
                }
            });
        }
        _ => {}
    }

    0
}

/// Configure and start a connectable legacy advertisement.
fn start_advertising<S>(ble: &Ble<S>) -> Result<(), BleError> {
    // Infers the address type to advertise with - public where the
    // controller has one (Linux, ESP), static random otherwise (nRF)
    let (_, own_addr_type) = ble.address()?;

    ble.set_device_name(DEVICE_NAME)?;

    ble.adv_set_fields(&BleAdvFields {
        flags: 0x06, // LE General Discoverable, BR/EDR unsupported
        name: Some(DEVICE_NAME),
        ..Default::default()
    })?;

    ble.adv_start(
        own_addr_type,
        &BleAdvParams {
            conn_mode: 2,   // BLE_GAP_CONN_MODE_UND
            disc_mode: 2,   // BLE_GAP_DISC_MODE_GEN
            itvl_min: 0x30, // 30 ms, in 0.625 ms units
            itvl_max: 0x60, // 60 ms
            ..Default::default()
        },
    )
}

/// Runs the GATT server over the given controller, forever.
///
/// A custom [`Parker`] may be injected (e.g. an RTOS-backed one); `None`
/// keeps the target's built-in default (thread parking on `std`, WFE on
/// bare-metal ARM, spinning elsewhere).
pub async fn run<C: Controller>(controller: C, parker: Option<&dyn Parker>) -> ! {
    let ble = match parker {
        Some(parker) => Ble::new_with_services_and_parker(&SERVICES, parker),
        None => Ble::new_with_services(&SERVICES),
    };
    let ble = match ble {
        Ok(ble) => ble,
        Err(e) => panic!("BLE init failed: {}", e.code()),
    };

    ble.host_subscribe(&on_host_event);
    ble.gap_subscribe(&on_gap_event);
    ble.gatts_subscribe(&on_gatts_event);

    info!("NimBLE host starting");

    embassy_futures::select::select(ble.run(controller), async {
        // The application loop: (re)advertise when needed and push the
        // counter to subscribers, once a second
        let mut counter: u16 = 0;
        loop {
            embassy_time::Timer::after_millis(1000).await;

            if needs_adv_swap(false) {
                match start_advertising(&ble) {
                    Ok(()) => info!("advertising"),
                    Err(e) => warning!("failed to start advertising (error {})", e.code()),
                }
            }

            let ind_handle = IND_VAL_HANDLE.load(Ordering::Relaxed);
            if ind_handle == 0 {
                continue;
            }

            counter = counter.wrapping_add(1);

            // Copy the list out so the borrow isn't held across `indicate`
            let subs = critical_section::with(|cs| SUBSCRIBERS.borrow_ref(cs).clone());
            for conn in subs {
                if let Err(e) = ble.indicate(conn, ind_handle, &counter.to_le_bytes()) {
                    warning!("indicate failed (error {})", e.code());
                }
            }
        }
    })
    .await;

    panic!("BLE host stopped unexpectedly");
}
