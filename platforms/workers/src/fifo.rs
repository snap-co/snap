//! FIFO turns for local socket callbacks. A queued callback cannot overtake an
//! older one, even if an earlier waiter is cancelled before or after being woken.
use futures_channel::oneshot;
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

#[derive(Default)]
pub(super) struct Fifo(Rc<RefCell<State>>);
#[derive(Default)]
struct State {
    active: bool,
    waiters: VecDeque<oneshot::Sender<Turn>>,
}
pub(super) struct Turn(Option<Rc<RefCell<State>>>);

impl Fifo {
    pub async fn enter(&self) -> Turn {
        let receive = {
            let mut state = self.0.borrow_mut();
            if !state.active {
                state.active = true;
                return Turn(Some(self.0.clone()));
            }
            let (send, receive) = oneshot::channel();
            state.waiters.push_back(send);
            receive
        };
        receive.await.expect("the active turn owns the queue")
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        let Some(state) = self.0.take() else {
            return;
        };
        loop {
            let waiter = state.borrow_mut().waiters.pop_front();
            let Some(waiter) = waiter else {
                state.borrow_mut().active = false;
                return;
            };
            match waiter.send(Turn(Some(state.clone()))) {
                Ok(()) => return,
                // A cancelled receiver returns ownership. Skip it without
                // recursively dropping turns or borrowing State across a wake.
                Err(mut turn) => {
                    turn.0.take();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;

    // Control the executor schedule that socket IO cannot deterministically
    // force: a new waiter arrives after the oldest waiter has been awakened.
    #[test]
    fn turns_remain_fifo_when_waiter_slots_are_reused_or_cancelled() {
        let queue = Fifo::default();
        let first = queue.enter().now_or_never().unwrap();
        let mut second = Box::pin(queue.enter());
        let mut third = Box::pin(queue.enter());
        let mut cx = std::task::Context::from_waker(futures_util::task::noop_waker_ref());
        use std::future::Future;
        assert!(second.as_mut().poll(&mut cx).is_pending());
        assert!(third.as_mut().poll(&mut cx).is_pending());
        drop(first);
        let second = second.now_or_never().unwrap();
        let mut fourth = Box::pin(queue.enter());
        assert!(fourth.as_mut().poll(&mut cx).is_pending());
        drop(second);
        assert!(fourth.as_mut().poll(&mut cx).is_pending());
        let third = third.now_or_never().unwrap();
        drop(third);
        let fourth = fourth.now_or_never().unwrap();
        let mut abandoned = Box::pin(queue.enter());
        assert!(abandoned.as_mut().poll(&mut cx).is_pending());
        drop(abandoned); // Cancel while still queued.
        let mut cancelled = Box::pin(queue.enter());
        let mut last = Box::pin(queue.enter());
        assert!(cancelled.as_mut().poll(&mut cx).is_pending());
        assert!(last.as_mut().poll(&mut cx).is_pending());
        drop(fourth);
        drop(cancelled); // Cancel after receiving ownership, before polling.
        drop(last.now_or_never().unwrap());
        assert!(queue.enter().now_or_never().is_some());
    }
}
