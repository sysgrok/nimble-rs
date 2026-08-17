//! A mock BLE controller: a minimal in-process HCI responder implementing
//! `bt_hci::transport::Transport`.
//!
//! Every HCI command gets a plausible Command Complete (from a small table of
//! non-trivial responses); ACL data from the host is exposed through
//! [`host_acl`], and test code can inject controller-to-host packets with
//! [`inject_event`] / [`inject_acl`] - enough to simulate a central
//! connecting and exchanging ATT traffic, hermetically (no HCI hardware, no
//! privileges).
//!
//! Shared by the integration tests via `#[path]` inclusion; each test binary
//! uses a different slice of this API.
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};

use bt_hci::transport::Transport;
use bt_hci::{ControllerToHostPacket, HostToControllerPacket, PacketKind};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;

pub type Packet = heapless::Vec<u8, 257>;

// The mock is a singleton (like the driver it tests); state lives in statics
// so that test code can drive it while the controller itself is owned by
// `Ble::run`.
static TO_HOST: Channel<CriticalSectionRawMutex, (PacketKind, Packet), 16> = Channel::new();
static FROM_HOST_ACL: Channel<CriticalSectionRawMutex, Packet, 16> = Channel::new();
static ADVERTISING: AtomicBool = AtomicBool::new(false);

/// Inject a raw HCI event packet (controller -> host).
pub fn inject_event(bytes: &[u8]) {
    TO_HOST
        .try_send((PacketKind::Event, Packet::from_slice(bytes).unwrap()))
        .expect("mock event queue full");
}

/// Inject a raw ACL data packet (controller -> host).
pub fn inject_acl(bytes: &[u8]) {
    TO_HOST
        .try_send((PacketKind::AclData, Packet::from_slice(bytes).unwrap()))
        .expect("mock event queue full");
}

/// The next ACL data packet the host sent (raw, incl. the 4-byte header).
pub async fn host_acl() -> Packet {
    FROM_HOST_ACL.receive().await
}

/// Whether the host has advertising enabled (tracks LE Set Advertise Enable).
pub fn advertising() -> bool {
    ADVERTISING.load(Ordering::SeqCst)
}

pub struct MockController(());

#[derive(Debug)]
pub struct MockError;

impl std::fmt::Display for MockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "mock controller error")
    }
}

impl std::error::Error for MockError {}

// `ExternalController` surfaces packet-parse failures through the transport
// error type.
impl From<bt_hci::FromHciBytesError> for MockError {
    fn from(_: bt_hci::FromHciBytesError) -> Self {
        Self
    }
}

impl embedded_io::Error for MockError {
    fn kind(&self) -> embedded_io::ErrorKind {
        embedded_io::ErrorKind::Other
    }
}

impl embedded_io::ErrorType for MockController {
    type Error = MockError;
}

impl Default for MockController {
    fn default() -> Self {
        Self::new()
    }
}

impl MockController {
    pub fn new() -> Self {
        // Reset the singleton state from any previous instance
        while TO_HOST.try_receive().is_ok() {}
        while FROM_HOST_ACL.try_receive().is_ok() {}
        ADVERTISING.store(false, Ordering::SeqCst);

        Self(())
    }

    fn complete(&self, opcode: u16, params: &[u8]) {
        // Command Complete: [0x0E][len][num_hci_cmd_pkts=1][opcode: 2][status=0][params]
        let mut evt = Packet::new();
        evt.push(0x0e).unwrap();
        evt.push((4 + params.len()) as u8).unwrap();
        evt.push(1).unwrap();
        evt.extend_from_slice(&opcode.to_le_bytes()).unwrap();
        evt.push(0).unwrap();
        evt.extend_from_slice(params).unwrap();

        TO_HOST
            .try_send((PacketKind::Event, evt))
            .expect("mock event queue full");
    }

