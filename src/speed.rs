//! User-initiated M-Lab NDT7 measurements. No device inventory or real files are
//! transmitted. Public data/IP disclosure is required for every run.
use crate::{
    app::{Action, App, Modal},
    task::Task,
    theme::*,
    util, widgets,
};
use eframe::egui::{self, ScrollArea, Ui};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::{self, Read},
    net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tungstenite::{
    client::IntoClientRequest, protocol::WebSocketConfig, stream::MaybeTlsStream, Error as WsError,
    Message, WebSocket,
};

const LOCATE: &str = "https://locate.measurementlab.net/v2/nearest/ndt/ndt7";
const SUBPROTOCOL: &str = "net.measurementlab.ndt.v7";
const MAX_LOCATOR_BYTES: usize = 128 * 1024;
const MAX_MESSAGE: usize = 1 << 24;
const TEST_TIME: Duration = Duration::from_secs(10);
const HARD_TIME: Duration = Duration::from_secs(13);
const USER_AGENT: &str = concat!("PCSweep/", env!("CARGO_PKG_VERSION"), " ndt7-client");
type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeedAction {
    Start { longer: bool },
}

#[derive(Clone, Deserialize)]
struct LocateServer {
    machine: String,
    #[serde(default)]
    urls: HashMap<String, String>,
    #[serde(default)]
    location: Location,
}
#[derive(Clone, Default, Deserialize)]
struct Location {
    #[serde(default)]
    city: String,
    #[serde(default)]
    country: String,
}
#[derive(Deserialize)]
struct LocateResponse {
    results: Vec<LocateServer>,
}

