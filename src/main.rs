mod bluetooth;
mod hid;
mod scanner;
mod util;
mod wiimote;

use std::io::{stdin, Read};
use std::iter;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::hid::OUTPUT_REPORT;
use crate::scanner::WiimoteScanner;
use crate::wiimote::{OutputReportID, WiimotePollThread};

// TODO: Logging
// TODO: https://x-io.co.uk/open-source-imu-and-ahrs-algorithms/
// TODO: Newtype for player numbers

const MAX_PLAYERS: usize = 8;

struct WiimoteSlot {
    wiimote_thread: WiimotePollThread,
    read_rx: Receiver<hid::Report>,
    write_tx: Sender<hid::Report>,
    device_path: String,
}

impl WiimoteSlot {
    pub fn new(device_path: String, player_num: usize) -> Self {
        println!("Opening HID Device with path {device_path:?}");
        let hid_device = hid::Device::open(&device_path).unwrap();
        let (read_tx, read_rx) = unbounded();
        let (write_tx, write_rx) = unbounded();
        let wiimote_thread = WiimotePollThread::new(hid_device, read_tx, write_rx, player_num);

        Self {
            wiimote_thread,
            read_rx,
            write_tx,
            device_path,
        }
    }

    pub fn is_connected(&self) -> bool {
        self.wiimote_thread.is_connected()
    }

    pub fn device_path(&self) -> &str {
        &self.device_path
    }
}

fn iter_slots(slots: &[Option<WiimoteSlot>]) -> impl Iterator<Item = (usize, &WiimoteSlot)> + '_ {
    slots
        .iter()
        .enumerate()
        .flat_map(|(player_num, slot_opt)| Some(player_num).zip(slot_opt.as_ref()))
}

fn try_recv_read_msgs(
    slots: &[Option<WiimoteSlot>],
) -> impl Iterator<Item = (usize, hid::Report)> + '_ {
    iter_slots(slots)
        .flat_map(|(player_num, slot)| iter::repeat(player_num).zip(slot.read_rx.try_iter()))
}

fn write_txs(
    slots: &[Option<WiimoteSlot>],
) -> impl Iterator<Item = (usize, &Sender<hid::Report>)> + '_ {
    iter_slots(slots).map(|(player_num, slot)| (player_num, &slot.write_tx))
}

fn main() {
    let is_running = Arc::new(AtomicBool::new(true));
    let thread_is_running = Arc::clone(&is_running);

    let join_handle = thread::spawn(move || {
        let (device_tx, device_rx) = unbounded();
        let mut scanner = WiimoteScanner::new();
        scanner.start_thread(device_tx);

        let mut wiimote_slots: [Option<WiimoteSlot>; MAX_PLAYERS] = Default::default();
        let mut is_pressed: [bool; MAX_PLAYERS] = Default::default();
        let mut num_pressed: [i32; MAX_PLAYERS] = Default::default();

        while thread_is_running.load(Ordering::SeqCst) {
            // Remove disconnected wiimotes
            // FIXME: drain_filter()?
            let mut i = 0;
            while i < wiimote_slots.len() {
                if let Some(wiimote_slot) = &wiimote_slots[i] {
                    if !wiimote_slot.is_connected() {
                        scanner.forget_device_path(wiimote_slot.device_path());
                        wiimote_slots[i] = None;

                        // XXX: How do we handle logic on disconnect?
                        is_pressed[i] = false;
                        num_pressed[i] = 0;

                        println!("Removed wiimote from slot {i}");
                    }
                }

                i += 1;
            }

            // Add new wiimotes to the slots
            for device_path in device_rx.try_iter() {
                // Add the wiimote to the first available slot
                let player_num = wiimote_slots
                    .iter()
                    .position(|wm| wm.is_none())
                    .unwrap_or_else(|| panic!("Maximum of {MAX_PLAYERS} wiimotes"));

                let wiimote_slot = Some(WiimoteSlot::new(device_path, player_num));
                wiimote_slots[player_num] = wiimote_slot;
            }

            // XXX: Request continuous reporting
            let mut req_status_report = hid::Report::new();
            req_status_report.push(OUTPUT_REPORT);
            req_status_report.push(OutputReportID::RequestStatus.into());
            req_status_report.push(0x00);

            // Process reports read from the wiimotes
            for (player_num, report) in try_recv_read_msgs(&wiimote_slots) {
                if report[3] == 0x08 {
                    if !is_pressed[player_num] {
                        num_pressed[player_num] += 1;
                        println!(
                            "Player {} has pressed A {} times",
                            player_num + 1,
                            num_pressed[player_num]
                        );
                    }

                    is_pressed[player_num] = true;
                } else if report[3] == 0x00 {
                    is_pressed[player_num] = false;
                }
            }
        }

        scanner.stop_thread();
        println!("Main thread stopped");
    });

    println!("Press a key to stop the program...");
    stdin().read_exact(&mut [0]).unwrap();

    println!("Stopping...");
    is_running.store(false, Ordering::SeqCst);
    join_handle.join().unwrap();
}
