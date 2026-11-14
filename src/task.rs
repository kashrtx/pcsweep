use eframe::egui;
use std::thread::JoinHandle;

/// A background job whose result is picked up by the UI thread when it finishes.
pub struct Task<T> {
    handle: Option<JoinHandle<T>>,
}

impl<T> Default for Task<T> {
    fn default() -> Self {
        Self { handle: None }
    }
}

impl<T: Send + 'static> Task<T> {
    pub fn spawn<F>(ctx: &egui::Context, f: F) -> Self
    where
        F: FnOnce() -> T + Send + 'static,
    {
        let ctx = ctx.clone();
        let handle = std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(move || {
                let out = f();
                ctx.request_repaint();
                out
            })
            .expect("failed to start a background thread");
        Self {
            handle: Some(handle),
        }
    }

    pub fn running(&self) -> bool {
        self.handle.is_some()
    }

    /// Returns `Some` exactly once, when the job has finished.
    pub fn poll(&mut self) -> Option<Result<T, String>> {
        let finished = self
            .handle
            .as_ref()
            .map(|h| h.is_finished())
            .unwrap_or(false);
        if !finished {
            return None;
        }
        let handle = self.handle.take()?;
        Some(handle.join().map_err(|e| {
            if let Some(s) = e.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = e.downcast_ref::<String>() {
                s.clone()
            } else {
                "A background task stopped unexpectedly".to_string()
            }
        }))
    }
}
