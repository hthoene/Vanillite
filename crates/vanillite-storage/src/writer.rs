//! A background thread for small file writes (player data, `level.dat`, saved data), so
//! saving never makes a tick wait for the disk. Jobs run one after another in the order they
//! were queued: the last queued write of a file wins.

use std::io;
use std::sync::mpsc::{self, Sender};

enum Job {
    Run(Box<dyn FnOnce() + Send>),
    Barrier(Sender<()>),
}

/// The handle of the writer thread. Cloned handles share the thread; it ends when the last
/// handle is dropped, after the queued jobs ran.
#[derive(Clone, Debug)]
pub struct FileWriter {
    jobs: Sender<Job>,
}

impl FileWriter {
    /// Starts the writer thread.
    pub fn start() -> io::Result<FileWriter> {
        let (jobs, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("Storage writer".to_owned())
            .spawn(move || {
                for job in rx {
                    match job {
                        Job::Run(job) => {
                            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err()
                            {
                                log::error!("A storage write panicked");
                            }
                        }
                        Job::Barrier(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            })?;
        Ok(FileWriter { jobs })
    }

    /// Queues a write. Without the thread (it panicked) the job runs on the calling thread.
    pub fn submit(&self, job: impl FnOnce() + Send + 'static) {
        if let Err(mpsc::SendError(Job::Run(job))) = self.jobs.send(Job::Run(Box::new(job))) {
            job();
        }
    }

    /// Waits until every write queued before this call is done.
    pub fn flush(&self) {
        let (done, wait) = mpsc::channel();
        if self.jobs.send(Job::Barrier(done)).is_ok() {
            let _ = wait.recv();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn jobs_run_in_order_and_flush_waits() {
        let writer = FileWriter::start().unwrap();
        let seen = Arc::new(AtomicUsize::new(0));
        for i in 0..100 {
            let seen = Arc::clone(&seen);
            writer.submit(move || {
                assert_eq!(seen.load(Ordering::SeqCst), i);
                seen.store(i + 1, Ordering::SeqCst);
            });
        }
        writer.flush();
        assert_eq!(seen.load(Ordering::SeqCst), 100);
    }
}
