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
#![feature(integer_atomics, asm, repr_simd, attr_literals)]

#[macro_use]
extern crate syscall;
extern crate libc;

mod backoff;
mod cacheline;
mod notifier;

use std::sync::atomic::{AtomicPtr, Ordering};
use std::thread;
use std::ptr;

use cacheline::CacheLineAligned;
use notifier::Notifier;

/// An MCS queue-lock
pub struct QLock {
    head: CacheLineAligned<AtomicPtr<QLockNode>>,
}
unsafe impl Send for QLock {}
unsafe impl Sync for QLock {}

pub struct QLockGuard<'r> {
    lock: &'r QLock,
    node: &'r mut QLockNode
}

impl QLock {
    pub fn new() -> Self {
        QLock {
            head: CacheLineAligned::new(AtomicPtr::new(ptr::null_mut()))
        }
    }

    pub fn lock<'r>(&'r self, node: &'r mut QLockNode) -> QLockGuard<'r> {
        node.notifier.reset();
        node.next.store(ptr::null_mut(), Ordering::Relaxed);

        unsafe {
            let head = self.head.swap(node, Ordering::AcqRel);
            if head != ptr::null_mut() {
                (*head).next.store(node, Ordering::Release);
                node.wait();
            }
        }
        QLockGuard { lock: self, node: node }
    }
}
impl<'r> Drop for QLockGuard<'r> {
    fn drop(&mut self) {
        unsafe {
            let mut next = self.node.next.load(Ordering::Acquire);
            if ptr::null_mut() == next {
                if let Ok(_) = self.lock.head.compare_exchange(self.node, ptr::null_mut(), Ordering::AcqRel, Ordering::Acquire) {
                    return;
                }

                let iters = 20;
                let mut counter = 0;
                let max = 9;
                loop {
                    next = self.node.next.load(Ordering::Acquire);
                    if next != ptr::null_mut() {
                        break;
                    }
                    counter += 1;
                    if counter >= iters {
                        break;
                    }
                    for _ in 0 .. 1 << if counter < max { counter } else { max } {
                        backoff::pause();
                    }
                }
                if next == ptr::null_mut() {
                    loop {
                        next = self.node.next.load(Ordering::Acquire);
                        if next != ptr::null_mut() {
                            break;
                        }
                        thread::yield_now();
                    }
                }
            }

            (*next).signal();
        }
    }
}

pub struct QLockNode {
    notifier: Notifier,
    next: CacheLineAligned<AtomicPtr<QLockNode>>,
}

impl QLockNode {
    pub fn new() -> QLockNode {
        QLockNode {
            notifier: Notifier::new(),
            next: CacheLineAligned::new(AtomicPtr::new(ptr::null_mut())),
        }
    }

    fn signal(&self) {
        self.notifier.signal();
    }

    fn wait(&self) {
        self.notifier.wait();
    }
}
