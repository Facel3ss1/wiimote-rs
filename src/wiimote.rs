use bitflags::bitflags;
use crossbeam_channel::{Receiver, Sender};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::hid::{self, OUTPUT_REPORT};

const RUMBLE_ON_CONNECT: bool = true;
const RUMBLE_DURATION: Duration = Duration::from_millis(250);

// TODO: Error enum for read/write/prepare errors

#[repr(u8)]
pub enum OutputReportID {
    // Rumble = 0x10,
    Led = 0x11,
    ReportMode = 0x12,
    RequestStatus = 0x15,
}

impl From<OutputReportID> for u8 {
    fn from(val: OutputReportID) -> Self {
        val as u8
    }
}

#[repr(u8)]
pub enum InputReportID {
    // Status = 0x20,
    // Ack = 0x22,
    CoreButtons = 0x30,
}

impl From<InputReportID> for u8 {
    fn from(val: InputReportID) -> Self {
        val as u8
    }
}

bitflags! {
    struct Led: u8 {
        const LED_1 = 0x10;
        const LED_2 = 0x20;
        const LED_3 = 0x40;
        const LED_4 = 0x80;
    }
}

impl Led {
    // NOTE: This is zero indexed
    pub fn player(p: usize) -> Self {
        match p {
            0 => Led::LED_1,
            1 => Led::LED_2,
            2 => Led::LED_3,
            3 => Led::LED_4,
            4 => !Led::LED_1,
            5 => !Led::LED_2,
            6 => !Led::LED_3,
            7 => !Led::LED_4,
            _ => Led::all(),
        }
    }
}

pub struct WiimotePollThread {
    is_connected: Arc<AtomicBool>,
    thread_handle: Option<thread::JoinHandle<()>>,
}

// XXX: Rename to WiimotePollThread or something?
impl WiimotePollThread {
    // TODO: Take in a device path and return a result if isn't a valid wiimote?
    pub fn new(
        hid_device: hid::Device,
        read_tx: Sender<hid::Report>,
        write_rx: Receiver<hid::Report>,
        player_num: usize,
    ) -> Self {
        let mut wiimote_thread = Self {
            is_connected: Arc::new(AtomicBool::new(false)),
            thread_handle: None,
        };

        wiimote_thread.start_thread(hid_device, read_tx, write_rx, player_num);

        wiimote_thread
    }

    fn start_thread(
        &mut self,
        hid_device: hid::Device,
        read_tx: Sender<hid::Report>,
        write_rx: Receiver<hid::Report>,
        player_num: usize,
    ) {
        if self.is_connected.load(Ordering::SeqCst) {
            return;
        }
        self.is_connected.store(true, Ordering::SeqCst);

        let is_connected = Arc::clone(&self.is_connected);
        let func = move || {
            if let Err(e) =
                Self::io_thread(&is_connected, hid_device, &read_tx, &write_rx, player_num)
            {
                println!("[Wiimote] Disconnecting Wiimote due to error: {e}");
            }

            is_connected.store(false, Ordering::SeqCst);
            println!("[Wiimote] P{} Thread stopped", player_num + 1);
            // `hid_device`, `read_tx`, and `write_rx` dropped here
        };

        self.thread_handle = Some(thread::spawn(func));
    }

    fn stop_thread(&mut self) {
        if self.is_connected.load(Ordering::SeqCst) {
            self.is_connected.store(false, Ordering::SeqCst);
            self.thread_handle.take().unwrap().join().unwrap();
        }
    }

    fn io_thread(
        is_connected: &Arc<AtomicBool>,
        mut hid_device: hid::Device,
        read_tx: &Sender<hid::Report>,
        write_rx: &Receiver<hid::Report>,
        player_num: usize,
    ) -> hid::Result<()> {
        Self::init(&mut hid_device, player_num)?;

        while is_connected.load(Ordering::SeqCst) {
            Self::write(&mut hid_device, write_rx, player_num)?;
            Self::read(&mut hid_device, read_tx, player_num)?;
        }

        Ok(())
    }

    fn init(hid_device: &mut hid::Device, player_num: usize) -> hid::Result<()> {
        // Set reporting mode to non-continuous core buttons and turn on rumble.
        let mode_report = [
            OUTPUT_REPORT,
            OutputReportID::ReportMode as u8,
            if RUMBLE_ON_CONNECT { 0x01 } else { 0x00 },
            InputReportID::CoreButtons as u8,
        ];
        // Request status and turn off rumble.
        let req_status_report = [OUTPUT_REPORT, OutputReportID::RequestStatus as u8, 0x00];
        let led_1 = [
            OUTPUT_REPORT,
            OutputReportID::Led as u8,
            Led::player(player_num).bits(),
        ];

        hid_device.write(&mode_report)?;
        thread::sleep(RUMBLE_DURATION);
        hid_device.write(&req_status_report)?;
        hid_device.write(&led_1)?;

        Ok(())
    }

    fn write(
        hid_device: &mut hid::Device,
        write_rx: &Receiver<hid::Report>,
        player_num: usize,
    ) -> hid::Result<()> {
        // let req_status_report = [OUTPUT_REPORT, OutputReportID::RequestStatus as u8, 0x00];
        // hid_device.write(&req_status_report)?;

        if let Ok(report) = write_rx.try_recv() {
            // println!("P{} write: {report:0x?}", player_num + 1);
            println!("Write queue length: {}", write_rx.len());
            hid_device.write(&report)?;
        }

        Ok(())
    }

    fn read(
        hid_device: &mut hid::Device,
        read_tx: &Sender<hid::Report>,
        player_num: usize,
    ) -> hid::Result<()> {
        let report = hid_device.read()?;
        // println!("P{} read: {report:0x?}", player_num + 1);
        if !report.is_empty() {
            read_tx.send(report);
        }

        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        self.is_connected.load(Ordering::SeqCst)
    }
}

impl Drop for WiimotePollThread {
    fn drop(&mut self) {
        self.stop_thread();
    }
}