#[derive(Default)]
struct Progress {
    cancel: AtomicBool,
    transferred: AtomicU64,
    phase: Mutex<String>,
    mbps: Mutex<Option<f64>>,
}
impl Progress {
    fn stage(&self, text: &str) {
        if let Ok(mut value) = self.phase.lock() {
            *value = text.into();
        }
    }
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    fn check(&self) -> Result<(), String> {
        if self.cancelled() {
            Err(
                "Speed test canceled. Partial traffic may already have been recorded by M-Lab."
                    .into(),
            )
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct Direction {
    mbps: Option<f64>,
    seconds: f64,
    bytes: u64,
    capped: bool,
    warning: Option<String>,
    rtt_ms: Option<f64>,
    variation_ms: Option<f64>,
}
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
struct ResultRow {
    time: String,
    server: String,
    location: String,
    download: Direction,
    upload: Direction,
    idle_ms: Option<f64>,
    idle_jitter_ms: Option<f64>,
    bytes: u64,
    longer: bool,
    warning: Option<String>,
}

pub struct SpeedState {
    job: Task<Result<ResultRow, String>>,
    progress: Option<Arc<Progress>>,
    started: Option<Instant>,
    finished: Option<Instant>,
    result: Option<ResultRow>,
    history: Vec<ResultRow>,
    longer: bool,
    keep_history: bool,
    consent: bool,
    error: Option<String>,
    sessions: u8,
}
impl Default for SpeedState {
    fn default() -> Self {
        Self {
            job: Task::default(),
            progress: None,
            started: None,
            finished: None,
            result: None,
            history: Vec::new(),
            longer: false,
            keep_history: false,
            consent: false,
            error: None,
            sessions: 0,
        }
    }
}
impl SpeedState {
    pub fn busy(&self) -> bool {
        self.job.running()
    }
}

fn mbps(bytes: u64, seconds: f64) -> Option<f64> {
    if seconds <= 0.0 || !seconds.is_finite() || bytes == 0 {
        None
    } else {
        Some(bytes as f64 * 8.0 / seconds / 1_000_000.0)
    }
}
fn mean(samples: &[f64]) -> Option<f64> {
    if samples.is_empty() || samples.iter().any(|x| !x.is_finite() || *x < 0.0) {
        None
    } else {
        Some(samples.iter().sum::<f64>() / samples.len() as f64)
    }
}
fn jitter(samples: &[f64]) -> Option<f64> {
    if samples.len() < 2 {
        return None;
    }
    mean(
        &samples
            .windows(2)
            .map(|s| (s[1] - s[0]).abs())
            .collect::<Vec<_>>(),
    )
}

/// The locator chooses the host, but an untrusted response cannot redirect the
/// client to arbitrary LAN endpoints, cleartext sockets, ports or URL paths.
fn endpoint(value: &str, direction: &str) -> Result<tungstenite::http::Request<()>, String> {
    if value.len() > 8192 {
        return Err("M-Lab returned an oversized test URL.".into());
    }
    let mut request = value
        .into_client_request()
        .map_err(|_| "M-Lab returned an invalid test URL.")?;
    let uri = request.uri();
    let host = uri.host().unwrap_or("").to_ascii_lowercase();
    if uri.scheme_str() != Some("wss")
        || !host.ends_with(".measurement-lab.org")
        || host.contains('@')
        || uri.authority().is_none_or(|a| a.as_str().contains('@'))
        || uri.port_u16().is_some_and(|p| p != 443)
        || uri.path() != format!("/ndt/v7/{direction}")
        || !matches!(direction, "download" | "upload")
    {
        return Err(
            "M-Lab returned an unsupported test endpoint. No test traffic was sent.".into(),
        );
    }
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", SUBPROTOCOL.parse().unwrap());
    request
        .headers_mut()
        .insert("User-Agent", USER_AGENT.parse().unwrap());
    Ok(request)
}
fn locate() -> Result<Vec<LocateServer>, String> {
    let response = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(12))
        .redirects(0)
        .build()
        .get(LOCATE)
        .set("User-Agent", USER_AGENT)
        .call()
        .map_err(|_| {
            "M-Lab server selection is unavailable. Check your connection and try later."
        })?;
    let mut data = Vec::new();
    response
        .into_reader()
        .take((MAX_LOCATOR_BYTES + 1) as u64)
        .read_to_end(&mut data)
        .map_err(|_| "The M-Lab server list could not be read.")?;
    if data.len() > MAX_LOCATOR_BYTES {
        return Err("The M-Lab server list was too large.".into());
    }
    let result: LocateResponse =
        serde_json::from_slice(&data).map_err(|_| "M-Lab returned an unreadable server list.")?;
    let mut valid = Vec::new();
    for server in result.results.into_iter().take(4) {
        let download = server.urls.get("wss:///ndt/v7/download");
        let upload = server.urls.get("wss:///ndt/v7/upload");
        if let (Some(d), Some(u)) = (download, upload) {
            let d = endpoint(d, "download")?;
            let u = endpoint(u, "upload")?;
            if d.uri().host() != u.uri().host() {
                continue;
            }
            if server.machine.len() <= 256 {
                valid.push(server);
            }
        }
    }
    if valid.is_empty() {
        Err("M-Lab has no available compatible test server. Try later.".into())
    } else {
        Ok(valid)
    }
}
fn resolve(host: &str) -> Result<Vec<SocketAddr>, String> {
    let addresses = (host, 443)
        .to_socket_addrs()
        .map_err(|_| "The M-Lab test server could not be resolved.")?
        .take(8)
        .filter(|a| !a.ip().is_loopback() && !a.ip().is_unspecified() && !a.ip().is_multicast())
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        Err("M-Lab did not resolve to a usable address.".into())
    } else {
        Ok(addresses)
    }
}
fn tcp_connect(addresses: &[SocketAddr], p: &Progress) -> Result<TcpStream, String> {
    let start = Instant::now();
    for address in addresses {
        p.check()?;
        if start.elapsed() >= Duration::from_secs(8) {
            break;
        }
        if let Ok(stream) = TcpStream::connect_timeout(address, Duration::from_secs(2)) {
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .map_err(|e| e.to_string())?;
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .map_err(|e| e.to_string())?;
            let _ = stream.set_nodelay(true);
            return Ok(stream);
        }
    }
    Err("The M-Lab server could not be reached. A firewall, VPN or provider outage may affect the test.".into())
}
fn connect(
    url: &str,
    direction: &str,
    addresses: &[SocketAddr],
    p: &Progress,
) -> Result<Socket, String> {
    let request = endpoint(url, direction)?;
    let stream = tcp_connect(addresses, p)?;
    let config = WebSocketConfig::default()
        .write_buffer_size(0)
        .max_write_buffer_size(256 * 1024)
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE));
    let (socket, response) = tungstenite::client_tls_with_config(
        request,
        stream,
        Some(config),
        None,
    )
    .map_err(|_| {
        "M-Lab could not start a secure NDT7 test. Try later; no download URL or token was logged."
    })?;
    if response
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|s| s.to_str().ok())
        != Some(SUBPROTOCOL)
    {
        return Err("The server did not confirm the NDT7 test protocol.".into());
    }
    tcp(socket.get_ref())
        .set_nonblocking(true)
        .map_err(|e| e.to_string())?;
    Ok(socket)
}
fn tcp(stream: &MaybeTlsStream<TcpStream>) -> &TcpStream {
    match stream {
        MaybeTlsStream::Plain(s) => s,
        MaybeTlsStream::NativeTls(s) => s.get_ref(),
        _ => unreachable!("only native TLS is enabled"),
    }
}
fn would_block(error: &WsError) -> bool {
    matches!(error, WsError::Io(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted))
}

