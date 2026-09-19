//! Just enough of an executor to run the driver on a plain thread.
//!
//! Every future the virtual bus returns is ready when polled: the SPI model
//! answers at once, BUSY is a sleep, and DIO1 is a condition variable waited
//! on inside the future. So the driver's futures never return `Pending`, and
//! a loop with a no-op waker is a complete executor for them. A `Pending`
//! would mean a future that expects a reactor; yielding keeps the loop honest
//! if one ever appears.

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};
use std::thread;

/// Drive a future to completion on the current thread.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => thread::yield_now(),
        }
    }
}
