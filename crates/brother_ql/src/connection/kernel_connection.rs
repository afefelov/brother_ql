//! Kernel device connection support for Brother QL printers
use std::path::Path;

#[cfg(target_os = "linux")]
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::fd::AsFd,
};

#[cfg(target_os = "linux")]
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
#[cfg(target_os = "linux")]
use tracing::debug;

use super::{PrinterConnection, printer_connection::sealed::ConnectionImpl};
use crate::error::KernelError;

/// Kernel connection to a Brother QL printer
///
/// Uses the Linux kernel USB printer driver for communication.
/// Opens the device file (typically `/dev/usb/lp0`) for reading and writing.
///
/// Implements [`PrinterConnection`] trait for high-level printing operations.
///
/// # Platform Support
///
/// Linux only. On other platforms, [`open`](Self::open) returns
/// [`KernelError::UnsupportedPlatform`].
pub struct KernelConnection {
    #[cfg(target_os = "linux")]
    handle: File,
}

impl KernelConnection {
    /// Open a kernel connection to a Brother QL printer
    ///
    /// Opens the specified device file for bidirectional communication.
    /// Common device paths are `/dev/usb/lp0`, `/dev/usb/lp1`, etc.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The device file doesn't exist
    /// - Insufficient permissions to access the device
    /// - The device is already in use by another process
    /// - The current platform does not support kernel device connections
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use brother_ql::connection::KernelConnection;
    /// # fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let connection = KernelConnection::open("/dev/usb/lp0")?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn open<P>(path: P) -> Result<Self, KernelError>
    where
        P: AsRef<Path>,
    {
        #[cfg(target_os = "linux")]
        {
            debug!(path = %path.as_ref().display(), "Opening kernel connection to the printer...");
            let handle = OpenOptions::new().read(true).write(true).open(path)?;

            debug!("Successfully opened kernel device!");
            Ok(Self { handle })
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = path.as_ref();
            Err(KernelError::UnsupportedPlatform)
        }
    }
}

impl PrinterConnection for KernelConnection {}

impl ConnectionImpl for KernelConnection {
    type Error = KernelError;

    fn write(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        #[cfg(target_os = "linux")]
        {
            let bytes_written = self.handle.write(data)?;
            if bytes_written != data.len() {
                return Err(KernelError::IncompleteWrite);
            }
            Ok(())
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = data;
            Err(KernelError::UnsupportedPlatform)
        }
    }

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        #[cfg(target_os = "linux")]
        {
            let mut pollfds = [PollFd::new(self.handle.as_fd(), PollFlags::POLLIN)];
            let nready = poll(&mut pollfds, PollTimeout::ZERO).unwrap_or(0);
            if nready == 0 {
                return Ok(0);
            }
            let bytes_read = self.handle.read(buffer)?;
            Ok(bytes_read)
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = buffer;
            Err(KernelError::UnsupportedPlatform)
        }
    }
}