#[derive(Default)]
struct ServerMeasurement {
    bytes: u64,
    elapsed_us: u64,
    rtt: Vec<f64>,
    variation: Option<f64>,
}
impl ServerMeasurement {
    fn observe(&mut self, text: &str, direction: &str, sent: u64) {
        if text.len() > 64 * 1024 {
            return;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
            return;
        };
        if value["Origin"] != "server" || value["Test"] != direction {
            return;
        }
        if let (Some(bytes), Some(elapsed)) = (
            value["AppInfo"]["NumBytes"].as_u64(),
            value["AppInfo"]["ElapsedTime"].as_u64(),
        ) {
            if bytes <= sent
                && elapsed >= self.elapsed_us
                && elapsed <= 15_000_000
                && bytes >= self.bytes
            {
                self.bytes = bytes;
                self.elapsed_us = elapsed;
            }
        }
        if let Some(rtt) = value["TCPInfo"]["RTT"]
            .as_u64()
            .filter(|r| *r > 0 && *r < 60_000_000)
        {
            if self.rtt.len() < 150 {
                self.rtt.push(rtt as f64 / 1000.0);
            }
        }
        if let Some(rttvar) = value["TCPInfo"]["RTTVar"]
            .as_u64()
            .filter(|r| *r < 60_000_000)
        {
            self.variation = Some(rttvar as f64 / 1000.0);
        }
    }
}

