// Copyright 2017 Steven Stewart-Gallus
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or
// implied.  See the License for the specific language governing
// permissions and limitations under the License.
//
use libc;

use std::mem;
use std::sync::atomic;
use std::sync::atomic::{AtomicU32, Ordering};

use exp;
use backoff;
use cacheline::CacheLineAligned;

const NUM_LOOPS: usize = 20;
const MAX_LOG_NUM_PAUSES: usize = 5;

const SPINNING: u32 = 0;
const NOT_SPINNING: u32 = 1;

// Due to legacy issues on x86 operations on values smaller than 32
// bits can be slow.

/// A single waiter, single signaller event semaphore.  Signaled once
/// and then thrown away.
pub struct Notifier {
    spin_state: CacheLineAligned<AtomicU32>,
    triggered: CacheLineAligned<AtomicU32>,
}

const FUTEX_WAIT_PRIVATE: usize = 0 | 128;
const FUTEX_WAKE_PRIVATE: usize = 1 | 128;

fn untriggered() -> u32 {
    u32::max_value()
}
// Make sure comparisons are against zero
fn triggered() -> u32 {
    0
}

impl Notifier {
    pub fn new() -> Notifier {
        Notifier {
            spin_state: CacheLineAligned::new(AtomicU32::new(SPINNING)),
            triggered: CacheLineAligned::new(AtomicU32::new(untriggered())),
        }
    }

    pub fn reset(&self) {
        self.spin_state.store(SPINNING, Ordering::Relaxed);
        self.triggered.store(untriggered(), Ordering::Relaxed);
    }

    pub fn wait(&self) {
        'wait_loop: loop {
            {
                let mut counter = 0;
                loop {
                    if triggered() == self.triggered.load(Ordering::Relaxed) {
                        break 'wait_loop;
                    }
                    if counter >= NUM_LOOPS {
                        break;
                    }
                    for _ in 0..backoff::thread_num(exp::exp(counter,
                                                             NUM_LOOPS,
                                                             MAX_LOG_NUM_PAUSES)) {
                        backoff::pause();
                    }
                    counter += 1;
                }
            }

            self.spin_state.store(NOT_SPINNING, Ordering::Relaxed);

            atomic::fence(Ordering::AcqRel);

            if triggered() == self.triggered.load(Ordering::Relaxed) {
                break;
            }

            let result: usize;
            unsafe {
                let trig: usize = mem::transmute(&self.triggered);
                result = syscall!(FUTEX, trig, FUTEX_WAIT_PRIVATE, untriggered(), 0);
            }
            // woken up
            if 0 == result {
                break;
            }
            // futex checked and found that a trigger already happened
            if -libc::EWOULDBLOCK as usize == result {
                break;
            }

            self.spin_state.store(SPINNING, Ordering::Relaxed);
        }
        atomic::fence(Ordering::Acquire);
    }

    pub fn signal(&self) {
        // If the waiter was spinning we can avoid a syscall.
        atomic::fence(Ordering::Release);

        self.triggered.store(triggered(), Ordering::Relaxed);

        atomic::fence(Ordering::AcqRel);

        if SPINNING == self.spin_state.load(Ordering::Relaxed) {
            return;
        }

        unsafe {
            let trig: usize = mem::transmute(&self.triggered);
            syscall!(FUTEX, trig, FUTEX_WAKE_PRIVATE, 1);
        }
    }
}
