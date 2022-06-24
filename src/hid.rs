use windows::{
    core::GUID,
    Win32::{
        Devices::{DeviceAndDriverInstallation::*, HumanInterfaceDevice::*},
        Foundation::*,
        Storage::FileSystem::*,
        System::{Threading::*, IO::*},
    },
};

use arrayvec::ArrayVec;
use thiserror::Error;

use std::ffi::CString;
use std::io;
use std::mem::{self, MaybeUninit};
use std::ptr;
use std::time::Duration;

use crate::util;

// TODO: Add SAFETY comments
// TODO: Box<str>?
// TODO: io::Error::last_os_error()

const WIIMOTE_READ_TIMEOUT: Duration = Duration::from_millis(200);
const WIIMOTE_WRITE_TIMEOUT: Duration = Duration::from_millis(1000);

pub const INPUT_REPORT: u8 = 0xa1;
pub const OUTPUT_REPORT: u8 = 0xa2;

// NOTE: This size includes the HID header
pub const MAX_REPORT_LENGTH: usize = 23;

pub type Report = ArrayVec<u8, MAX_REPORT_LENGTH>;

#[derive(Debug, PartialEq, Error)]
pub enum Error {
    #[error("A timeout occurred on writing to the device")]
    WriteTimedOut,
    #[error("The device is not connected")]
    // XXX: Check for this in From impl
    NotConnected,
    #[error("A Windows error occured: {0:?}")]
    Windows(#[from] windows::core::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

struct Overlapped(OVERLAPPED);

impl Overlapped {
    pub fn new() -> io::Result<Self> {
        Ok(Self(OVERLAPPED {
            // XXX: Change manual reset to false?
            hEvent: unsafe { CreateEventA(ptr::null_mut(), true, false, None)? },
            ..Default::default()
        }))
    }

    /// Returns a mutable pointer to the internal [`OVERLAPPED`] structure.
    pub fn raw(&self) -> *mut OVERLAPPED {
        &self.0 as *const _ as *mut _
    }

    fn event(&self) -> HANDLE {
        self.0.hEvent
    }

    /// Reset the event to the un-signalled state.
    pub fn reset_event(&mut self) {
        unsafe {
            ResetEvent(self.event());
        }
    }

    /// Block for at most `timeout` while the operation on `handle` completes.
    ///
    /// If the operation completes within the timeout specified, this will
    /// return an [`Ok(Some)`] containing the number of bytes transferred. If
    /// the operation times out, [`Ok(None)`] will be returned instead. If the
    /// operation fails, [`Err`] will be returned.
    ///
    /// If the operation has already completed, this will return the
    /// [`Ok(Some)`] immediately without blocking.
    fn get_overlapped_result_ex(
        &mut self,
        handle: HANDLE,
        timeout: Duration,
    ) -> windows::core::Result<Option<usize>> {
        let mut bytes_transferred: u32 = 0;
        let res = unsafe {
            GetOverlappedResultEx(
                handle,
                self.raw(),
                &mut bytes_transferred,
                timeout.as_millis() as u32,
                false,
            )
        };

        match res.ok() {
            Ok(_) => Ok(Some(bytes_transferred as usize)),
            Err(e) => {
                if e.code() == WAIT_TIMEOUT.to_hresult() {
                    Ok(None)
                } else {
                    Err(e)
                }
            }
        }
    }
}

impl Drop for Overlapped {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.event());
        }
    }
}

unsafe impl Send for Overlapped {}
unsafe impl Sync for Overlapped {}

pub struct Device {
    path: String,
    read_ol: Overlapped,
    write_ol: Overlapped,
    handle: HANDLE,
}

impl Device {
    pub fn open(path: &str) -> io::Result<Self> {
        // Open a read/write handle to our device
        let handle = unsafe {
            CreateFileA(
                path,
                FILE_GENERIC_READ | FILE_GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )?
        };

        Ok(Self {
            path: path.to_string(),
            read_ol: Overlapped::new()?,
            write_ol: Overlapped::new()?,
            handle,
        })
    }

