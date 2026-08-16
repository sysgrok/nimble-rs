//! Shared support code for the std examples: the mock controller and the
//! Linux HCI-socket transport.

#[cfg(target_os = "linux")]
pub mod linux;
pub mod mock;
