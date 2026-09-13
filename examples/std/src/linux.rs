//! A Linux HCI-socket transport (`HCI_CHANNEL_USER`) implementing
//! `bt_hci::transport::Transport`, for driving nimble-rs over a real adapter
//! or a BlueZ `btvirt` virtual controller.
//!
//! Adapted from the `bt-hci-linux` crate of the trouble workspace
//! (https://github.com/embassy-rs/trouble, MIT OR Apache-2.0), with the I/O
//! rebased from tokio's `AsyncFd` onto `async-io`. That matters beyond
//! executor taste: async-io's reactor runs on its own thread, so socket
//! readiness keeps flowing (and wakes parked waiters) even while the calling
//! thread sits inside nimble-rs' pump-while-pending HCI wait - whereas a
//! current-thread tokio runtime would be starved by that wait and time every
//! command out. It also makes the examples executor-agnostic (plain
//! `block_on` works).
//!
//! Requires the HCI device to be *down* and `CAP_NET_ADMIN`.

use core::convert::Infallible;
use core::mem;
use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

use async_io::Async;

use bt_hci::transport::{self, PacketToController, PacketToHost, WithIndicator};
use bt_hci::{PacketKind, ReadHciError};

const BTPROTO_HCI: libc::c_int = 1;
const HCI_CHANNEL_USER: libc::c_ushort = 1;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
#[allow(non_camel_case_types)]
struct sockaddr_hci {
    hci_family: libc::c_ushort,
    hci_dev: libc::c_ushort,
    hci_channel: libc::c_ushort,
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum Error {
    ReadHci(ReadHciError<Infallible>),
    Io(io::Error),
}

impl core::error::Error for Error {}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl embedded_io::Error for Error {
    fn kind(&self) -> embedded_io::ErrorKind {
        embedded_io::ErrorKind::Other
    }
}

// `ExternalController` surfaces packet-parse failures through the transport
// error type.
impl From<ReadHciError<Infallible>> for Error {
    fn from(e: ReadHciError<Infallible>) -> Self {
        Self::ReadHci(e)
    }
}

pub struct Transport {
    fd: Async<OwnedFd>,
}

impl Transport {
    // We use `libc` directly because `nix` makes it awkward to bind an
    // arbitrary address and `rustix` to set arbitrary sockopts.
    pub fn new(dev: u16) -> io::Result<Self> {
        let fd = unsafe {
            libc::socket(
                libc::AF_BLUETOOTH,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                BTPROTO_HCI,
            )
        };
        let fd = if fd < 0i32 {
            return Err(io::Error::last_os_error());
        } else {
            unsafe { OwnedFd::from_raw_fd(fd) }
        };

        let mut addr: sockaddr_hci = unsafe { mem::zeroed() };
        addr.hci_family = libc::AF_BLUETOOTH as u16;
        addr.hci_dev = dev;
        addr.hci_channel = HCI_CHANNEL_USER;
        if unsafe {
            libc::bind(
                fd.as_raw_fd(),
                (&raw const addr).cast(),
                mem::size_of::<sockaddr_hci>() as libc::socklen_t,
            )
        } < 0i32
        {
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            fd: Async::new(fd)?,
        })
    }
}

impl transport::Transport for Transport {
    async fn read<'a, P: PacketToHost<'a>>(&self, rx: &'a mut [u8]) -> Result<P, Self::Error> {
        // One HCI packet per socket read on the user channel. The wire form
        // (indicator byte + packet) lands in a scratch buffer; the packet is
        // then parsed into `rx`, which is what the caller gets a view of.
        let mut wire = vec![0u8; rx.len() + 1];
        let read = self
            .fd
            .read_with(|fd| {
                let ret =
                    unsafe { libc::read(fd.as_raw_fd(), wire.as_mut_ptr().cast(), wire.len()) };
                usize::try_from(ret).map_err(|_| io::Error::last_os_error())
            })
            .await
            .map_err(Error::Io)?;

        let mut data = wire
            .get(..read)
            .expect("more bytes read than the buffer holds");
        let kind = PacketKind::read(&mut data).map_err(Error::ReadHci)?;
        P::read_hci(kind, &mut data, rx).map_err(Error::ReadHci)
    }

    async fn write<T: PacketToController>(&self, val: &T) -> Result<(), Self::Error> {
        let mut buf = Vec::<u8>::new();
        WithIndicator::new(val).write_hci(&mut buf).unwrap();

        let written = self
            .fd
            .write_with(|fd| {
                let ret = unsafe { libc::write(fd.as_raw_fd(), buf.as_ptr().cast(), buf.len()) };
                usize::try_from(ret).map_err(|_| io::Error::last_os_error())
            })
            .await
            .map_err(Error::Io)?;

        assert!(
            written == buf.len(),
            "partial write of a whole HCI packet shouldn't happen"
        );
        Ok(())
    }
}

impl embedded_io::ErrorType for Transport {
    type Error = Error;
}