    pub fn read(&mut self) -> Result<Report> {
        // SAFETY: The buffer is a `MaybeUninit` array so that it may change
        // while the read operation is ongoing. We zero the buffer instead of
        // leaving it uninitialized so that any bytes that aren't changed by the
        // read operation will still be valid (zero) when we use the initalized
        // buffer later.
        let mut buf: [MaybeUninit<u8>; MAX_REPORT_LENGTH] =
            unsafe { MaybeUninit::zeroed().assume_init() };
        // Add data report indicator byte
        buf[0] = MaybeUninit::new(INPUT_REPORT);

        // Start the read operation
        let res: Result<()> = {
            // Leave space for data report indicator byte
            let buf = &mut buf[1..];

            self.read_ol.reset_event();
            let read_res = unsafe {
                ReadFile(
                    self.handle,
                    buf.as_mut_ptr().cast(),
                    buf.len() as u32,
                    ptr::null_mut(),
                    self.read_ol.raw(),
                )
            };

            let mut res = read_res.ok();
            if let Err(e) = &res {
                if e.code() == ERROR_IO_PENDING.to_hresult() {
                    res = Ok(());
                }
            }

            res.map_err(Error::Windows)
        };

        // Wait until the read operation completes/times out
        let res: Result<usize> = res.and_then(|_| {
            let bytes_read = self
                .read_ol
                .get_overlapped_result_ex(self.handle, WIIMOTE_READ_TIMEOUT)?
                // If the read times out, it isn't an error
                .unwrap_or(0);
            Ok(bytes_read)
        });

        let bytes_read = match res {
            Ok(bytes_read) => bytes_read,
            Err(e) => {
                // If there were any errors, cancel the pending operation
                self.cancel_io();
                return Err(e);
            }
        };

        // FIXME: This is a workaround for `assume_init_array` being unstable
        // SAFETY: The read operation will have completed by this point, so the
        // values of the bytes in the buffer will be fixed. Therefore the buffer
        // is initialized and we can transmute to the initialized type.
        let buf = unsafe { mem::transmute::<_, [u8; MAX_REPORT_LENGTH]>(buf) };

        let mut report = Report::from(buf);
        if bytes_read > 0 {
            // TODO: Actually figure out the report size
            // The length of the full report includes the data report indicator byte
            report.truncate(bytes_read + 1);
        } else {
            // Return an empty report if the read timed out
            report.truncate(0);
        }

        Ok(report)
    }

    // XXX: If we write do we need to cancel the current read?
    // TODO: Change slice to Report parameter?
    pub fn write(&mut self, buf: &[u8]) -> Result<usize> {
        // Start the write operation
        let res: Result<()> = {
            // Ignore the data report indicator byte
            let buf = &buf[1..];

            self.write_ol.reset_event();
            let write_res = unsafe {
                WriteFile(
                    self.handle,
                    buf.as_ptr().cast(),
                    buf.len() as u32,
                    ptr::null_mut(),
                    self.write_ol.raw(),
                )
            };

            let mut res = write_res.ok();
            if let Err(e) = &res {
                if e.code() == ERROR_IO_PENDING.to_hresult() {
                    res = Ok(());
                }
            }

            res.map_err(Error::Windows)
        };

        // Wait until the write operation completes/times out
        let res: Result<usize> = res.and_then(|_| {
            match self
                .write_ol
                .get_overlapped_result_ex(self.handle, WIIMOTE_WRITE_TIMEOUT)?
            {
                Some(bytes_written) => Ok(bytes_written),
                None => Err(Error::WriteTimedOut),
            }
        });

        // If there were any errors, cancel the pending operation
        if res.is_err() {
            self.cancel_io();
        }

        res
    }

    // NOTE: This will only cancel IO operations issued by the calling thread
    fn cancel_io(&mut self) {
        unsafe {
            CancelIo(self.handle);
        }
    }

    fn get_attributes(&self) -> Option<(u16, u16)> {
        let mut attrib = HIDD_ATTRIBUTES {
            Size: mem::size_of::<HIDD_ATTRIBUTES>() as u32,
            ..Default::default()
        };

        unsafe {
            // FIXME: Refactor if and when BOOLEAN is nicer
            if HidD_GetAttributes(self.handle, &mut attrib).0 != 0 {
                Some((attrib.VendorID, attrib.ProductID))
            } else {
                None
            }
        }
    }

    fn get_product_string(&self) -> Option<String> {
        // FIXME: Default::default()
        let mut buf: [u16; 128] = [0; 128];

        unsafe {
            // FIXME: Refactor if and when BOOLEAN is nicer
            if HidD_GetProductString(self.handle, buf.as_mut_ptr().cast(), buf.len() as u32).0 != 0
            {
                Some(util::wstring_to_utf8(&buf))
            } else {
                None
            }
        }
    }

    fn get_info(&self) -> Option<DeviceInfo> {
        self.get_attributes()
            .zip(self.get_product_string())
            .map(|((vid, pid), ps)| DeviceInfo {
                path: self.path.clone(),
                vendor_id: vid,
                product_id: pid,
                product_string: ps,
            })
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
        self.handle = HANDLE::default();
    }
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    // TODO: DevicePath wrapper type?
    pub path: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub product_string: String,
}

impl DeviceInfo {
    pub fn is_wiimote(&self) -> bool {
        (self.vendor_id == 0x057e && (self.product_id == 0x0306 || self.product_id == 0x0330))
            // TODO: Is this needed?
            || util::is_valid_device_name(&self.product_string)
    }
}

