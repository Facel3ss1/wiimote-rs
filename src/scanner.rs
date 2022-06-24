use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crossbeam_channel::Sender;

use crate::bluetooth;
use crate::hid;
use crate::util;

// XXX: use a thread::Builder
// TODO: Start and stop wiimote scanning on demand

pub struct WiimoteScanner {
    // Remember device paths so we don't try to connect to the same device twice
    known_paths: Arc<Mutex<HashSet<String>>>,
    thread_running: Arc<AtomicBool>,
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl WiimoteScanner {
    pub fn new() -> Self {
        Self {
            known_paths: Arc::new(Mutex::new(HashSet::new())),
            thread_running: Arc::new(AtomicBool::new(false)),
            thread_handle: None,
        }
    }

    pub fn start_thread(&mut self, device_tx: Sender<String>) {
        if self.thread_running.load(Ordering::SeqCst) {
            return;
        }
        self.thread_running.store(true, Ordering::SeqCst);

        let known_paths_mutex = Arc::clone(&self.known_paths);
        let is_running = Arc::clone(&self.thread_running);
        let func = move || Self::scanning_thread(&is_running, &known_paths_mutex, device_tx);

        self.thread_handle = Some(thread::spawn(func));
    }

    pub fn stop_thread(&mut self) {
        if self.thread_running.load(Ordering::SeqCst) {
            self.thread_running.store(false, Ordering::SeqCst);

            self.thread_handle.take().unwrap().join().unwrap();
        }
    }

    fn scanning_thread(
        is_running: &Arc<AtomicBool>,
        known_paths_mutex: &Arc<Mutex<HashSet<String>>>,
        device_tx: Sender<String>,
    ) {
        while is_running.load(Ordering::SeqCst) {
            println!("[WiimoteScanner] Updating bluetooth devices...");
            // Scan for bluetooth devices, then enable new wiimotes and remove disconnected wiimotes
            bluetooth::iter_devices(true, |bt_device| {
                println!(
                    "[Bluetooth] Found \"{}\" ({})",
                    bt_device.name(),
                    bt_device.address(),
                );

                if util::is_valid_device_name(bt_device.name()) {
                    let wiimote = bt_device;

                    println!(
                        "[Bluetooth] Wiimote detected - Authenticated: {}, Connected: {}, Remembered: {}",
                        wiimote.is_authenticated(),
                        wiimote.is_connected(),
                        wiimote.is_remembered()
                    );

                    // Disable and remove any remembered devices that aren't connected
                    if wiimote.is_remembered() && !wiimote.is_connected() {
                        // XXX: This probably isn't needed
                        // match wiimote.disable_device() {
                        //     Ok(_) => println!("[Bluetooth] Disabled Wiimote {}", wiimote.address()),
                        //     Err(e) => eprintln!("[Bluetooth] Error disabling Wiimote {}: {:?}", wiimote.address(), e),
                        // }

                        wiimote.remove();
                        println!("[Bluetooth] Removed Wiimote {}", wiimote.address());

                        return;
                    }

                    // Ignore any currently connected wiimotes
                    if wiimote.is_connected() {
                        return;
                    }

                    // Wiimotes at this point are not remembered or connected - so enable them
                    match wiimote.enable() {
                        Ok(_) => println!("[Bluetooth] Enabled Wiimote {}", wiimote.address()),
                        Err(e) => eprintln!("[Bluetooth] Error enabling Wiimote: {e:?}"),
                    }
                }
            });

            println!("[WiimoteScanner] Finding HID devices...");
            {
                let mut known_paths = known_paths_mutex.lock().unwrap();
                let device_enumerator = hid::DeviceEnumerator::new();

                for device_info in device_enumerator.devices().filter(|d| d.is_wiimote()) {
                    let device_path = device_info.path;
                    // Ignore any currently connected (known) wiimotes
                    if known_paths.contains(&device_path) {
                        continue;
                    }

                    // Send the device path and remember this wiimote
                    device_tx.send(device_path.clone());
                    known_paths.insert(device_path);
                    // println!("[WiimoteScanner] known_paths: {known_paths:?}");
                }
            }
        }

        // TODO: Disconnect/Power off wiimotes here (could be done on drop?)
        println!("[WiimoteScanner] Thread stopped");
    }

    pub fn forget_device_path(&self, path: &str) {
        let mut known_ids = self.known_paths.lock().unwrap();
        known_ids.remove(path);
    }
}

impl Drop for WiimoteScanner {
    fn drop(&mut self) {
        self.stop_thread();
    }
}
