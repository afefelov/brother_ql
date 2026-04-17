//! USB connection support for Brother QL printers
use std::time::Duration;

#[cfg(not(target_os = "windows"))]
use rusb::{Context, Device, DeviceHandle, UsbContext};
#[cfg(target_os = "windows")]
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
};
use tracing::debug;
#[cfg(target_os = "windows")]
use winreg::{RegKey, enums::HKEY_LOCAL_MACHINE};

use crate::{
    error::{PrintError, UsbError},
    printer::PrinterModel,
    printjob::PrintJob,
    status::{Phase, StatusType},
};

use super::{PrinterConnection, printer_connection::sealed::ConnectionImpl};

const BROTHER_USB_VENDOR_ID: u16 = 0x04f9;
#[cfg(target_os = "windows")]
const WINDOWS_USB_MONITOR_PORTS_KEY: &str =
    r"SYSTEM\CurrentControlSet\Control\Print\Monitors\USB Monitor\Ports";
#[cfg(target_os = "windows")]
const WINDOWS_BROTHER_DEVICE_PREFIX: &str = "USB\\VID_04F9&PID_";

#[cfg(not(target_os = "windows"))]
type UsbHandle = DeviceHandle<Context>;
#[cfg(target_os = "windows")]
type UsbHandle = File;

#[cfg(target_os = "windows")]
#[derive(Debug, Clone)]
struct WindowsUsbPrinter {
    device_id: String,
    device_path: String,
}

/// USB connection parameters for a Brother QL printer
///
/// Contains all necessary USB parameters to establish a connection,
/// including vendor/product IDs, endpoints, and timeout settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbConnectionInfo {
    /// USB vendor ID (typically 0x04f9 for Brother Industries, Ltd)
    pub(crate) vendor_id: u16,
    /// USB product ID (specific to each printer model)
    pub(crate) product_id: u16,
    /// USB interface number (typically 0 for printers)
    pub(crate) interface: u8,
    /// USB endpoint address for writing data to the printer (OUT endpoint)
    pub(crate) endpoint_out: u8,
    /// USB endpoint address for reading data from the printer (IN endpoint)
    pub(crate) endpoint_in: u8,
    /// Timeout for USB operations
    pub(crate) timeout: Duration,
}

impl UsbConnectionInfo {
    /// Create connection info from a printer model.
    #[must_use]
    pub const fn from_model(model: PrinterModel) -> Self {
        Self {
            vendor_id: BROTHER_USB_VENDOR_ID,
            product_id: model.product_id(),
            interface: 0,
            endpoint_out: 0x02,
            endpoint_in: 0x81,
            timeout: Duration::from_millis(5000),
        }
    }

    /// Find a connected printer and return its connection info.
    ///
    /// # Errors
    /// Returns an error if USB enumeration fails.
    pub fn discover() -> Result<Option<Self>, UsbError> {
        #[cfg(target_os = "windows")]
        {
            for printer in enum_windows_usb_printers()? {
                if let Some(product_id) = product_id_from_device_id(&printer.device_id)
                    && let Some(model) = PrinterModel::from_product_id(product_id)
                {
                    return Ok(Some(Self::from_model(model)));
                }
            }
            return Ok(None);
        }

        #[cfg(not(target_os = "windows"))]
        {
            let context = Context::new()?;
            let devices = context.devices()?;

            for device in devices.iter() {
                let descriptor = device.device_descriptor()?;
                if descriptor.vendor_id() != BROTHER_USB_VENDOR_ID {
                    continue;
                }

                if let Some(model) = PrinterModel::from_product_id(descriptor.product_id()) {
                    return Ok(Some(Self::from_model(model)));
                }
            }

            Ok(None)
        }
    }
}

/// Active USB connection to a Brother QL printer.
#[cfg_attr(target_os = "windows", allow(dead_code))]
pub struct UsbConnection {
    handle: UsbHandle,
    interface: u8,
    timeout: Duration,
    endpoint_out: u8,
    endpoint_in: u8,
}

impl UsbConnection {
    /// Open a USB connection to a Brother QL printer.
    ///
    /// # Errors
    /// Returns an error if the printer cannot be opened for the current platform.
    pub fn open(info: UsbConnectionInfo) -> Result<Self, UsbError> {
        debug!("Opening USB connection to the printer...");

        #[cfg(target_os = "windows")]
        {
            let device_path = find_windows_device_path(info.vendor_id, info.product_id)?;
            let handle = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&device_path)?;

            debug!("Successfully established USB connection via Windows USB monitor");
            return Ok(Self {
                handle,
                interface: info.interface,
                timeout: info.timeout,
                endpoint_out: info.endpoint_out,
                endpoint_in: info.endpoint_in,
            });
        }

