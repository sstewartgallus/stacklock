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
#![feature(integer_atomics)]

#[macro_use]
extern crate syscall;

extern crate libc;

extern crate qlock_util;

mod notifier;

use std::ptr;
use std::sync::atomic;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::thread;

use qlock_util::backoff;
use qlock_util::cacheline::CacheLineAligned;

use notifier::Notifier;

const RELEASE_PAUSES: usize = 5;
const MAX_EXP: usize = 10;
const HEAD_SPINS: usize = 30;

/// An MCS queue-lock
pub struct QLock {
    head: CacheLineAligned<AtomicPtr<QLockNode>>,
}
unsafe impl Send for QLock {}
unsafe impl Sync for QLock {}

pub struct QLockGuard<'r> {
    lock: &'r QLock,
    node: &'r mut QLockNode,
}

impl QLock {
    pub fn new() -> Self {
        QLock { head: CacheLineAligned::new(AtomicPtr::new(ptr::null_mut())) }
    }

    pub fn lock<'r>(&'r self, node: &'r mut QLockNode) -> QLockGuard<'r> {
        unsafe {
            // First loads have separate branch probabilities
            if self.head.load(Ordering::Relaxed) == ptr::null_mut() {
                (*node).reset();
                if self.head
                    .compare_exchange_weak(ptr::null_mut(),
                                           node,
                                           Ordering::Release,
                                           Ordering::Relaxed)
                    .is_ok() {
                    atomic::fence(Ordering::Acquire);
                    return QLockGuard {
                        lock: self,
                        node: node,
                    };
                }
            }

            {
                let mut counter = 0;
                loop {
                    for _ in 0..1 << counter {
                        backoff::pause();
                    }

                    let guess = self.head.load(Ordering::Relaxed);
                    if guess == ptr::null_mut() {
                        (*node).reset();
                        if self.head
                            .compare_exchange_weak(ptr::null_mut(),
                                                   node,
                                                   Ordering::Release,
                                                   Ordering::Relaxed)
                            .is_ok() {
                            atomic::fence(Ordering::Acquire);
                            return QLockGuard {
                                lock: self,
                                node: node,
                            };
                        }
                    }
                    counter += 1;
                    if counter > MAX_EXP {
                        break;
                    }
                }
            }

            {
                let mut counter = HEAD_SPINS;
                loop {
                    backoff::pause();
                    thread::yield_now();

                    let guess = self.head.load(Ordering::Relaxed);
                    if guess == ptr::null_mut() {
                        (*node).reset();
                        if self.head
                            .compare_exchange_weak(ptr::null_mut(),
                                                   node,
                                                   Ordering::Release,
                                                   Ordering::Relaxed)
                            .is_ok() {
                            atomic::fence(Ordering::Acquire);
                            return QLockGuard {
                                lock: self,
                                node: node,
                            };
                        }
                    }
                    match counter.checked_sub(1) {
                        None => break,
                        Some(newcounter) => {
                            counter = newcounter;
                        }
                    }
                }
            }

            (*node).reset();
            let prev = self.head.swap(node, Ordering::AcqRel);
            if prev == ptr::null_mut() {
                return QLockGuard {
                    lock: self,
                    node: node,
                };
            }

            (*prev).next.store(node, Ordering::Release);
            node.wait();

            QLockGuard {
                lock: self,
                node: node,
            }
        }
    }
}

impl<'r> Drop for QLockGuard<'r> {
    fn drop(&mut self) {
        unsafe {
            if self.lock.head.load(Ordering::Relaxed) == self.node {
                if self.lock
                    .head
                    .compare_exchange_weak(self.node,
                                           ptr::null_mut(),
                                           Ordering::Release,
                                           Ordering::Relaxed)
                    .is_ok() {
                    return;
                }
            }

            let mut counter = RELEASE_PAUSES;
            loop {
                let next = self.node.next.load(Ordering::Relaxed);
                if next != ptr::null_mut() {
                    atomic::fence(Ordering::Acquire);
                    (*next).signal();
                    return;
                }
                match counter.checked_sub(1) {
                    None => break,
                    Some(newcounter) => {
                        counter = newcounter;
                    }
                }
                backoff::pause();
            }
            backoff::pause();
            loop {
                let next = self.node.next.load(Ordering::Relaxed);
                if next != ptr::null_mut() {
                    atomic::fence(Ordering::Acquire);
                    (*next).signal();
                    break;
                }
                backoff::pause();
                thread::yield_now();
            }
        }
    }
}

pub struct QLockNode {
    notifier: Notifier,
    next: CacheLineAligned<AtomicPtr<QLockNode>>,
}

impl QLockNode {
    #[inline]
    pub fn new() -> QLockNode {
        QLockNode {
            notifier: Notifier::new(),
            next: CacheLineAligned::new(AtomicPtr::new(ptr::null_mut())),
        }
    }

    fn reset(&self) {
        self.next.store(ptr::null_mut(), Ordering::Relaxed);
        self.notifier.reset();
    }

    fn signal(&self) {
        self.notifier.signal();
    }

    fn wait(&self) {
        self.notifier.wait();
    }
}