fn transfer(mut socket: Socket, upload: bool, cap: u64, p: &Progress) -> Result<Direction, String> {
    let start = Instant::now();
    let direction = if upload { "upload" } else { "download" };
    let mut count = 0u64;
    let mut last_data = Duration::ZERO;
    let mut measurements = ServerMeasurement::default();
    let mut warning = None;
    let mut stopped_writing = None;
    let mut pending_flush = false;
    // Synthetic incompressible data only. This random payload never comes from a
    // user's files or clipboard. Windows' system-preferred RNG fills it once.
    let mut payload = vec![0u8; 64 * 1024];
    if upload {
        random_payload(&mut payload)?;
    }
    let result: Result<(), String> = (|| {
        loop {
            p.check()?;
            let elapsed = start.elapsed();
            if elapsed >= HARD_TIME {
                warning = Some("The test reached its time limit.".into());
                break;
            }
            if !upload && (count >= cap || elapsed >= TEST_TIME) {
                break;
            }
            if upload && stopped_writing.is_none() && (count >= cap || elapsed >= TEST_TIME) {
                stopped_writing = Some(Instant::now());
            }
            if stopped_writing.is_some_and(|t: Instant| t.elapsed() > Duration::from_secs(1)) {
                break;
            }
            let mut progressed = false;
            // Reading even during upload allows server measurements, pings and
            // close frames to be processed rather than backing up indefinitely.
            for _ in 0..8 {
                match socket.read() {
                    Ok(Message::Binary(bytes)) if !upload => {
                        if !(1024..=MAX_MESSAGE).contains(&bytes.len()) {
                            return Err("The NDT7 server sent an invalid measurement frame.".into());
                        }
                        count = count.saturating_add(bytes.len() as u64);
                        p.transferred
                            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                        last_data = start.elapsed();
                        progressed = true;
                        if count >= cap {
                            break;
                        }
                    }
                    Ok(Message::Binary(_)) => {
                        return Err("The upload server sent unexpected binary data.".into())
                    }
                    Ok(Message::Text(text)) => {
                        measurements.observe(
                            &text,
                            direction,
                            if upload { count } else { u64::MAX },
                        );
                        progressed = true;
                    }
                    Ok(Message::Close(_))
                    | Err(WsError::ConnectionClosed)
                    | Err(WsError::AlreadyClosed) => return Ok(()),
                    Ok(_) => {
                        progressed = true;
                    }
                    Err(e) if would_block(&e) => break,
                    Err(_) => {
                        warning =
                            Some("The server connection ended before a normal test close.".into());
                        return Ok(());
                    }
                }
            }
            if pending_flush {
                match socket.flush() {
                    Ok(()) => {
                        pending_flush = false;
                        progressed = true;
                    }
                    Err(e) if would_block(&e) => {}
                    Err(_) => {
                        warning = Some("The upload connection ended early.".into());
                        break;
                    }
                }
            }
            if upload && stopped_writing.is_none() && !pending_flush {
                let size = payload.len().min(cap.saturating_sub(count) as usize);
                if size >= 1024 {
                    match socket.send(Message::Binary(payload[..size].to_vec().into())) {
                        Ok(()) => {}
                        Err(e) if would_block(&e) => pending_flush = true,
                        Err(WsError::WriteBufferFull(_)) => {
                            pending_flush = true;
                            continue;
                        }
                        Err(_) => {
                            warning = Some("The upload connection ended early.".into());
                            break;
                        }
                    }
                    count += size as u64;
                    p.transferred.fetch_add(size as u64, Ordering::Relaxed);
                    last_data = start.elapsed();
                    progressed = true;
                } else {
                    stopped_writing = Some(Instant::now());
                }
            }
            if let Ok(mut live) = p.mbps.try_lock() {
                *live = if upload {
                    mbps(
                        measurements.bytes,
                        measurements.elapsed_us as f64 / 1_000_000.0,
                    )
                } else {
                    mbps(count, start.elapsed().as_secs_f64())
                };
            }
            if !progressed {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        Ok(())
    })();
    let _ = tcp(socket.get_ref()).shutdown(Shutdown::Both);
    result?;
    let seconds = if upload {
        measurements.elapsed_us as f64 / 1_000_000.0
    } else {
        last_data.as_secs_f64()
    };
    let rate = mbps(if upload { measurements.bytes } else { count }, seconds);
    if rate.is_none() {
        return Err(format!("No usable {direction} measurement was received. Partial traffic may still count toward your data allowance."));
    }
    Ok(Direction {
        mbps: rate,
        seconds,
        bytes: count,
        capped: count >= cap,
        warning,
        rtt_ms: mean(&measurements.rtt),
        variation_ms: measurements.variation,
    })
}

fn random_payload(bytes: &mut [u8]) -> Result<(), String> {
    #[link(name = "bcrypt")]
    extern "system" {
        fn BCryptGenRandom(handle: isize, buffer: *mut u8, length: u32, flags: u32) -> i32;
    }
    let result = unsafe { BCryptGenRandom(0, bytes.as_mut_ptr(), bytes.len() as u32, 2) };
    if result < 0 {
        Err("Windows could not generate the synthetic upload payload.".into())
    } else {
        Ok(())
    }
}
fn run(longer: bool, p: Arc<Progress>) -> Result<ResultRow, String> {
    p.stage("Choosing a nearby M-Lab server…");
    p.check()?;
    let servers = locate()?;
    let cap = if longer {
        512 * 1024 * 1024
    } else {
        64 * 1024 * 1024
    };
    let mut last_error = "M-Lab has no available server.".to_owned();
    for server in servers.into_iter().take(2) {
        p.check()?;
        let download_url = &server.urls["wss:///ndt/v7/download"];
        let upload_url = &server.urls["wss:///ndt/v7/upload"];
        let request = endpoint(download_url, "download")?;
        let host = request.uri().host().ok_or("Missing test host")?;
        p.stage("Connecting and measuring idle TCP latency…");
        let addresses = match resolve(host) {
            Ok(v) => v,
            Err(e) => {
                last_error = e;
                continue;
            }
        };
        let mut latency = Vec::new();
        for _ in 0..3 {
            let now = Instant::now();
            if let Ok(stream) = tcp_connect(&addresses, &p) {
                latency.push(now.elapsed().as_secs_f64() * 1000.0);
                let _ = stream.shutdown(Shutdown::Both);
            }
            p.check()?;
        }
        p.stage("Measuring download…");
        let socket = match connect(download_url, "download", &addresses, &p) {
            Ok(v) => v,
            Err(e) => {
                last_error = e;
                continue;
            }
        };
        let download = transfer(socket, false, cap, &p)?;
        p.check()?;
        p.stage("Measuring upload…");
        if let Ok(mut live) = p.mbps.lock() {
            *live = None;
        }
        let upload = match connect(upload_url, "upload", &addresses, &p)
            .and_then(|s| transfer(s, true, cap, &p))
        {
            Ok(v) => v,
            Err(e) => {
                p.check()?;
                return Ok(ResultRow {
                    time: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
                    server: host.to_owned(),
                    location: format!("{}, {}", server.location.city, server.location.country),
                    download,
                    upload: Direction::default(),
                    idle_ms: mean(&latency),
                    idle_jitter_ms: jitter(&latency),
                    bytes: p.transferred.load(Ordering::Relaxed),
                    longer,
                    warning: Some(format!("Download completed; upload unavailable: {e}")),
                });
            }
        };
        return Ok(ResultRow {
            time: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            server: host.to_owned(),
            location: format!("{}, {}", server.location.city, server.location.country),
            download,
            upload,
            idle_ms: mean(&latency),
            idle_jitter_ms: jitter(&latency),
            bytes: p.transferred.load(Ordering::Relaxed),
            longer,
            warning: None,
        });
    }
    Err(last_error)
}

fn rate_text(value: Option<f64>) -> String {
    value
        .map(|n| format!("{n:.1} Mbps"))
        .unwrap_or_else(|| "Unavailable".into())
}
fn ms_text(value: Option<f64>) -> String {
    value
        .map(|n| format!("{n:.1} ms"))
        .unwrap_or_else(|| "Not measured".into())
}
fn reserve_daily_test() -> Result<(), String> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject},
    };
    use winreg::{enums::*, RegKey};
    let name: Vec<u16> = "Local\\PCSweep.NdtBudget.v1"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle == 0 {
        return Err("The local speed-test budget is unavailable.".into());
    }
    let status = unsafe { WaitForSingleObject(handle, 0) };
    if status != 0 && status != 0x80 {
        unsafe { CloseHandle(handle) };
        return Err("Another speed test is reserving its data budget.".into());
    }
    let result = (|| {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(r"Software\PCSweep\SpeedTest")
            .map_err(|e| e.to_string())?;
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let old: Option<String> = match key.get_value("DailyBudgetV1") {
            Ok(v) => Some(v),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.to_string()),
        };
        let count = next_daily_count(old.as_deref(), &today)?;
        key.set_value("DailyBudgetV1", &format!("{today}|{count}"))
            .map_err(|e| e.to_string())?;
        Ok(())
    })();
    unsafe {
        ReleaseMutex(handle);
        CloseHandle(handle);
    }
    result
}
fn next_daily_count(old: Option<&str>, today: &str) -> Result<u32, String> {
    let count = match old {
        None => 0,
        Some(value) => {
            let (date, count) = value
                .split_once('|')
                .ok_or("The saved speed-test budget is unreadable.")?;
            chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
                .map_err(|_| "The saved speed-test date is invalid.")?;
            let n = count
                .parse::<u32>()
                .map_err(|_| "The saved speed-test budget is invalid.")?;
            if date == today {
                n
            } else {
                0
            }
        }
    };
    if count >= 20 {
        return Err("This account has reached 20 test runs for today (UTC). Other clients sharing your IP also count toward M-Lab's provider limits.".into());
    }
    Ok(count + 1)
}
impl App {
    pub fn poll_speed(&mut self, _ctx: &egui::Context) {
        if let Some(result) = self.speed.job.poll() {
            self.speed.finished = Some(Instant::now());
            match result.and_then(|r| r) {
                Ok(row) => {
                    util::log(format!("M-Lab speed test completed: download {}, upload {}. Results are route/server dependent.", rate_text(row.download.mbps), rate_text(row.upload.mbps)));
                    if self.speed.keep_history {
                        self.speed.history.insert(0, row.clone());
                        self.speed.history.truncate(20);
                    }
                    self.speed.result = Some(row);
                }
                Err(e) => self.speed.error = Some(e),
            }
        }
    }
    pub fn execute_speed(&mut self, action: SpeedAction, ctx: &egui::Context) {
        if self.speed.busy() || !self.speed.consent {
            return;
        }
        if self.speed.sessions >= 20
            || self
                .speed
                .finished
                .is_some_and(|t| t.elapsed() < Duration::from_secs(60))
        {
            self.speed.error = Some("Wait at least one minute between runs. M-Lab permits at most 40 interactive directional tests per day; a complete run uses two.".into());
            return;
        }
        let SpeedAction::Start { longer } = action;
        if let Err(e) = reserve_daily_test() {
            self.speed.error = Some(e);
            return;
        }
        self.speed.sessions += 1;
        self.speed.consent = false;
        self.speed.error = None;
        self.speed.result = None;
        self.speed.started = Some(Instant::now());
        let progress = Arc::new(Progress::default());
        self.speed.progress = Some(progress.clone());
        self.speed.job = Task::spawn(ctx, move || run(longer, progress));
    }
    pub fn page_speed(&mut self, ui: &mut Ui) {
        let mut start = false;
        let busy = self.speed.busy();
        ScrollArea::vertical().id_salt("internet_speed_page").auto_shrink([false, false]).show(ui, |ui| {
            widgets::page_header(ui, "Internet speed test", "Measure the current internet route with M-Lab NDT7. Adapter link speed and internet throughput are different.");
            widgets::banner(ui, WARN, widgets::ic::INFO, |ui| { ui.vertical(|ui| {
                ui.label("M-Lab publishes measurement data including your public IP address and test time, and retains it indefinitely.");
                ui.label(widgets::faint("This is optional. PCSweep sends synthetic test traffic and its app/version identifier, not your files or device inventory. A VPN changes the route and IP measured."));
            }); });
            ui.horizontal_wrapped(|ui| {
                if ui.add(widgets::small("PCSweep privacy details")).clicked() { util::open_uri("https://github.com/kashrtx/pcsweep/blob/main/PRIVACY.md"); }
                if ui.add(widgets::small("M-Lab privacy policy")).clicked() { util::open_uri("https://www.measurementlab.net/privacy/"); }
                if ui.add(widgets::small("Provider terms and limits")).clicked() { util::open_uri("https://www.measurementlab.net/aup/"); }
            });
            widgets::card(ui, |ui| {
                ui.label(widgets::strong("Test options", 17.0));
                ui.add_enabled_ui(!busy, |ui| {
                    ui.radio_value(&mut self.speed.longer, false, "Quick · up to about 128 MiB of test payload");
                    ui.radio_value(&mut self.speed.longer, true, "Longer · up to about 1 GiB of test payload");
                    ui.label(widgets::faint("Each direction runs for at most 10 seconds or its data cap. An incoming frame may add up to 16 MiB; protocol overhead also uses data. Avoid testing on a limited or metered plan unless you accept this usage."));
                    ui.checkbox(&mut self.speed.keep_history, "Keep the last 20 results in this PCSweep session");
                    ui.checkbox(&mut self.speed.consent, "I accept the data usage and M-Lab's public IP/result disclosure for this run");
                });
                if busy {
                    if let Some(p) = &self.speed.progress {
                        let phase = p.phase.try_lock().map(|s| s.clone()).unwrap_or_else(|_| "Measuring…".into());
                        ui.horizontal_wrapped(|ui| { ui.spinner(); ui.label(widgets::strong(phase, 16.0)); });
                        let amount = p.transferred.load(Ordering::Relaxed);
                        let live = p.mbps.try_lock().ok().and_then(|v| *v);
                        ui.label(format!("{} transferred · {} · {} seconds elapsed", util::fmt_bytes(amount), rate_text(live), self.speed.started.map(|t| t.elapsed().as_secs()).unwrap_or(0)));
                        if ui.add_enabled(!p.cancelled(), widgets::ghost(if p.cancelled() { "Stopping…" } else { "Cancel test" })).clicked() { p.cancel.store(true, Ordering::Relaxed); }
                        if p.cancelled() { ui.label(widgets::dim("Stopping the current connection. Name resolution or a secure handshake may take a few seconds to finish.")); }
                    }
                } else {
                    let ready = self.speed.consent && !self.mutations_running()
                        && self.speed.sessions < 20 && self.speed.finished.is_none_or(|t| t.elapsed() >= Duration::from_secs(60));
                    if ui.add_enabled(ready, widgets::primary("Review and start speed test…")).clicked() { start = true; }
                    if self.speed.finished.is_some_and(|t| t.elapsed() < Duration::from_secs(60)) { ui.label(widgets::faint("Allow one minute between runs.")); }
                    ui.label(widgets::faint("Run only when needed. M-Lab allows at most 40 directional tests per day; each run uses two. PCSweep limits this account to 20 runs per UTC day across restarts; other clients on your IP share the provider limit."));
                }
            });
            if let Some(e) = &self.speed.error { ui.colored_label(WARN, e); }
            if let Some(row) = &self.speed.result { render_result(ui, row); }
            if !self.speed.history.is_empty() {
                widgets::section(ui, "This session's history");
                ui.label(widgets::faint("Local to this open PCSweep session. Compare the same route, server and test mode; a single result does not prove an ISP or adapter fault."));
                if ui.add(widgets::small("Clear session history")).clicked() { self.speed.history.clear(); }
                for row in &self.speed.history { widgets::tight_frame().show(ui, |ui| {
                    ui.label(widgets::strong(format!("{} · ↓ {} · ↑ {}", row.time, rate_text(row.download.mbps), rate_text(row.upload.mbps)), 14.0));
                    ui.label(widgets::faint(&row.server));
                }); }
            }
        });
        if start {
            self.modal = Some(Modal { title: "Run this internet speed test?".into(),
                body: "M-Lab will measure synthetic download and upload traffic. Its public research dataset includes your IP address, measurements and test time, retained indefinitely.".into(),
                details: vec![if self.speed.longer { "Longer mode: approximately 1 GiB maximum payload, plus buffering/protocol overhead.".into() } else { "Quick mode: approximately 128 MiB maximum payload, plus buffering/protocol overhead.".into() }],
                note: Some("Close bandwidth-heavy downloads for a useful comparison. No files are uploaded. You can cancel; traffic and measurements already sent cannot be recalled.".into()),
                confirm: "Accept and run test".into(), danger: false,
                action: Action::Speed(SpeedAction::Start { longer: self.speed.longer }) });
        }
    }
}
fn render_result(ui: &mut Ui, row: &ResultRow) {
    widgets::section(ui, "Measured result");
    widgets::card(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(widgets::strong(
                format!("Download {}", rate_text(row.download.mbps)),
                22.0,
            ));
            ui.label(widgets::strong(
                format!("Upload {}", rate_text(row.upload.mbps)),
                22.0,
            ));
        });
        ui.label(widgets::dim(format!(
            "{} · {} · {}",
            row.server, row.location, row.time
        )));
        ui.label(widgets::dim(format!(
            "Idle TCP connect latency: {} · connect jitter estimate: {}",
            ms_text(row.idle_ms),
            ms_text(row.idle_jitter_ms)
        )));
        ui.label(widgets::dim(format!(
            "Loaded server TCP RTT: {} · TCP RTT variation: {}",
            ms_text(row.download.rtt_ms),
            ms_text(row.download.variation_ms)
        )));
        ui.label(widgets::faint("Idle latency/jitter use three TCP connects to the test server. Loaded RTT/variation are server TCP estimates during download, not a browser ping or packet-loss measurement. Packet loss is not measured."));
        ui.label(widgets::faint(format!(
            "{} synthetic payload transferred · {}",
            util::fmt_bytes(row.bytes),
            if row.longer {
                "Longer mode"
            } else {
                "Quick mode"
            }
        )));
        if row.download.capped || row.upload.capped {
            ui.colored_label(WARN, "A data cap shortened the sample. This estimate may not represent sustained throughput, especially on fast connections.");
        }
        if let Some(e) = &row.warning {
            ui.colored_label(WARN, e);
        }
        for value in [&row.download, &row.upload] {
            if let Some(e) = &value.warning {
                ui.label(widgets::dim(e));
            }
        }
        ui.label(widgets::faint("NDT7 measures one TCP connection's application throughput. Wi-Fi, VPNs, competing traffic, your PC, the server and the route can limit the result. It is not guaranteed line capacity."));
    });
}

