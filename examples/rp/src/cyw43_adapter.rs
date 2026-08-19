//! **If you plan to run `nimble-rs` on a `cyw43` radio (Pico W, Pico 2 W), you need the
//! [`Cyw43Controller`] wrapper in this module** - the stock
//! `bt_hci::controller::ExternalController` over `cyw43::bluetooth::BtDriver` is not enough on
//! its own.
//!
//! `BtDriver` is a mere pair of channels: every HCI byte on the wire is moved by
//! `cyw43::Runner::run`, a *separate* future, conventionally spawned as its own task. The NimBLE
//! host, being a C stack, blocks the calling task until the controller acks an HCI command, and
//! `nimble-rs` spends that wait pumping the HCI bridge and then parking. For the duration of the
//! wait the executor belongs to the blocked caller, so a spawned runner is never polled: the
//! command never reaches the chip, the ack never arrives, and the host hangs on its first HCI
//! `Reset`.
//!
//! The remedy is to stop spawning the runner and make it reachable *through* the controller.
//! [`SharedRunner`] holds the pinned runner future and [`Cyw43Controller`] polls it while awaiting
//! any HCI operation, which makes it part of the bridge's I/O - so the parker's `WFE` is woken by
//! the runner's own DMA transfers. The runner still needs an arm of its own for the times when no
//! HCI operation is in flight; see `run` in the crate root for the whole assembly.
//!
//! Nothing here is `cyw43`-specific beyond the name: the same wrapper applies to any controller
//! whose transport is driven by a future other than the controller's own.

use core::cell::RefCell;
use core::convert::Infallible;
use core::future::{poll_fn, Future};
use core::pin::Pin;
use core::task::{Context, Poll};

use bt_hci::cmd::{AsyncCmd, SyncCmd};
use bt_hci::controller::{ControllerCmdAsync, ControllerCmdSync};
use bt_hci::data::{AclPacket, IsoPacket, SyncPacket};
use bt_hci::ControllerToHostPacket;
use embassy_futures::select::select;
use embassy_futures::select::Either::{First, Second};
use embedded_io::ErrorType;

/// Re-shape a never-returning future into one with a *nameable* `Infallible` output, so that it
/// can be type-erased as `dyn Future` (`cyw43::Runner::run` returns `!`, which cannot be named).
pub async fn never_ending<F>(fut: F) -> Infallible
where
    F: Future,
{
    fut.await;

    core::future::pending().await
}

/// The radio runner future, borrowed by everything that needs it polled.
pub struct SharedRunner<'a>(RefCell<Pin<&'a mut (dyn Future<Output = Infallible> + 'a)>>);

impl<'a> SharedRunner<'a> {
    /// Share the given (pinned) `cyw43` runner future.
    pub fn new(runner: Pin<&'a mut (dyn Future<Output = Infallible> + 'a)>) -> Self {
        Self(RefCell::new(runner))
    }

    /// Poll the runner once, unless it is already being polled further up the stack.
    pub fn poll(&self, cx: &mut Context<'_>) -> Poll<Infallible> {
        let Ok(mut runner) = self.0.try_borrow_mut() else {
            // Already being polled further up the stack, and that poll registers the waker.
            return Poll::Pending;
        };

        runner.as_mut().poll(cx)
    }
}

/// A `bt-hci` controller that drives the radio runner - i.e. the transport it is speaking through -
/// for as long as it is awaiting an HCI operation.
///
/// See the module docs for why this is mandatory rather than an optimization.
pub struct Cyw43Controller<'a, C> {
    ctl: C,
    runner: &'a SharedRunner<'a>,
}

impl<'a, C> Cyw43Controller<'a, C> {
    /// Create a new instance.
    pub const fn new(ctl: C, runner: &'a SharedRunner<'a>) -> Self {
        Self { ctl, runner }
    }

    /// Await `op`, polling the runner alongside it.
    async fn with<T>(&self, op: impl Future<Output = T>) -> T {
        match select(op, poll_fn(|cx| self.runner.poll(cx))).await {
            First(result) => result,
            Second(never) => match never {},
        }
    }
}

impl<C> ErrorType for Cyw43Controller<'_, C>
where
    C: ErrorType,
{
    type Error = C::Error;
}

impl<C> bt_hci::controller::Controller for Cyw43Controller<'_, C>
where
    C: bt_hci::controller::Controller,
{
    fn write_acl_data(&self, packet: &AclPacket) -> impl Future<Output = Result<(), Self::Error>> {
        self.with(self.ctl.write_acl_data(packet))
    }

    fn write_sync_data(
        &self,
        packet: &SyncPacket,
    ) -> impl Future<Output = Result<(), Self::Error>> {
        self.with(self.ctl.write_sync_data(packet))
    }

    fn write_iso_data(&self, packet: &IsoPacket) -> impl Future<Output = Result<(), Self::Error>> {
        self.with(self.ctl.write_iso_data(packet))
    }

    fn read<'a>(
        &self,
        buf: &'a mut [u8],
    ) -> impl Future<Output = Result<ControllerToHostPacket<'a>, Self::Error>> {
        self.with(self.ctl.read(buf))
    }
}

impl<C, Q> ControllerCmdSync<Q> for Cyw43Controller<'_, C>
where
    C: ControllerCmdSync<Q>,
    Q: SyncCmd + ?Sized,
{
    fn exec(
        &self,
        cmd: &Q,
    ) -> impl Future<Output = Result<Q::Return, bt_hci::cmd::Error<Self::Error>>> {
        self.with(self.ctl.exec(cmd))
    }
}

impl<C, Q> ControllerCmdAsync<Q> for Cyw43Controller<'_, C>
where
    C: ControllerCmdAsync<Q>,
    Q: AsyncCmd + ?Sized,
{
    fn exec(&self, cmd: &Q) -> impl Future<Output = Result<(), bt_hci::cmd::Error<Self::Error>>> {
        self.with(self.ctl.exec(cmd))
    }
}
