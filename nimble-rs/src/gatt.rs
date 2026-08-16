//! GATT: shared types and helpers.
//!
//! Modeled on the NimBLE GATT API of `esp-idf-svc` (`src/ble/gatt.rs`).

use core::ffi::c_int;

#[cfg(feature = "gatt-server")]
use enumset::{EnumSet, EnumSetType};

use nimble_rs_sys as sys;

use crate::{BleError, ConnHandle};

#[cfg(feature = "gatt-client")]
pub mod client;
#[cfg(feature = "gatt-server")]
pub mod server;

/// A GATT attribute handle (e.g. a characteristic's value handle).
pub type AttrHandle = u16;

/// Set the preferred ATT MTU. Safe to call before or after the host starts.
pub fn set_preferred_mtu(mtu: u16) -> Result<(), BleError> {
    BleError::check(unsafe { sys::ble_att_set_preferred_mtu(mtu) })
}

/// The negotiated ATT MTU for a connection.
pub fn att_mtu(conn_handle: ConnHandle) -> Result<u16, BleError> {
    match unsafe { sys::ble_att_mtu(conn_handle) } {
        0 => Err(BleError::new(sys::BLE_HS_ENOTCONN as c_int)),
        mtu => Ok(mtu),
    }
}

/// A GATT characteristic operation / permission flag (`BLE_GATT_CHR_F_*`).
///
/// The operation flags become the property bits of the characteristic
/// declaration (and gate what the ATT server lets a peer do); the `*Enc` /
/// `*Authen` flags additionally demand a security level before the access is
/// dispatched to the [`gatts_subscribe`](crate::BleDriver::gatts_subscribe)
/// hook.
#[cfg(feature = "gatt-server")]
#[derive(Debug, EnumSetType)]
pub enum BleGattCharFlag {
    /// The value may be broadcast (sets the Broadcast property bit).
    Broadcast,
    /// The peer may read the value.
    Read,
    /// The peer may write the value **without** a response (ATT Write Command).
    WriteNoRsp,
    /// The peer may write the value **with** a response (ATT Write Request).
    Write,
    /// The value may be notified (unacknowledged). NimBLE adds the CCCD for you.
    Notify,
    /// The value may be indicated (acknowledged). NimBLE adds the CCCD for you.
    Indicate,
    /// The peer may write the value with a signature (ATT Signed Write Command).
    AuthSignWrite,
    /// Sets the "Reliable Write" extended property (prepare/execute writes).
    ReliableWrite,
    /// Sets the "Writable Auxiliaries" extended property.
    AuxWrite,
    /// Reads require an encrypted link.
    ReadEnc,
    /// Reads require an encrypted link from an authenticated (MITM) pairing.
    ReadAuthen,
    /// Reads require authorization. **Not usable yet** - the authorize GAP
    /// event is not surfaced, and NimBLE rejects an unanswered request.
    ReadAuthor,
    /// Writes require an encrypted link.
    WriteEnc,
    /// Writes require an encrypted link from an authenticated (MITM) pairing.
    WriteAuthen,
    /// Writes require authorization. Same caveat as [`ReadAuthor`](Self::ReadAuthor).
    WriteAuthor,
    /// Subscribing (writing the CCCD) requires an encrypted link.
    NotifyIndicateEnc,
    /// Subscribing requires an encrypted link from an authenticated pairing.
    NotifyIndicateAuthen,
    /// Subscribing requires authorization. Same caveat as [`ReadAuthor`](Self::ReadAuthor).
    NotifyIndicateAuthor,
}

#[cfg(feature = "gatt-server")]
impl BleGattCharFlag {
    /// The raw NimBLE flag bit (`BLE_GATT_CHR_F_*`). `const`, so it can be
    /// used to build a static service table (see [`gatt_services!`](crate::gatt_services)).
    pub const fn repr(self) -> sys::ble_gatt_chr_flags {
        (match self {
            Self::Broadcast => sys::BLE_GATT_CHR_F_BROADCAST,
            Self::Read => sys::BLE_GATT_CHR_F_READ,
            Self::WriteNoRsp => sys::BLE_GATT_CHR_F_WRITE_NO_RSP,
            Self::Write => sys::BLE_GATT_CHR_F_WRITE,
            Self::Notify => sys::BLE_GATT_CHR_F_NOTIFY,
            Self::Indicate => sys::BLE_GATT_CHR_F_INDICATE,
            Self::AuthSignWrite => sys::BLE_GATT_CHR_F_AUTH_SIGN_WRITE,
            Self::ReliableWrite => sys::BLE_GATT_CHR_F_RELIABLE_WRITE,
            Self::AuxWrite => sys::BLE_GATT_CHR_F_AUX_WRITE,
            Self::ReadEnc => sys::BLE_GATT_CHR_F_READ_ENC,
            Self::ReadAuthen => sys::BLE_GATT_CHR_F_READ_AUTHEN,
            Self::ReadAuthor => sys::BLE_GATT_CHR_F_READ_AUTHOR,
            Self::WriteEnc => sys::BLE_GATT_CHR_F_WRITE_ENC,
            Self::WriteAuthen => sys::BLE_GATT_CHR_F_WRITE_AUTHEN,
            Self::WriteAuthor => sys::BLE_GATT_CHR_F_WRITE_AUTHOR,
            Self::NotifyIndicateEnc => sys::BLE_GATT_CHR_F_NOTIFY_INDICATE_ENC,
            Self::NotifyIndicateAuthen => sys::BLE_GATT_CHR_F_NOTIFY_INDICATE_AUTHEN,
            Self::NotifyIndicateAuthor => sys::BLE_GATT_CHR_F_NOTIFY_INDICATE_AUTHOR,
        }) as sys::ble_gatt_chr_flags
    }
}

#[cfg(feature = "gatt-server")]
impl From<BleGattCharFlag> for sys::ble_gatt_chr_flags {
    fn from(flag: BleGattCharFlag) -> Self {
        flag.repr()
    }
}

#[cfg(feature = "gatt-server")]
pub(crate) fn flags_to_repr(flags: EnumSet<BleGattCharFlag>) -> sys::ble_gatt_chr_flags {
    flags
        .iter()
        .fold(0, |acc, flag| acc | sys::ble_gatt_chr_flags::from(flag))
}
