use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use sensor_sim::{
    accelerometer::Accelerometer,
    force_sensor::ForceSensor,
    thermometer::Thermometer,
    traits::Sensor,
};


fn main() {
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    ctrlc::set_handler(move || {
        println!("\nExit");
        running_clone.store(false, Ordering::SeqCst);
    })
    .expect("fail to set Ctrl+C handler");

    let mut thermo_1 = Thermometer::new("thermo-1".to_string(), 10);
    let mut thermo_2 = Thermometer::new("thermo-2".to_string(), 10);
    let mut accel_1 = Accelerometer::new("accel-1".to_string(), 20);
    let mut accel_2 = Accelerometer::new("accel-2".to_string(), 20);
    let mut force_1 = ForceSensor::new("force-1".to_string(), 15);

    thermo_1.start();
    thermo_2.start();
    accel_1.start();
    accel_2.start();
    force_1.start();

    let running_clone = running.clone();
    let handle_t1 = thread::spawn(move || {
        while running_clone.load(Ordering::Relaxed) {

        }
    });

    let running_clone = running.clone();
    let handle_t2 = thread::spawn(move || {
        while running_clone.load(Ordering::Relaxed) {

        }
    });

    let running_clone = running.clone();
    let handle_a1 = thread::spawn(move || {
        while running_clone.load(Ordering::Relaxed) {

        }
    });

    let running_clone=running.clone();
    let handle_a2 = thread::spawn(move || {
        while running_clone.load(Ordering::Relaxed) {

        }
    });

    let running_clone=running.clone();
    let handle_f1 = thread::spawn(move || {
        while running_clone.load(Ordering::Relaxed) {

        }
    });

    while running.load(Ordering::Relaxed) {
        print!("reading!");
    }

    println!("Exit");
    handle_t1.join().unwrap();
    handle_t2.join().unwrap();
    handle_a1.join().unwrap();
    handle_a2.join().unwrap();
    handle_f1.join().unwrap();

}