pub struct DeviceEnumerator {
    /// The GUID for the HID class.
    guid: GUID,
    /// A handle to the device information set.
    /// The device information set includes all the devices that are part of the
    /// HID class.
    h_dev_info: HDEVINFO,
}

impl DeviceEnumerator {
    pub fn new() -> Self {
        let guid = unsafe {
            let mut guid = MaybeUninit::<GUID>::uninit();
            HidD_GetHidGuid(guid.as_mut_ptr());
            guid.assume_init()
        };

        let h_dev_info = unsafe {
            let flags = DIGCF_DEVICEINTERFACE | DIGCF_PRESENT;
            SetupDiGetClassDevsA(&guid, None, None, flags).unwrap()
        };

        Self { guid, h_dev_info }
    }

    pub fn devices(&self) -> impl Iterator<Item = DeviceInfo> + '_ {
        DeviceEnumeration {
            index: 0,
            enumerator: self,
        }
    }
}

impl Drop for DeviceEnumerator {
    fn drop(&mut self) {
        unsafe {
            SetupDiDestroyDeviceInfoList(self.h_dev_info);
        }
    }
}

struct DeviceEnumeration<'a> {
    index: u32,
    enumerator: &'a DeviceEnumerator,
}

impl<'a> DeviceEnumeration<'a> {
    fn next_interface(&mut self) -> Option<SP_DEVICE_INTERFACE_DATA> {
        let mut device_interface_data = SP_DEVICE_INTERFACE_DATA {
            cbSize: mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..Default::default()
        };

        // Get interface data for a single device
        let res = unsafe {
            SetupDiEnumDeviceInterfaces(
                self.enumerator.h_dev_info,
                ptr::null_mut(),
                &self.enumerator.guid,
                self.index,
                &mut device_interface_data,
            )
        };

        if res.into() {
            // Increment the index for the next device
            self.index += 1;
            Some(device_interface_data)
        } else {
            None
        }
    }

    fn get_path(&self, device_interface_data: SP_DEVICE_INTERFACE_DATA) -> Option<String> {
        unsafe {
            let mut buf_size: u32 = 0;

            // Get the buffer size for the device detail struct
            SetupDiGetDeviceInterfaceDetailA(
                self.enumerator.h_dev_info,
                &device_interface_data,
                ptr::null_mut(),
                0,
                &mut buf_size,
                ptr::null_mut(),
            );

            // Allocate the buffer
            let mut detail_struct_buf = vec![0u8; buf_size as usize];
            ptr::write(
                detail_struct_buf.as_mut_ptr().cast(),
                SP_DEVICE_INTERFACE_DETAIL_DATA_A {
                    cbSize: mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_A>() as u32,
                    ..Default::default()
                },
            );

            // Populate the detail struct
            let res = SetupDiGetDeviceInterfaceDetailA(
                self.enumerator.h_dev_info,
                &device_interface_data,
                detail_struct_buf.as_mut_ptr().cast(),
                buf_size,
                ptr::null_mut(),
                ptr::null_mut(),
            );

            // TODO: Does cbSize tell us the length of the variable-length array?

            if res.into() {
                // TODO: memoffset crate?
                // let detail_struct_ptr =
                //     detail_struct_buf.as_ptr() as *const SP_DEVICE_INTERFACE_DETAIL_DATA_A;

                // let device_path_ptr = ptr::addr_of!((*detail_struct_ptr).DevicePath) as *const u8;
                // let device_path_end_ptr = detail_struct_buf.as_ptr_range().end;
                // let device_path_len = device_path_end_ptr.sub_ptr(device_path_ptr);

                // Slice to the DevicePath variable length array in the detail struct
                let device_path_slice = &detail_struct_buf[mem::size_of::<u32>()..];

                // We have to take the null byte off the end to construct a `CString`
                Some(
                    CString::new(&device_path_slice[..device_path_slice.len() - 1])
                        .unwrap()
                        .to_string_lossy()
                        .to_string(),
                )
            } else {
                None
            }
        }
    }
}

impl<'a> Iterator for DeviceEnumeration<'a> {
    type Item = DeviceInfo;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(device_interface_data) = self.next_interface() {
            let device_info = self
                .get_path(device_interface_data)
                .and_then(|path| Device::open(&path).ok())
                .and_then(|device| device.get_info());

            if device_info.is_none() {
                continue;
            }

            return device_info;
        }

        None
    }
}

// pub fn iter_devices() -> impl Iterator<Item = DeviceInfo> + '_ {
//     let device_enumerator = DeviceEnumerator::new();
//     device_enumerator.devices()
// }