        #[cfg(not(target_os = "windows"))]
        {
            let context = Context::new()?;
            let device = Self::find_device(&context, info.vendor_id, info.product_id)?;
            let handle = device.open()?;

            match handle.set_auto_detach_kernel_driver(true) {
                Ok(()) => {}
                Err(rusb::Error::NotSupported) => {
                    debug!("Automatic kernel-driver detachment is not supported on this platform");
                }
                Err(e) => return Err(e.into()),
            }
            match handle.kernel_driver_active(info.interface) {
                Ok(true) => match handle.detach_kernel_driver(info.interface) {
                    Ok(()) => {}
                    Err(rusb::Error::NotSupported) => {
                        debug!("Kernel-driver detachment is not supported on this platform");
                    }
                    Err(e) => return Err(e.into()),
                },
                Ok(false) => {}
                Err(rusb::Error::NotSupported) => {
                    debug!("Kernel-driver status detection is not supported on this platform");
                }
                Err(e) => return Err(e.into()),
            }
            handle.set_active_configuration(1)?;
            handle.claim_interface(info.interface)?;

            if let Err(e) = handle.set_alternate_setting(info.interface, 0) {
                let _ = handle.release_interface(info.interface);
                return Err(e.into());
            }

            debug!("Successfully established USB connection!");
            Ok(Self {
                handle,
                interface: info.interface,
                timeout: info.timeout,
                endpoint_out: info.endpoint_out,
                endpoint_in: info.endpoint_in,
            })
        }
    }

    /// Find a USB device with the specified vendor and product IDs.
    #[cfg(not(target_os = "windows"))]
    fn find_device(
        context: &Context,
        vendor_id: u16,
        product_id: u16,
    ) -> Result<Device<Context>, UsbError> {
        let devices = context.devices()?;

        for device in devices.iter() {
            let descriptor = device.device_descriptor()?;
            if descriptor.vendor_id() == vendor_id && descriptor.product_id() == product_id {
                return Ok(device);
            }
        }

        Err(UsbError::DeviceNotFound {
            vendor_id,
            product_id,
        })
    }
}

#[cfg(target_os = "windows")]
fn enum_windows_usb_printers() -> Result<Vec<WindowsUsbPrinter>, UsbError> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let ports_key = hklm.open_subkey(WINDOWS_USB_MONITOR_PORTS_KEY)?;
    let mut printers = Vec::new();

    for port_name in ports_key.enum_keys().flatten() {
        let port_key = match ports_key.open_subkey(&port_name) {
            Ok(key) => key,
            Err(_) => continue,
        };
        let device_id: String = match port_key.get_value("Device Id") {
            Ok(value) => value,
            Err(_) => continue,
        };
        if !device_id
            .to_ascii_uppercase()
            .starts_with(WINDOWS_BROTHER_DEVICE_PREFIX)
        {
            continue;
        }
        let device_path: String = match port_key.get_value("Device Path") {
            Ok(value) => value,
            Err(_) => continue,
        };
        printers.push(WindowsUsbPrinter {
            device_id,
            device_path,
        });
    }

    Ok(printers)
}

#[cfg(target_os = "windows")]
fn product_id_from_device_id(device_id: &str) -> Option<u16> {
    let normalized = device_id.to_ascii_uppercase();
    let suffix = normalized.strip_prefix(WINDOWS_BROTHER_DEVICE_PREFIX)?;
    let product_id = suffix.get(..4)?;
    u16::from_str_radix(product_id, 16).ok()
}

#[cfg(target_os = "windows")]
fn find_windows_device_path(vendor_id: u16, product_id: u16) -> Result<String, UsbError> {
    for printer in enum_windows_usb_printers()? {
        if product_id_from_device_id(&printer.device_id) == Some(product_id)
            && printer
                .device_id
                .to_ascii_uppercase()
                .starts_with(&format!("USB\\VID_{vendor_id:04X}&PID_{product_id:04X}"))
        {
            return Ok(printer.device_path);
        }
    }

    Err(UsbError::DeviceNotFound {
        vendor_id,
        product_id,
    })
}

impl PrinterConnection for UsbConnection {
    #[cfg(target_os = "windows")]
    fn print(&mut self, job: PrintJob) -> Result<(), PrintError<Self::Error>> {
        let status = self
            .get_status()
            .map_err(PrintError::err_source_mapper(0))?;
        <Self as ConnectionImpl>::validate_status(
            &status,
            job.media,
            StatusType::StatusRequestReply,
            Phase::Receiving,
        )
        .map_err(|e| PrintError::with_page(e, 0))?;

        let compiled = job.compile();
        self.write(&compiled)
            .map_err(|e| PrintError::with_page(e, 1))?;

        Ok(())
    }
}

impl ConnectionImpl for UsbConnection {
    type Error = UsbError;

    fn write(&mut self, data: &[u8]) -> Result<(), Self::Error> {
        #[cfg(target_os = "windows")]
        {
            self.handle.write_all(data)?;
            self.handle.flush()?;
            Ok(())
        }

        #[cfg(not(target_os = "windows"))]
        {
            let bytes_written = self
                .handle
                .write_bulk(self.endpoint_out, data, self.timeout)?;
            if bytes_written != data.len() {
                return Err(UsbError::IncompleteWrite);
            }
            Ok(())
        }
    }

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        #[cfg(target_os = "windows")]
        {
            Ok(self.handle.read(buffer)?)
        }

        #[cfg(not(target_os = "windows"))]
        {
            let bytes_read = self
                .handle
                .read_bulk(self.endpoint_in, buffer, self.timeout)?;
            Ok(bytes_read)
        }
    }
}

impl Drop for UsbConnection {
    fn drop(&mut self) {
        #[cfg(not(target_os = "windows"))]
        {
            let _ = self.handle.release_interface(self.interface);
        }
    }
}
