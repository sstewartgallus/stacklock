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
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread;

use qlock_util::backoff;
use qlock_util::cacheline::CacheLineAligned;

const MAX_EXP: usize = 8;
const YIELD_INTERVAL: usize = 8;
const LOOPS: usize = 20;

const TRIGGERED: u32 = 0;
const NOT_TRIGGERED: u32 = 1;

const SPINNING: bool = false;
const NOT_SPINNING: bool = true;

// Due to legacy issues on x86 operations on values smaller than 32
// bits can be slow.

// There are two possible implementations. One that uses 2 variables
// and one that uses 1 and cmpxchg.  The 2 variable approach reduces
// variation considerably.

/// A single waiter, single signaller event semaphore.  Signaled once
/// and then thrown away.
pub struct Notifier {
    // Must be 32 bits for futex system calls
    triggered: CacheLineAligned<AtomicU32>,
    spinning: CacheLineAligned<AtomicBool>,
}

const FUTEX_WAIT_PRIVATE: usize = 0 | 128;
const FUTEX_WAKE_PRIVATE: usize = 1 | 128;

impl Notifier {
    #[inline(always)]
    pub fn new() -> Notifier {
        Notifier {
            triggered: CacheLineAligned::new(AtomicU32::new(NOT_TRIGGERED)),
            spinning: CacheLineAligned::new(AtomicBool::new(SPINNING)),
        }
    }

    pub fn wait(&self) {
        atomic::fence(Ordering::Release);

        'wait_loop: loop {
            // The first load has a different branch probability, so
            // help the predictor
            if self.triggered.load(Ordering::Relaxed) == TRIGGERED {
                break 'wait_loop;
            }

            backoff::pause();

            {
                let mut counter = 0;
                loop {
                    if self.triggered.load(Ordering::Relaxed) == TRIGGERED {
                        break 'wait_loop;
                    }
                    if counter >= LOOPS {
                        break;
                    }

                    if counter % YIELD_INTERVAL == YIELD_INTERVAL - 1 {
                        thread::yield_now();
                    }

                    let exp;
                    if counter > MAX_EXP {
                        exp = 1 << MAX_EXP;
                    } else {
                        exp = 1 << counter;
                    }

                    // Unroll the loop for better performance
                    let spins = backoff::thread_num(1, exp);
                    let unroll = 16;
                    for _ in 0..spins % unroll {
                        backoff::pause();
                    }

                    for _ in 0..spins / unroll {
                        for _ in 0..unroll {
                            backoff::pause();
                        }
                    }

                    counter += 1;
                }
            }

            self.spinning.store(NOT_SPINNING, Ordering::Release);

            if self.triggered.load(Ordering::Acquire) == TRIGGERED {
                break;
            }

            let result: usize;
            unsafe {
                let trig: usize = mem::transmute(&self.triggered);
                result = syscall!(FUTEX, trig, FUTEX_WAIT_PRIVATE, NOT_TRIGGERED, 0);
            }
            // woken up
            if 0 == result {
                break;
            }
            // futex checked and found that a trigger already happened
            if -libc::EWOULDBLOCK as usize == result {
                break;
            }

            self.spinning.store(SPINNING, Ordering::Release);
        }
        atomic::fence(Ordering::Acquire);
    }

    pub fn signal(&self) {
        atomic::fence(Ordering::Release);

        self.triggered.store(TRIGGERED, Ordering::Release);
        if self.spinning.load(Ordering::Acquire) == SPINNING {
            return;
        }

        unsafe {
            let trig: usize = mem::transmute(&self.triggered);
            syscall!(FUTEX, trig, FUTEX_WAKE_PRIVATE, 1);
        }
    }
}
