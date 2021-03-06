use std::marker::PhantomData;
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};

pub trait TestCase {
    type TestType: Clone + Send;

    fn create_value() -> Self::TestType;
    fn do_stuff_with_value(value: &Self::TestType, times: usize);
}

pub const STANDARD_TESTS: [usize; 4] = [2, 3, 4, 20];

pub fn contend<T: TestCase + 'static, B>(_: PhantomData<T>, b: B, numthreads: usize)
    where B: FnOnce(&mut FnMut())
{
    let lock: T::TestType = T::create_value();

    let mut children = Vec::new();

    let is_done = Arc::new(AtomicBool::new(false));
    let start = Arc::new(Barrier::new(numthreads + 1));
    let done = Arc::new(Barrier::new(numthreads + 1));

    for _ in 0..numthreads {
        let lock_ref = lock.clone();
        let start_ref = start.clone();
        let done_ref = done.clone();
        let is_done_ref = is_done.clone();

        let child = thread::spawn(move || {
            loop {
                start_ref.wait();
                if is_done_ref.load(Ordering::Acquire) {
                    break;
                }

                T::do_stuff_with_value(&lock_ref, 2000);

                done_ref.wait();
            }
        });
        children.push(child);
    }

    let mut func = || {
        start.wait();
        done.wait();
    };
    b(&mut func);
    is_done.store(true, Ordering::Release);
    start.wait();

    for child in children {
        child.join().unwrap();
    }
}
