//! Work the UI thread hands off and polls (M1 plan section 7.9): the
//! doctor's checks, a directory listing, a probe. Each runs on its own
//! named thread and sends one result back; the UI thread only
//! `try_recv`s. A job given up before it ends leaves its thread in
//! [`Threads`], which [`crate::run`] joins once the window has gone.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::JoinHandle;

/// One piece of work running on its own thread.
#[derive(Debug)]
pub struct Job<T> {
    rx: Receiver<T>,
    /// The thread, for the join at exit: none for a test's job, or one
    /// whose thread couldn't start.
    thread: Option<JoinHandle<()>>,
}

/// What [`Job::poll`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Poll<T> {
    Pending,
    Done(T),
    /// The thread ended without a result: it panicked, or couldn't start.
    Lost,
}

impl<T> Poll<T> {
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Poll<U> {
        match self {
            Poll::Pending => Poll::Pending,
            Poll::Done(t) => Poll::Done(f(t)),
            Poll::Lost => Poll::Lost,
        }
    }
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
        let thread = match started {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::error!(error = %e, name, "a worker thread can't start");
                None
            }
        };
        Job { rx, thread }
    }

    /// A job whose result comes on `rx`, for tests.
    pub fn from_receiver(rx: Receiver<T>) -> Job<T> {
        Job { rx, thread: None }
    }

    /// A job that is already done, for tests.
    pub fn done(value: T) -> Job<T> {
        let (tx, rx) = mpsc::channel();
        let _ = tx.send(value);
        Job { rx, thread: None }
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

impl<T> Job<T> {
    /// The job's thread, if it hasn't ended.
    pub fn into_thread(self) -> Option<JoinHandle<()>> {
        self.thread.filter(|h| !h.is_finished())
    }
}

/// The threads of jobs given up before they ended: a listing of a folder
/// left since, a probe of a file closed since, any job at exit. They are
/// joined after the event loop ([`crate::run`]), so that none is still in
/// a driver or FFmpeg while the process's exit tears those down.
#[derive(Debug, Default)]
pub struct Threads(Vec<JoinHandle<()>>);

impl Threads {
    /// Keep `job`'s thread if it still runs, and forget those that ended.
    /// Never waits.
    pub fn keep<T>(&mut self, job: Job<T>) {
        self.0.retain(|h| !h.is_finished());
        self.0.extend(job.into_thread());
    }

    /// The threads kept, for the join.
    pub fn take(&mut self) -> Vec<JoinHandle<()>> {
        std::mem::take(&mut self.0)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The next poll that isn't pending.
    fn settled<T: Send + 'static>(job: &Job<T>) -> Poll<T> {
        loop {
            match job.poll() {
                Poll::Pending => std::thread::yield_now(),
                other => return other,
            }
        }
    }

    #[test]
    fn a_job_comes_back_once_and_a_panic_is_lost() {
        let job = Job::spawn("test-job", || 7);
        assert_eq!(settled(&job), Poll::Done(7));
        // Pending until the thread, past its send, drops its end.
        assert_eq!(settled(&job), Poll::Lost);
        let job: Job<u8> = Job::spawn("test-panic", || panic!("on purpose"));
        assert_eq!(settled(&job), Poll::Lost);
    }

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "a test's worker waits to be let go, and is joined"
    )]
    fn a_given_up_job_leaves_its_thread_until_it_ends() {
        let (go, wait) = mpsc::channel::<()>();
        let job = Job::spawn("test-given-up", move || {
            let _ = wait.recv();
            1
        });
        let mut threads = Threads::default();
        threads.keep(job);
        assert_eq!(threads.len(), 1);
        // A job with no thread leaves none.
        threads.keep(Job::done(2));
        assert_eq!(threads.len(), 1);
        go.send(()).unwrap();
        let mut left = threads.take();
        assert!(threads.is_empty());
        left.pop().unwrap().join().unwrap();
        // A thread that ended is forgotten at the next keep.
        let job = Job::spawn("test-ended", || 3);
        while job.thread.as_ref().is_some_and(|h| !h.is_finished()) {
            std::thread::yield_now();
        }
        threads.keep(job);
        assert!(threads.is_empty());
    }
}
