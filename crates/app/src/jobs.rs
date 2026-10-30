//! Work the UI thread hands off and polls (M1 plan section 7.9): the
//! doctor's checks, a directory listing, a probe. Each runs on its own
//! named thread and sends one result back; the UI thread only
//! `try_recv`s.

use std::sync::mpsc::{self, Receiver, TryRecvError};

/// One piece of work running on its own thread.
#[derive(Debug)]
pub struct Job<T> {
    rx: Receiver<T>,
}

/// What [`Job::poll`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Poll<T> {
    Pending,
    Done(T),
    /// The thread ended without a result: it panicked, or couldn't start.
    Lost,
}

impl<T: Send + 'static> Job<T> {
    /// Run `f` on a thread named `name`. If the thread can't start, the
    /// job is [`Poll::Lost`] at once.
    pub fn spawn(name: &str, f: impl FnOnce() -> T + Send + 'static) -> Job<T> {
        let (tx, rx) = mpsc::channel();
        let started = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let _ = tx.send(f());
            });
        if let Err(e) = started {
            tracing::error!(error = %e, name, "a worker thread can't start");
        }
        Job { rx }
    }

    /// A job whose result comes on `rx`, for tests.
    pub fn from_receiver(rx: Receiver<T>) -> Job<T> {
        Job { rx }
    }

    /// A job that is already done, for tests.
    pub fn done(value: T) -> Job<T> {
        let (tx, rx) = mpsc::channel();
        let _ = tx.send(value);
        Job { rx }
    }

    /// The result, once there is one. Never waits.
    pub fn poll(&self) -> Poll<T> {
        match self.rx.try_recv() {
            Ok(v) => Poll::Done(v),
            Err(TryRecvError::Empty) => Poll::Pending,
            Err(TryRecvError::Disconnected) => Poll::Lost,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_comes_back_once_and_a_panic_is_lost() {
        let job = Job::spawn("test-job", || 7);
        let got = loop {
            match job.poll() {
                Poll::Pending => std::thread::yield_now(),
                other => break other,
            }
        };
        assert_eq!(got, Poll::Done(7));
        assert_eq!(job.poll(), Poll::Lost);
        let job: Job<u8> = Job::spawn("test-panic", || panic!("on purpose"));
        let got = loop {
            match job.poll() {
                Poll::Pending => std::thread::yield_now(),
                other => break other,
            }
        };
        assert_eq!(got, Poll::Lost);
    }
}
