use windows::{
    core::GUID,
    Win32::{Devices::Bluetooth::*, Foundation::*},
};

use std::fmt;
use std::mem;

use crate::util;

// TODO: Where is this from?? (bthdef.h?) This is different to HidD_GetHidGuid
// 00001124-0000-1000-8000-00805f9b34fb
const HID_SERVICE_CLASS_GUID: GUID = GUID::from_u128(0x00001124_0000_1000_8000_00805f9b34fb);

#[derive(Clone, Copy)]
pub struct Address([u8; 6]);

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let addr = self.0;

        write!(
            f,
            "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            addr[0], addr[1], addr[2], addr[3], addr[4], addr[5]
        )
    }
}

impl From<BLUETOOTH_ADDRESS> for Address {
    fn from(addr: BLUETOOTH_ADDRESS) -> Self {
        unsafe { Self(addr.Anonymous.rgBytes) }
    }
}

pub struct Radio {
    h_radio: HANDLE,
    h_find_radio: HANDLE,
}

impl Radio {
    // Returns `None` if there are no radios i.e. we don't have bluetooth
    fn find_first_radio() -> Option<Self> {
        let radio_params = BLUETOOTH_FIND_RADIO_PARAMS {
            dwSize: mem::size_of::<BLUETOOTH_FIND_RADIO_PARAMS>() as u32,
        };

        let mut h_radio = HANDLE::default();
        let h_find_radio = unsafe { HANDLE(BluetoothFindFirstRadio(&radio_params, &mut h_radio)) };

        if !h_find_radio.is_invalid() {
            Some(Self {
                h_radio,
                h_find_radio,
            })
        } else {
            None
        }
    }

    fn find_next_radio(mut self) -> Option<Self> {
        unsafe {
            if BluetoothFindNextRadio(self.h_find_radio.0, &mut self.h_radio).into() {
                Some(self)
            } else {
                None
            }
        }
    }

    pub fn address(&self) -> windows::core::Result<Address> {
        let mut radio_info = BLUETOOTH_RADIO_INFO {
            dwSize: mem::size_of::<BLUETOOTH_RADIO_INFO>() as u32,
            ..Default::default()
        };

        let res = unsafe { WIN32_ERROR(BluetoothGetRadioInfo(self.h_radio, &mut radio_info)) };

        if res == ERROR_SUCCESS {
            Ok(radio_info.address.into())
        } else {
            Err(res.to_hresult().into())
        }
    }
}

impl Drop for Radio {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.h_radio);
            BluetoothFindRadioClose(self.h_find_radio.0);
            self.h_find_radio = HANDLE::default();
        }
    }
}

pub struct Device {
    h_find_device: HANDLE,
    device_info: BLUETOOTH_DEVICE_INFO,
    name: String,
    radio: Radio,
}

impl Device {
    fn find_first_device(radio: Radio, new_scan: bool) -> Option<Self> {
        let search_params = BLUETOOTH_DEVICE_SEARCH_PARAMS {
            dwSize: mem::size_of::<BLUETOOTH_DEVICE_SEARCH_PARAMS>() as u32,
            // The `into`s are to convert to windows BOOLs
            fReturnAuthenticated: true.into(),
            fReturnRemembered: true.into(),
            // XXX: This filter doesn't work?
            fReturnConnected: true.into(),
            fReturnUnknown: true.into(),
            fIssueInquiry: new_scan.into(),
            cTimeoutMultiplier: 2,
            hRadio: radio.h_radio,
        };

        let mut device_info = BLUETOOTH_DEVICE_INFO {
            dwSize: mem::size_of::<BLUETOOTH_DEVICE_INFO>() as u32,
            ..Default::default()
        };

        let h_find_device =
            unsafe { HANDLE(BluetoothFindFirstDevice(&search_params, &mut device_info)) };

        if !h_find_device.is_invalid() {
            let name = unsafe { util::wstring_to_utf8(&device_info.szName) };

            Some(Self {
                h_find_device,
                device_info,
                name,
                radio,
            })
        } else {
            None
        }
    }

    fn find_next_device(mut self) -> Option<Self> {
        unsafe {
            if BluetoothFindNextDevice(self.h_find_device.0, &mut self.device_info).into() {
                self.name = util::wstring_to_utf8(&self.device_info.szName);
                Some(self)
            } else {
                None
            }
        }
    }

    pub fn is_authenticated(&self) -> bool {
        self.device_info.fAuthenticated.into()
    }

    pub fn is_connected(&self) -> bool {
        self.device_info.fConnected.into()
    }

    pub fn is_remembered(&self) -> bool {
        self.device_info.fRemembered.into()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn address(&self) -> Address {
        self.device_info.Address.into()
    }

    // pub fn radio_address(&self) -> Address {
    //     self.radio.address()
    // }

    // // Removes device if it is remembered, but not connected
    // // Returns true if a device was actually forgotten about
    // pub fn forget_device(&mut self, radio: &BluetoothRadio) -> bool {
    //     if self.is_remembered() && !self.is_connected() {
    //         self.remove_device(radio).expect("Error removing device");

    //         true
    //     } else {
    //         false
    //     }
    // }

    pub fn enable(&mut self) -> windows::core::Result<()> {
        unsafe {
            let res = WIN32_ERROR(BluetoothSetServiceState(
                self.radio.h_radio,
                &self.device_info,
                &HID_SERVICE_CLASS_GUID,
                BLUETOOTH_SERVICE_ENABLE,
            ));

            if res.is_err() {
                Err(windows::core::Error::from(res))
            } else {
                Ok(())
            }
        }
    }

    pub fn disable(&mut self) -> windows::core::Result<()> {
        unsafe {
            let res = WIN32_ERROR(BluetoothSetServiceState(
                self.radio.h_radio,
                &self.device_info,
                &HID_SERVICE_CLASS_GUID,
                BLUETOOTH_SERVICE_DISABLE,
            ));

            if res.is_err() {
                Err(res.into())
            } else {
                Ok(())
            }
        }
    }

    pub fn remove(&mut self) {
        unsafe {
            // This will error if the address isn't found
            // Since we can only remove devices we know about, it's fine™
            BluetoothRemoveDevice(&self.device_info.Address);
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe {
            BluetoothFindDeviceClose(self.h_find_device.0);
        }
        self.h_find_device = HANDLE::default();
    }
}

struct Scanner {
    current_radio: Option<Radio>,
    current_device: Option<Device>,
    should_scan: bool,
}

impl Scanner {
    pub fn new(should_scan: bool) -> Self {
        Self {
            current_radio: Radio::find_first_radio(),
            current_device: None,
            should_scan,
        }
    }

    // TODO: Traverse multiple radios?
    fn next(&mut self) -> Option<&mut Device> {
        self.current_device = match self.current_device.take() {
            Some(device) => device.find_next_device(),
            None => Device::find_first_device(self.current_radio.take()?, self.should_scan),
        };

        self.current_device.as_mut()
    }
}

pub fn iter_devices<F>(should_scan: bool, mut f: F)
where
    F: FnMut(&mut Device),
{
    let mut scanner = Scanner::new(should_scan);
    while let Some(device) = scanner.next() {
        // Sometimes the device's name is empty, so filter it out
        if !device.name().is_empty() {
            f(device);
        }
    }
}
