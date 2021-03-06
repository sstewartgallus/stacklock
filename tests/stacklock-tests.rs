extern crate stacklock;

use stacklock::Mutex;
use std::sync::{Arc, Barrier};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

#[test]
fn test_as_lock() {
    let lock = Arc::new(Mutex::new(()));

    let mut children = Vec::new();
    for _ in 0..20 {
        let lock_ref = lock.clone();

        let child = thread::spawn(move || {
            for _ in 0..20 {
                let _ = lock_ref.lock();
            }
        });
        children.push(child);
    }

    for child in children {
        child.join().unwrap();
    }
}

#[test]
fn test_race() {
    let num = 20;

    let lock = Arc::new(Mutex::new(()));
    let racer = Arc::new(AtomicBool::new(false));
    let start = Arc::new(Barrier::new(num));

    let mut children = Vec::new();
    for _ in 0..num {
        let lock_ref = lock.clone();
        let racer_ref = racer.clone();
        let barrier_ref = start.clone();

        let child = thread::spawn(move || {
            barrier_ref.wait();

            for _ in 0..1000 {
                let _val = lock_ref.lock();
                for _ in 0..20 {
                    let prev = racer_ref.swap(true, Ordering::Relaxed);
                    assert_eq!(prev, false);
                    let val = racer_ref.swap(false, Ordering::Relaxed);
                    assert_eq!(val, true);
                }
            }
        });
        children.push(child);
    }

    for child in children {
        child.join().unwrap();
    }
}
