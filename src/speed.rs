//! On-demand HTTPS measurements against Cloudflare's published speed endpoints.
use crate::{
    app::{Action, App, Modal},
    task::Task,
    theme::*,
    util, widgets,
};
use eframe::egui::{self, ScrollArea, Ui};
use std::{
    io::{self, Read},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

const PROVIDER: &str = "https://speed.cloudflare.com";
const USER_AGENT: &str = concat!("PCSweep/", env!("CARGO_PKG_VERSION"));
const MIB: u64 = 1024 * 1024;
const MAX_SAMPLE: u64 = 32 * MIB;
const RESPONSE_LIMIT: u64 = 8192;
const SAMPLE_TIME: Duration = Duration::from_secs(10);
const DIRECTION_TIME: Duration = Duration::from_secs(18);

#[derive(Clone, Copy)]
pub enum SpeedAction {
    Start { longer: bool },
}

#[derive(Default)]
struct Progress {
    cancel: AtomicBool,
    transferred: AtomicU64,
    phase: Mutex<&'static str>,
}
impl Progress {
    fn stage(&self, value: &'static str) {
        if let Ok(mut phase) = self.phase.lock() {
            *phase = value;
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn check(&self) -> Result<(), String> {
        if self.cancelled() {
            Err("Speed test canceled.".into())
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug)]
struct Sample {
    bytes: u64,
    elapsed: Duration,
}

impl Sample {
    fn mbps(&self) -> f64 {
        self.bytes as f64 * 8.0 / self.elapsed.as_secs_f64().max(0.0001) / 1_000_000.0
    }
}

#[derive(Clone, Default, Debug)]
struct ResultRow {
    time: String,
    download: Option<Sample>,
    upload: Option<Sample>,
    latency: Option<f64>,
    jitter: Option<f64>,
    bytes: u64,
    longer: bool,
    warnings: Vec<String>,
}

#[derive(Default)]
pub struct SpeedState {
    job: Task<Result<ResultRow, String>>,
    progress: Option<Arc<Progress>>,
    started: Option<Instant>,
    result: Option<ResultRow>,
    history: Vec<ResultRow>,
    longer: bool,
    keep_history: bool,
    consent: bool,
    error: Option<String>,
}
impl SpeedState {
    pub fn busy(&self) -> bool {
        self.job.running()
    }
}

fn agent() -> Result<ureq::Agent, String> {
    Ok(crate::platform::https_agent()?
        .timeout_connect(Duration::from_secs(4))
        .timeout_read(Duration::from_secs(4))
        .timeout_write(Duration::from_secs(4))
        .user_agent(USER_AGENT)
        .build())
}

fn report_http(error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(status, _) => format!("Cloudflare returned HTTP {status}."),
        ureq::Error::Transport(error) => match error.kind() {
            ureq::ErrorKind::Dns => "Cloudflare's test address could not be resolved.".into(),
            ureq::ErrorKind::ConnectionFailed => "Could not connect securely to Cloudflare.".into(),
            ureq::ErrorKind::Io => "The connection timed out or closed during the transfer.".into(),
            _ => "The Cloudflare request could not be completed.".into(),
        },
    }
}

fn response(result: Result<ureq::Response, ureq::Error>) -> Result<ureq::Response, String> {
    let response = result.map_err(report_http)?;
    if response.status() != 200 {
        return Err(format!("Cloudflare returned HTTP {}.", response.status()));
    }
    Ok(response)
}

fn read_payload(
    mut reader: impl Read,
    expected: u64,
    p: &Progress,
    deadline: Instant,
) -> Result<u64, String> {
    let mut count = 0;
    let mut buf = [0; 64 * 1024];
    loop {
        p.check()?;
        if Instant::now() >= deadline {
            return Err("The transfer exceeded its time limit.".into());
        }
        // Read at most one byte beyond the advertised size to detect an
        // invalid response without accepting unbounded payloads.
        let limit = buf.len().min((expected + 1 - count) as usize);
        let n = reader
            .read(&mut buf[..limit])
            .map_err(|_| "The response ended during the download.")? as u64;
        if n == 0 {
            break;
        }
        p.transferred.fetch_add(n, Ordering::Relaxed);
        count += n;
        if count > expected {
            return Err("The server returned more data than requested.".into());
        }
    }
    if count != expected {
        return Err(format!("Received {count} of {expected} requested bytes."));
    }
    Ok(count)
}

struct SyntheticUpload<'a> {
    remaining: u64,
    progress: &'a Progress,
    deadline: Instant,
}
impl Read for SyntheticUpload<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.progress.cancelled() || Instant::now() >= self.deadline {
            return Err(io::Error::other("Upload stopped"));
        }
        let n = out.len().min(self.remaining as usize);
        out[..n].fill(b'0');
        self.remaining -= n as u64;
        self.progress
            .transferred
            .fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

fn sample(
    agent: &ureq::Agent,
    provider: &str,
    bytes: u64,
    upload: bool,
    progress: &Progress,
    limit: Duration,
) -> Result<Sample, String> {
    progress.check()?;
    let start = Instant::now();
    let deadline = start + limit;
    if upload {
        let body = SyntheticUpload {
            remaining: bytes,
            progress,
            deadline,
        };
        let reply = response(
            agent
                .post(&format!("{provider}/__up"))
                .timeout(limit)
                .set("Content-Type", "application/octet-stream")
                .set("Content-Length", &bytes.to_string())
                .send(body),
        )?;
        progress.check()?;
        // Server confirmation is part of upload timing. A truncated response
        // or failed upload cannot produce a successful measurement.
        let read = io::copy(
            &mut reply.into_reader().take(RESPONSE_LIMIT + 1),
            &mut io::sink(),
        )
        .map_err(|_| "The upload response could not be read.")?;
        if read > RESPONSE_LIMIT {
            return Err("The upload response was too large.".into());
        }
    } else {
        let reply = response(
            agent
                .get(&format!("{provider}/__down?bytes={bytes}"))
                .timeout(limit)
                .set("Cache-Control", "no-cache")
                .set("Accept-Encoding", "identity")
                .call(),
        )?;
        read_payload(reply.into_reader(), bytes, progress, deadline)?;
    }
    progress.check()?;
    Ok(Sample {
        bytes,
        elapsed: start.elapsed(),
    })
}

fn budget(longer: bool) -> u64 {
    if longer {
        96 * MIB
    } else {
        24 * MIB
    }
}

fn direction(
    agent: &ureq::Agent,
    provider: &str,
    longer: bool,
    upload: bool,
    progress: &Progress,
) -> Result<(Option<Sample>, Option<String>), String> {
    let deadline = Instant::now() + DIRECTION_TIME;
    let mut remaining = budget(longer);
    let mut bytes = 512 * 1024;
    let mut best = None;
    let mut warning = None;
    for _ in 0..6 {
        progress.check()?;
        let limit = deadline
            .saturating_duration_since(Instant::now())
            .min(SAMPLE_TIME);
        if limit < Duration::from_millis(300) || remaining < 512 * 1024 {
            break;
        }
        bytes = bytes.min(remaining).min(MAX_SAMPLE);
        remaining -= bytes;
        match sample(agent, provider, bytes, upload, progress, limit) {
            Ok(result) => {
                let reached_duration =
                    result.elapsed >= Duration::from_millis(if longer { 1800 } else { 950 });
                best = Some(result);
                if reached_duration || bytes == MAX_SAMPLE {
                    break;
                }
                bytes *= 4;
            }
            Err(error) => {
                progress.check()?;
                warning = Some(error);
                break;
            }
        }
    }
    Ok((best, warning))
}

fn median(mut samples: Vec<f64>) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    samples.sort_by(f64::total_cmp);
    let middle = samples.len() / 2;
    if samples.len() % 2 == 0 {
        Some((samples[middle - 1] + samples[middle]) / 2.0)
    } else {
        Some(samples[middle])
    }
}

fn run(longer: bool, p: Arc<Progress>) -> Result<ResultRow, String> {
    p.check()?;
    let agent = agent()?;
    p.stage("Connecting to Cloudflare");
    // Exclude the initial DNS/TLS handshake from idle latency samples.
    sample(&agent, PROVIDER, 0, false, &p, Duration::from_secs(7))
        .map_err(|error| format!("Could not start the speed test. {error}"))?;
    let mut idle = Vec::new();
    for _ in 0..4 {
        p.check()?;
        if let Ok(sample) = sample(&agent, PROVIDER, 0, false, &p, Duration::from_secs(2)) {
            idle.push(sample.elapsed.as_secs_f64() * 1000.0);
        } else {
            break;
        }
    }

    p.stage("Measuring download");
    let (download, down_error) = direction(&agent, PROVIDER, longer, false, &p)?;
    p.stage("Measuring upload");
    let (upload, up_error) = direction(&agent, PROVIDER, longer, true, &p)?;
    let warnings: Vec<_> = [
        ("Download", &download, down_error),
        ("Upload", &upload, up_error),
    ]
    .into_iter()
    .filter_map(|(label, sample, error)| {
        error.map(|e| {
            if sample.is_some() {
                format!("{label} uses the last completed sample. {e}")
            } else {
                format!("{label} unavailable. {e}")
            }
        })
    })
    .collect();
    if download.is_none() && upload.is_none() {
        return Err(if warnings.is_empty() {
            "Neither transfer completed.".into()
        } else {
            warnings.join(" ")
        });
    }
    let jitter = median(idle.windows(2).map(|s| (s[1] - s[0]).abs()).collect());
    Ok(ResultRow {
        time: chrono::Local::now().format("%b %-d, %H:%M").to_string(),
        download,
        upload,
        latency: median(idle),
        jitter,
        bytes: p.transferred.load(Ordering::Relaxed),
        longer,
        warnings,
    })
}

fn rate_text(sample: &Option<Sample>) -> String {
    sample
        .as_ref()
        .map(|s| format!("{:.1} Mbps", s.mbps()))
        .unwrap_or_else(|| "Unavailable".into())
}
fn ms_text(value: Option<f64>) -> String {
    value
        .map(|n| format!("{n:.1} ms"))
        .unwrap_or_else(|| "Unavailable".into())
}

impl App {
    pub fn poll_speed(&mut self, _ctx: &egui::Context) {
        if let Some(result) = self.speed.job.poll() {
            match result.and_then(|result| result) {
                Ok(row) => {
                    if self.speed.keep_history {
                        self.speed.history.insert(0, row.clone());
                        self.speed.history.truncate(20);
                    }
                    self.speed.result = Some(row);
                }
                Err(error) => self.speed.error = Some(error),
            }
        }
    }

    pub fn execute_speed(&mut self, action: SpeedAction, ctx: &egui::Context) {
        if self.speed.busy() || !self.speed.consent {
            return;
        }
        let SpeedAction::Start { longer } = action;
        self.speed.consent = false;
        self.speed.error = None;
        self.speed.started = Some(Instant::now());
        let progress = Arc::new(Progress::default());
        self.speed.progress = Some(progress.clone());
        self.speed.job = Task::spawn(ctx, move || run(longer, progress));
    }

    pub fn page_speed(&mut self, ui: &mut Ui) {
        let mut start = false;
        let busy = self.speed.busy();
        ScrollArea::vertical()
            .id_salt("internet_speed_page")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                widgets::page_header(
                    ui,
                    "Internet speed test",
                    "Measure download, upload, and latency on the current route.",
                );
                if let Some(row) = &self.speed.result {
                    render_result(ui, row);
                }
                widgets::card(ui, |ui| {
                    ui.label(widgets::strong("Cloudflare speed test", 18.0));
                    ui.label(widgets::dim(
                        "Cloudflare receives your IP address and synthetic test traffic.",
                    ));
                    ui.add_enabled_ui(!busy, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.radio_value(&mut self.speed.longer, false, "Quick · up to 48 MiB");
                            ui.radio_value(&mut self.speed.longer, true, "Longer · up to 192 MiB");
                        });
                        ui.checkbox(
                            &mut self.speed.keep_history,
                            "Keep results for this session",
                        );
                        ui.checkbox(
                            &mut self.speed.consent,
                            "I agree to the data usage for this run",
                        );
                    });
                    if busy {
                        if let Some(p) = &self.speed.progress {
                            let phase = p.phase.try_lock().map(|s| *s).unwrap_or("Measuring");
                            ui.horizontal_wrapped(|ui| {
                                ui.spinner();
                                ui.label(widgets::strong(phase, 16.0));
                            });
                            ui.label(widgets::dim(format!(
                                "{} payload processed · {}s elapsed",
                                util::fmt_bytes(p.transferred.load(Ordering::Relaxed)),
                                self.speed
                                    .started
                                    .map(|t| t.elapsed().as_secs())
                                    .unwrap_or(0)
                            )));
                            if ui
                                .add_enabled(
                                    !p.cancelled(),
                                    widgets::ghost(if p.cancelled() {
                                        "Stopping…"
                                    } else {
                                        "Cancel test"
                                    }),
                                )
                                .clicked()
                            {
                                p.cancel.store(true, Ordering::Relaxed);
                            }
                        }
                    } else if ui
                        .add_enabled(
                            self.speed.consent && !self.mutations_running(),
                            widgets::primary("Review and start speed test…"),
                        )
                        .clicked()
                    {
                        start = true;
                    }
                    if let Some(error) = &self.speed.error {
                        ui.colored_label(color_warning(), error);
                    }
                    ui.horizontal_wrapped(|ui| {
                        if ui.add(widgets::small("Provider privacy policy")).clicked() {
                            util::open_uri("https://www.cloudflare.com/privacypolicy/");
                        }
                        if ui.add(widgets::small("Open browser test")).clicked() {
                            util::open_uri(PROVIDER);
                        }
                    });
                });
                if !self.speed.history.is_empty() {
                    widgets::section(ui, "This session's history");
                    if ui.add(widgets::small("Clear history")).clicked() {
                        self.speed.history.clear();
                    }
                    for row in &self.speed.history {
                        widgets::tight_card(ui, |ui| {
                            ui.label(widgets::strong(
                                format!(
                                    "{} · ↓ {} · ↑ {}",
                                    row.time,
                                    rate_text(&row.download),
                                    rate_text(&row.upload)
                                ),
                                14.0,
                            ));
                        });
                    }
                }
            });
        if start {
            self.modal = Some(Modal {
                title: "Run internet speed test?".into(),
                body: "Send synthetic test traffic to Cloudflare over HTTPS. Cloudflare receives the public IP address of your current connection.".into(),
                details: vec![format!("Up to {} of payload plus HTTPS overhead", util::fmt_bytes(2 * budget(self.speed.longer)))],
                note: self.speed.longer.then(|| "Use Quick on a limited or metered connection.".into()),
                confirm: "Start test".into(), danger: false,
                action: Action::Speed(SpeedAction::Start { longer: self.speed.longer }),
            });
        }
    }
}

