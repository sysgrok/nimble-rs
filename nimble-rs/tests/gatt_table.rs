//! Walks the raw `ble_gatt_svc_def` tree produced by the runtime service
//! table builder (`BleGattServices`), verifying the C-heap pointer graph:
//! per-service characteristic arrays with terminators, stable UUID storage,
//! and the final service terminator.

use enumset::enum_set;

use nimble_rs::gatt::server::{BleGattCharacteristic, BleGattService, BleGattServices};
use nimble_rs::gatt::BleGattCharFlag;
use nimble_rs::BleUuid;

#[test]
fn gatt_table() {
    let chars_a = [
        BleGattCharacteristic::new(BleUuid::uuid16(0x1111), enum_set!(BleGattCharFlag::Write)),
        BleGattCharacteristic::new(
            BleUuid::uuid16(0x2222),
            enum_set!(BleGattCharFlag::Read | BleGattCharFlag::Indicate),
        ),
    ];
    let chars_b = [BleGattCharacteristic::new(
        BleUuid::uuid16(0x3333),
        enum_set!(BleGattCharFlag::Notify),
    )];

    let services = BleGattServices::new(&[
        BleGattService::new(true, BleUuid::uuid16(0xaaaa), &chars_a),
        BleGattService::new(false, BleUuid::uuid16(0xbbbb), &chars_b),
    ])
    .unwrap();

    let defs = services.as_ref();
    assert_eq!(defs.len(), 3);

    // Service entries + terminator
    assert!(!defs[0].uuid.is_null() && !defs[1].uuid.is_null());
    assert!(defs[2].uuid.is_null());
    assert_eq!(
        defs[0].type_,
        nimble_rs_sys::BLE_GATT_SVC_TYPE_PRIMARY as u8
    );
    assert_eq!(
        defs[1].type_,
        nimble_rs_sys::BLE_GATT_SVC_TYPE_SECONDARY as u8
    );

    // Per-service characteristic arrays: correct lengths, terminators, and
    // the flat layout (service B's array starts right after A's terminator)
    for (svc, expected) in defs[..2].iter().zip([&chars_a[..], &chars_b[..]]) {
        let mut chr = svc.characteristics;
        for _ in expected {
            let def = unsafe { &*chr };
            assert!(!def.uuid.is_null());
            assert!(def.access_cb.is_some());
            chr = unsafe { chr.add(1) };
        }
        assert!(unsafe { &*chr }.uuid.is_null(), "missing terminator");
    }
    assert_eq!(
        unsafe { defs[0].characteristics.add(chars_a.len() + 1) },
        defs[1].characteristics
    );

    // UUID storage: the 16-bit UUIDs round-trip through the raw pointers
    let uuid16 = |p: *const nimble_rs_sys::ble_uuid_t| unsafe {
        assert_eq!((*p).type_, nimble_rs_sys::BLE_UUID_TYPE_16 as u8);
        (*p.cast::<nimble_rs_sys::ble_uuid16_t>()).value
    };
    assert_eq!(uuid16(defs[0].uuid), 0xaaaa);
    assert_eq!(uuid16(defs[1].uuid), 0xbbbb);
    assert_eq!(uuid16(unsafe { &*defs[0].characteristics }.uuid), 0x1111);
    assert_eq!(
        uuid16(unsafe { &*defs[0].characteristics.add(1) }.uuid),
        0x2222
    );
    assert_eq!(uuid16(unsafe { &*defs[1].characteristics }.uuid), 0x3333);
}
