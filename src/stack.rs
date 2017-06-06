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
use std::mem;
use std::sync::atomic::{AtomicU64, Ordering};

use std::ptr;
use qlock_util::backoff;
use qlock_util::cacheline::CacheLineAligned;
use notifier::Notifier;

static mut DUMMY_NODE: Node = Node::new_uninit();

#[inline]
pub fn dummy_node() -> *mut Node {
    unsafe {
        return &mut DUMMY_NODE;
    }
}

#[derive(Copy, Clone)]
struct Aba {
    ptr: u64,
}
impl Aba {
    fn new(ptr: *mut Node, tag: u32) -> Self {
        Aba { ptr: to_u64(ptr, tag) }
    }
    fn ptr(&self) -> *mut Node {
        from_u64(self.ptr).0
    }
    fn tag(&self) -> u32 {
        from_u64(self.ptr).1
    }
}
impl PartialEq for Aba {
    fn eq(&self, other: &Aba) -> bool {
        self.ptr == other.ptr
    }
}
struct AtomicAba {
    ptr: AtomicU64,
}
impl AtomicAba {
    #[inline(always)]
    fn new(ptr: *mut Node) -> Self {
        AtomicAba { ptr: AtomicU64::new(to_u64(ptr, 0)) }
    }

    fn load(&self, ordering: Ordering) -> Aba {
        Aba { ptr: self.ptr.load(ordering) }
    }

    fn compare_exchange_weak(&self,
                             old: Aba,
                             new: Aba,
                             success: Ordering,
                             fail: Ordering)
                             -> Result<Aba, Aba> {
        match self.ptr
            .compare_exchange_weak(old.ptr, new.ptr, success, fail) {
            Err(x) => Err(Aba { ptr: x }),
            Ok(x) => Ok(Aba { ptr: x }),
        }
    }
}

fn to_u64(node: *mut Node, tag: u32) -> u64 {
    unsafe {
        let node_bits: u64 = mem::transmute(node);
        let tag_bits: u64 = (tag & ((1 << 23) - 1)) as u64;
        tag_bits | node_bits << 16
    }
}
fn from_u64(ptr: u64) -> (*mut Node, u32) {
    unsafe {
        let node_bits = (ptr >> 23) << 7;
        let tag_bits: u64 = ptr & ((1 << 23) - 1);
        (mem::transmute(node_bits), tag_bits as u32)
    }
}

const MAX_EXP: usize = 6;

pub struct Node {
    notifier: Notifier,
    next: CacheLineAligned<*mut Node>,
}

impl Node {
    #[inline(always)]
    pub fn new() -> Node {
        Node {
            notifier: Notifier::new(),
            next: CacheLineAligned::new(dummy_node()),
        }
    }

    pub const fn new_uninit() -> Node {
        Node {
            notifier: Notifier::new(),
            next: CacheLineAligned::new(ptr::null_mut()),
        }
    }

    pub fn signal(&self) {
        self.notifier.signal();
    }

    pub fn wait(&self) {
        self.notifier.wait();
    }
}

pub struct Stack {
    head: CacheLineAligned<AtomicAba>,
}

impl Stack {
    #[inline(always)]
    pub fn new() -> Self {
        Stack { head: CacheLineAligned::new(AtomicAba::new(dummy_node())) }
    }

    pub unsafe fn push(&self, node: *mut Node) {
        let mut head = self.head.load(Ordering::Relaxed);
        let mut counter = 0;
        loop {
            let new = Aba::new(node, head.tag().wrapping_add(1));

            *(*node).next = head.ptr();

            let newhead = self.head.load(Ordering::Relaxed);
            if newhead != head {
                head = newhead;
            } else {
                match self.head
                    .compare_exchange_weak(head, new, Ordering::Release, Ordering::Relaxed) {
                    Err(newhead) => {
                        head = newhead;
                    }
                    Ok(_) => break,
                }
            }

            let exp;
            if counter > MAX_EXP {
                exp = 1 << MAX_EXP;
            } else {
                exp = 1 << counter;
                counter = counter.wrapping_add(1);
            }
            backoff::yield_now();

            let spins = backoff::thread_num(1, exp);

            backoff::pause_times(spins);
        }
    }

    pub fn pop(&self) -> *mut Node {
        unsafe {
            let mut head = self.head.load(Ordering::Relaxed);

            let mut next = *(*head.ptr()).next;
            let mut new = Aba::new(next, head.tag().wrapping_add(1));

            if head.ptr() == dummy_node() {
                return dummy_node();
            }

            let mut counter = 0;
            loop {
                let maybe_head = self.head.load(Ordering::Relaxed);
                if maybe_head != head {
                    head = maybe_head;

                    next = *(*head.ptr()).next;
                    new = Aba::new(next, head.tag().wrapping_add(1));

                    if head.ptr() == dummy_node() {
                        return dummy_node();
                    }
                } else {
                    match self.head
                        .compare_exchange_weak(head, new, Ordering::Release, Ordering::Relaxed) {
                        Err(newhead) => {
                            head = newhead;

                            next = *(*head.ptr()).next;
                            new = Aba::new(next, head.tag().wrapping_add(1));

                            if head.ptr() == dummy_node() {
                                return dummy_node();
                            }
                        }
                        Ok(_) => break,
                    }
                }

                let exp;
                if counter > MAX_EXP {
                    exp = 1 << MAX_EXP;
                } else {
                    exp = 1 << counter;
                    counter = counter.wrapping_add(1);
                }
                backoff::yield_now();

                let spins = backoff::thread_num(1, exp);

                backoff::pause_times(spins);
            }
            return head.ptr();
        }
    }
}