    fn handle_cmd(&self, cmd: &[u8]) {
        let opcode = u16::from_le_bytes([cmd[0], cmd[1]]);

        match opcode {
            // Read Local Version Information
            0x1001 => self.complete(opcode, &[0x0c, 0, 0, 0x0c, 0xff, 0xff, 0, 0]),
            // Read Local Supported Commands: claim support for everything
            0x1002 => self.complete(opcode, &[0xff; 64]),
            // Read Local Supported Features: LE supported, BR/EDR not supported
            0x1003 => self.complete(opcode, &[0, 0, 0, 0, 0x60, 0, 0, 0]),
            // Read Buffer Size (classic)
            0x1005 => self.complete(opcode, &[0xfb, 0, 0, 16, 0, 0, 0]),
            // Read BD_ADDR
            0x1009 => self.complete(opcode, &[0x01, 0x02, 0x03, 0x04, 0x05, 0xc0]),
            // Read Remote Version Information: an *async* command - ack it,
            // then deliver the Read Remote Version Information Complete event
            // (the host defers the app-level connect event until it arrives)
            0x041d => {
                self.complete(opcode, &[]);
                inject_event(&[0x0c, 8, 0, cmd[3], cmd[4], 0x0c, 0xff, 0xff, 0, 0]);
            }
            // LE Read Remote Features: async as above; deliver the LE Read
            // Remote Features Complete meta event
            0x2016 => {
                self.complete(opcode, &[]);
                inject_event(&[0x3e, 12, 0x04, 0, cmd[3], cmd[4], 0x01, 0, 0, 0, 0, 0, 0, 0]);
            }
            // LE Create Connection: async - ack it, then deliver the LE
            // Connection Complete meta event (role: master; the peer address
            // comes from the command parameters)
            0x200d => {
                self.complete(opcode, &[]);
                #[rustfmt::skip]
                inject_event(&[
                    0x3e, 19, 0x01,
                    0x00,       // status
                    0x01, 0x00, // handle 1
                    0x00,       // role: master
                    cmd[8],     // peer addr type
                    cmd[9], cmd[10], cmd[11], cmd[12], cmd[13], cmd[14],
                    0x28, 0x00, // conn interval
                    0x00, 0x00, // latency
                    0xf4, 0x01, // supervision timeout
                    0x00,       // master clock accuracy
                ]);
            }
            // LE Read Buffer Size
            0x2002 => self.complete(opcode, &[0xfb, 0, 8]),
            // LE Read Local Supported Features: plain 4.2 feature set
            0x2003 => self.complete(opcode, &[0x01, 0, 0, 0, 0, 0, 0, 0]),
            // LE Read Advertising Channel TX Power
            0x2007 => self.complete(opcode, &[0]),
            // LE Set Advertise Enable
            0x200a => {
                ADVERTISING.store(cmd[3] != 0, Ordering::SeqCst);
                self.complete(opcode, &[]);
            }
            // LE Read Filter Accept List Size
            0x200f => self.complete(opcode, &[8]),
            // LE Rand ("chosen by fair dice roll")
            0x2018 => self.complete(opcode, &[4, 4, 4, 4, 4, 4, 4, 4]),
            // LE Set Data Length: returns the connection handle
            0x2022 => self.complete(opcode, &[cmd[3], cmd[4]]),
            // LE Read Suggested Default Data Length
            0x2023 => self.complete(opcode, &[0x1b, 0, 0x48, 0x01]),
            // LE Read Resolving List Size
            0x202a => self.complete(opcode, &[8]),
            // LE Read Maximum Data Length
            0x202f => self.complete(opcode, &[0xfb, 0, 0x48, 0x08, 0xfb, 0, 0x48, 0x08]),
            // LE Read Buffer Size V2
            0x2060 => self.complete(opcode, &[0xfb, 0, 8, 0, 0, 0]),
            // Everything else (set-event-mask family, LE set-*, reset, ...):
            // status-only success
            _ => {
                log::debug!("mock: generic success for opcode {opcode:#06x}");
                self.complete(opcode, &[]);
            }
        }
    }
}

impl Transport for MockController {
    async fn read<'a>(&self, buf: &'a mut [u8]) -> Result<ControllerToHostPacket<'a>, Self::Error> {
        let (kind, packet) = TO_HOST.receive().await;
        buf[..packet.len()].copy_from_slice(&packet);

        ControllerToHostPacket::from_hci_bytes_with_kind(kind, &buf[..packet.len()])
            .map(|(packet, _)| packet)
            .map_err(|_| MockError)
    }

    async fn write<T: HostToControllerPacket>(&self, val: &T) -> Result<(), Self::Error> {
        let mut raw = [0; 260];
        let size = val.size();
        val.write_hci(&mut raw[..size]).map_err(|_| MockError)?;

        match T::KIND {
            PacketKind::Cmd => self.handle_cmd(&raw[..size]),
            PacketKind::AclData => {
                log::debug!("mock: ACL from host: {:02x?}", &raw[..size]);

                // Return the controller buffer credit immediately (Number Of
                // Completed Packets), or the host stalls after
                // `LE Read Buffer Size`-many ACL packets
                let handle = u16::from_le_bytes([raw[0], raw[1]]) & 0x0fff;
                let [h0, h1] = handle.to_le_bytes();
                inject_event(&[0x13, 5, 1, h0, h1, 1, 0]);

                FROM_HOST_ACL
                    .try_send(Packet::from_slice(&raw[..size]).unwrap())
                    .expect("mock ACL queue full");
            }
            kind => log::info!("mock: ignoring {kind:?} packet from host"),
        }

        Ok(())
    }
}
