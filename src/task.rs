use eframe::egui;
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// A background job whose result is picked up by the UI thread when it finishes.
pub struct Task<T> {
    result: Option<Receiver<Result<T, String>>>,
}

impl<T> Default for Task<T> {
    fn default() -> Self {
        Self { result: None }
    }
}

impl<T: Send + 'static> Task<T> {
    pub fn spawn<F>(ctx: &egui::Context, f: F) -> Self
    where
        F: FnOnce() -> T + Send + 'static,
    {
        let (sender, receiver) = mpsc::channel();
        let failed = sender.clone();
        let ctx = ctx.clone();
        let started = std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(move || {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|e| {
                        if let Some(s) = e.downcast_ref::<&str>() {
                            s.to_string()
                        } else if let Some(s) = e.downcast_ref::<String>() {
                            s.clone()
                        } else {
                            "A background task stopped unexpectedly".to_string()
                        }
                    });
                // Publish before waking the UI: a repaint always sees the result,
                // even if the worker thread has not quite exited yet.
                let _ = sender.send(result);
                ctx.request_repaint();
            });
        if let Err(e) = started {
            let _ = failed.send(Err(format!("Could not start a background task: {e}")));
        }
        Self {
            result: Some(receiver),
        }
    }

    pub fn running(&self) -> bool {
        self.result.is_some()
    }

    /// Returns `Some` exactly once, when the job has finished.
    pub fn poll(&mut self) -> Option<Result<T, String>> {
        let result = match self.result.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                Err("A background task stopped unexpectedly".to_string())
            }
        };
        self.result = None;
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn finish<T: Send + 'static>(task: &mut Task<T>) -> Result<T, String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = task.poll() {
                return result;
            }
            assert!(Instant::now() < deadline, "background task timed out");
            std::thread::yield_now();
        }
    }

    #[test]
    fn completion_is_delivered_once() {
        let mut task = Task::spawn(&egui::Context::default(), || 42);
        assert!(task.running());
        assert_eq!(finish(&mut task), Ok(42));
        assert!(!task.running());
        assert!(task.poll().is_none());
    }

    #[test]
    fn polling_does_not_wait_for_a_busy_worker() {
        let (release, wait) = mpsc::channel();
        let mut task = Task::spawn(&egui::Context::default(), move || wait.recv().unwrap());
        assert!(task.poll().is_none());
        assert!(task.running());
        release.send(7).unwrap();
        assert_eq!(finish(&mut task), Ok(7));
    }

    #[test]
    fn panic_becomes_a_reportable_error() {
        let mut task = Task::<()>::spawn(&egui::Context::default(), || panic!("scan failed"));
        assert_eq!(finish(&mut task).unwrap_err(), "scan failed");
        assert!(!task.running());
    }
}