fn render_result(ui: &mut Ui, row: &ResultRow) {
    widgets::card(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            for (label, value) in [
                ("Download", rate_text(&row.download)),
                ("Upload", rate_text(&row.upload)),
                ("Idle HTTPS latency", ms_text(row.latency)),
                ("Latency variation", ms_text(row.jitter)),
            ] {
                ui.vertical(|ui| {
                    ui.set_min_width(126.0);
                    ui.label(widgets::dim(label));
                    ui.label(widgets::strong(value, 23.0));
                });
            }
        });
        ui.label(widgets::faint(format!(
            "{} · {} mode · {} test payload",
            row.time,
            if row.longer { "Longer" } else { "Quick" },
            util::fmt_bytes(row.bytes)
        )));
        for warning in &row.warnings {
            ui.colored_label(color_warning(), warning);
        }
        ui.label(widgets::faint("Estimates reflect one HTTPS connection and the current route. Wi-Fi, VPNs and other traffic can affect the result."));
    });
}

#[cfg(test)]
pub(crate) fn preview_fixture() -> SpeedState {
    SpeedState {
        result: Some(ResultRow {
            time: "Nov 19, 11:54".into(),
            download: Some(Sample {
                bytes: 20_000_000,
                elapsed: Duration::from_millis(376),
            }),
            upload: Some(Sample {
                bytes: 8_000_000,
                elapsed: Duration::from_millis(765),
            }),
            latency: Some(11.1),
            jitter: Some(1.4),
            bytes: 40 * MIB,
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts_download_bytes_rejects_short_and_oversize_replies() {
        for (sent, expected, ok) in [(8192, 8192, true), (8092, 8192, false), (8193, 8192, false)] {
            let progress = Progress::default();
            let read = read_payload(
                &vec![1; sent][..],
                expected,
                &progress,
                Instant::now() + Duration::from_secs(1),
            );
            assert_eq!(read.is_ok(), ok);
            assert_eq!(progress.transferred.load(Ordering::Relaxed), sent as u64);
        }
    }

    #[test]
    fn synthetic_upload_respects_length_and_cancellation() {
        let progress = Progress::default();
        let mut upload = SyntheticUpload {
            remaining: 31,
            progress: &progress,
            deadline: Instant::now() + Duration::from_secs(1),
        };
        let mut bytes = [0; 64];
        assert_eq!(upload.read(&mut bytes).unwrap(), 31);
        assert_eq!(&bytes[..31], &[b'0'; 31]);
        assert_eq!(upload.read(&mut bytes).unwrap(), 0);
        progress.cancel.store(true, Ordering::Relaxed);
        assert!(upload.read(&mut bytes).is_err());
    }

    #[test]
    #[ignore = "connects to Cloudflare with small synthetic payloads"]
    fn cloudflare_https_smoke() {
        let agent = agent().unwrap();
        let progress = Progress::default();
        let timeout = Duration::from_secs(10);
        let idle = sample(&agent, PROVIDER, 0, false, &progress, timeout).unwrap();
        let down = sample(&agent, PROVIDER, 8192, false, &progress, timeout).unwrap();
        let up = sample(&agent, PROVIDER, 8192, true, &progress, timeout).unwrap();
        assert_eq!((idle.bytes, down.bytes, up.bytes), (0, 8192, 8192));
        assert_eq!(progress.transferred.load(Ordering::Relaxed), 16_384);
    }

    #[test]
    fn canceled_run_never_contacts_provider_and_timing_math_is_correct() {
        let p = Arc::new(Progress::default());
        p.cancel.store(true, Ordering::Relaxed);
        assert!(run(false, p).unwrap_err().contains("canceled"));
        assert_eq!(
            Sample {
                bytes: 125_000_000,
                elapsed: Duration::from_secs(1)
            }
            .mbps(),
            1000.0
        );
        assert_eq!(median(vec![10.0, 4.0, 2.0, 6.0]), Some(5.0));
    }
}