#[cfg(test)]
pub(crate) fn preview_fixture() -> SpeedState {
    SpeedState {
        result: Some(ResultRow {
            time: "2026-11-15 12:30:00".into(),
            server: "M-Lab preview server".into(),
            location: "Toronto, CA".into(),
            download: Direction {
                mbps: Some(425.2),
                capped: true,
                rtt_ms: Some(16.4),
                variation_ms: Some(2.3),
                ..Default::default()
            },
            upload: Direction {
                mbps: Some(83.7),
                ..Default::default()
            },
            idle_ms: Some(11.1),
            idle_jitter_ms: Some(1.4),
            bytes: 128 * 1024 * 1024,
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn budget_survives_restarts_and_rejects_excess_or_invalid_records() {
        assert_eq!(next_daily_count(None, "2026-11-15").unwrap(), 1);
        assert_eq!(
            next_daily_count(Some("2026-11-15|19"), "2026-11-15").unwrap(),
            20
        );
        assert!(next_daily_count(Some("2026-11-15|20"), "2026-11-15").is_err());
        assert_eq!(
            next_daily_count(Some("2026-11-14|20"), "2026-11-15").unwrap(),
            1
        );
        assert!(next_daily_count(Some("bad|3"), "2026-11-15").is_err());
    }

    #[test]
    fn measurement_math_distinguishes_megabits_and_missing_samples() {
        assert_eq!(mbps(125_000_000, 1.0), Some(1000.0));
        assert_eq!(mbps(1, 0.0), None);
        assert_eq!(mbps(1, f64::NAN), None);
        assert_eq!(jitter(&[10.0, 12.0, 9.0]), Some(2.5));
        assert_eq!(jitter(&[10.0]), None);
    }
    #[test]
    fn locator_endpoints_cannot_escape_provider_or_test_protocol() {
        let valid = "wss://ndt-mlab1-yyz01.mlab-oti.measurement-lab.org/ndt/v7/download?access_token=example";
        assert!(endpoint(valid, "download").is_ok());
        for bad in [
            "ws://ndt.measurement-lab.org/ndt/v7/download",
            "wss://measurement-lab.org.attacker.test/ndt/v7/download",
            "wss://127.0.0.1/ndt/v7/download",
            "wss://ndt.measurement-lab.org:444/ndt/v7/download",
            "wss://user@ndt.measurement-lab.org/ndt/v7/download",
            "wss://ndt.measurement-lab.org/unexpected",
        ] {
            assert!(endpoint(bad, "download").is_err(), "{bad}");
        }
    }
    #[test]
    fn upload_uses_server_received_bytes_and_rejects_wrong_or_impossible_reports() {
        let mut m = ServerMeasurement::default();
        m.observe(r#"{"Origin":"server","Test":"upload","AppInfo":{"NumBytes":1250000,"ElapsedTime":1000000}}"#, "upload", 2_000_000);
        assert_eq!(mbps(m.bytes, m.elapsed_us as f64 / 1e6), Some(10.0));
        m.observe(r#"{"Origin":"server","Test":"upload","AppInfo":{"NumBytes":999999999,"ElapsedTime":2000000}}"#, "upload", 2_000_000);
        assert_eq!(m.bytes, 1_250_000);
        m.observe(r#"{"Origin":"server","Test":"download","AppInfo":{"NumBytes":1500000,"ElapsedTime":3000000}}"#, "upload", 2_000_000);
        assert_eq!(m.bytes, 1_250_000);
    }
    #[test]
    fn cancellation_is_checked_before_network_work() {
        let p = Arc::new(Progress::default());
        p.cancel.store(true, Ordering::Relaxed);
        assert!(run(false, p).unwrap_err().contains("canceled"));
    }
    #[test]
    fn local_websocket_download_counts_payload_and_stays_bounded() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            for _ in 0..4 {
                if socket
                    .send(Message::Binary(vec![7u8; 8192].into()))
                    .is_err()
                {
                    break;
                }
            }
            let _ = socket.close(None);
        });
        let stream = TcpStream::connect(addr).unwrap();
        stream.set_nonblocking(false).unwrap();
        let (socket, _) = tungstenite::client(
            format!("ws://{addr}/").into_client_request().unwrap(),
            MaybeTlsStream::Plain(stream),
        )
        .unwrap();
        tcp(socket.get_ref()).set_nonblocking(true).unwrap();
        let p = Progress::default();
        let result = transfer(socket, false, 16 * 1024, &p).unwrap();
        assert!(result.capped);
        assert_eq!(result.bytes, 16 * 1024);
        assert!(result.mbps.unwrap() > 0.0);
        server.join().unwrap();
    }
}
