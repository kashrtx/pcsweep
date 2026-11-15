//! Read-only connection health. Link negotiation and adapter counters are kept
//! separate from optional internet reachability checks; no adapter is reconfigured.
use crate::app::{Action, App, Modal, ToastKind};
use crate::platform;
use crate::task::Task;
use crate::theme::*;
use crate::util;
use crate::widgets::{self, ic};
use eframe::egui::{self, Align, Label, Layout, RichText, ScrollArea, Ui};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

// All names are data, never command text. Read each inventory once and join
// literal identities locally, including aliases containing wildcard characters.
const ADAPTER_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$WarningPreference = 'SilentlyContinue'
$profiles = @{}
try { Get-NetConnectionProfile -ErrorAction Stop | ForEach-Object { $profiles[[string]$_.InterfaceIndex] = [string]$_.NetworkCategory } } catch { }
$configs = @{}
try { Get-NetIPConfiguration -All -ErrorAction Stop | ForEach-Object { $configs[[string]$_.InterfaceIndex] = $_ } } catch { }
$domain = $null
try { $domain = [bool](Get-CimInstance Win32_ComputerSystem -Property PartOfDomain -ErrorAction Stop).PartOfDomain } catch { }
$statistics = @{}
$statisticsError = $null
try { Get-NetAdapterStatistics -IncludeHidden -ErrorAction Stop | ForEach-Object { $statistics[[string]$_.Name] = $_ } } catch { $statisticsError = $_.Exception.Message }
$rows = @(Get-NetAdapter -IncludeHidden -ErrorAction Stop | ForEach-Object {
    $a = $_
    $config = $configs[[string]$a.ifIndex]
    $stats = $statistics[[string]$a.Name]
    $statsError = $statisticsError
    if ($null -eq $stats -and $null -eq $statsError) { $statsError = 'Windows did not publish counters for this adapter.' }
    $rx = [uint64]$a.ReceiveLinkSpeed
    $tx = [uint64]$a.TransmitLinkSpeed
    $rate = $rx
    if ($rate -eq 0 -or ($tx -gt 0 -and $tx -lt $rate)) { $rate = $tx }
    [pscustomobject]@{
        guid=([string]$a.InterfaceGuid).Trim('{}').ToLowerInvariant(); index=[uint32]$a.ifIndex;
        name=[string]$a.Name; description=[string]$a.InterfaceDescription; status=[string]$a.Status;
        physical=[bool]$a.HardwareInterface; hardware=[bool]$a.HardwareInterface; virtual_adapter=[bool]$a.Virtual;
        interface_type=[uint32]$a.ifType; physical_medium=[uint32]$a.NdisPhysicalMedium;
        mac=[string]$a.MacAddress; pnp_device_id=[string]$a.PnPDeviceID;
        link_bps=$rate; receive_link_bps=$rx; transmit_link_bps=$tx;
        driver_version=[string]$a.DriverVersionString; driver_date=[string]$a.DriverDate; driver_provider=[string]$a.DriverProvider;
        ipv4_gateways=@($config.IPv4DefaultGateway | Where-Object { $null -ne $_ } | ForEach-Object { [string]$_.NextHop });
        ipv6_gateways=@($config.IPv6DefaultGateway | Where-Object { $null -ne $_ } | ForEach-Object { [string]$_.NextHop });
        ipv4_addresses=@($config.IPv4Address | Where-Object { $null -ne $_ } | ForEach-Object { [string]$_.IPAddress });
        ipv6_addresses=@($config.IPv6Address | Where-Object { $null -ne $_ } | ForEach-Object { [string]$_.IPAddress });
        domain_authenticated=($profiles[[string]$a.ifIndex] -eq 'DomainAuthenticated'); domain_member=$domain; remote_session=$false;
        received_bytes=[uint64]$stats.ReceivedBytes; sent_bytes=[uint64]$stats.SentBytes;
        received_errors=[uint64]$stats.ReceivedPacketErrors; sent_errors=[uint64]$stats.OutboundPacketErrors;
        received_discards=[uint64]$stats.ReceivedDiscardedPackets; sent_discards=[uint64]$stats.OutboundDiscardedPackets;
        statistics_error=$statsError
    }
})
ConvertTo-Json -InputObject $rows -Depth 5 -Compress
"#;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct AdapterInfo {
    pub guid: String,
    pub index: u32,
    pub name: String,
    pub description: String,
    pub status: String,
    pub physical: bool,
    pub hardware: bool,
    pub virtual_adapter: bool,
    pub interface_type: u32,
    pub physical_medium: u32,
    pub mac: String,
    pub pnp_device_id: String,
    pub link_bps: u64,
    pub receive_link_bps: u64,
    pub transmit_link_bps: u64,
    pub driver_version: String,
    pub driver_date: String,
    pub driver_provider: String,
    pub ipv4_gateways: Vec<String>,
    pub ipv6_gateways: Vec<String>,
    pub ipv4_addresses: Vec<String>,
    pub ipv6_addresses: Vec<String>,
    pub domain_authenticated: bool,
    pub domain_member: Option<bool>,
    pub remote_session: bool,
    pub received_bytes: u64,
    pub sent_bytes: u64,
    pub received_errors: u64,
    pub sent_errors: u64,
    pub received_discards: u64,
    pub sent_discards: u64,
    pub statistics_error: Option<String>,
    pub wifi_signal: Option<u32>,
    pub wifi_receive_bps: Option<u64>,
    pub wifi_transmit_bps: Option<u64>,
    pub wifi_error: Option<String>,
}
impl AdapterInfo {
    pub fn connected(&self) -> bool {
        self.status.eq_ignore_ascii_case("Up")
    }
    pub fn is_physical(&self) -> bool {
        self.physical && self.hardware && !self.virtual_adapter
    }
    fn wireless(&self) -> bool {
        self.interface_type == 71 || matches!(self.physical_medium, 1 | 9)
    }
}

fn guid(value: &str) -> Option<windows_sys::core::GUID> {
    let plain = value.trim_matches(['{', '}']).replace('-', "");
    if plain.len() != 32 || !plain.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut last = [0; 8];
    for (i, b) in last.iter_mut().enumerate() {
        *b = u8::from_str_radix(&plain[16 + i * 2..18 + i * 2], 16).ok()?;
    }
    Some(windows_sys::core::GUID {
        data1: u32::from_str_radix(&plain[..8], 16).ok()?,
        data2: u16::from_str_radix(&plain[8..12], 16).ok()?,
        data3: u16::from_str_radix(&plain[12..16], 16).ok()?,
        data4: last,
    })
}

// WLAN_CONNECTION_ATTRIBUTES / WLAN_ASSOCIATION_ATTRIBUTES from wlanapi.h.
// Signal is a driver's 0..100 quality estimate, not a bandwidth measurement.
#[repr(C)]
struct Ssid {
    len: u32,
    bytes: [u8; 32],
}
#[repr(C)]
struct Association {
    ssid: Ssid,
    bss_type: i32,
    bssid: [u8; 6],
    phy_type: i32,
    phy_index: u32,
    signal: u32,
    receive_kbps: u32,
    transmit_kbps: u32,
}
#[repr(C)]
struct ConnectionAttributes {
    state: i32,
    mode: i32,
    profile_name: [u16; 256],
    association: Association,
    security_enabled: i32,
    one_x_enabled: i32,
    auth_algorithm: i32,
    cipher_algorithm: i32,
}
#[link(name = "wlanapi")]
extern "system" {
    fn WlanOpenHandle(
        version: u32,
        reserved: *mut std::ffi::c_void,
        negotiated: *mut u32,
        handle: *mut isize,
    ) -> u32;
    fn WlanQueryInterface(
        handle: isize,
        interface: *const windows_sys::core::GUID,
        opcode: i32,
        reserved: *mut std::ffi::c_void,
        size: *mut u32,
        data: *mut *mut std::ffi::c_void,
        value_type: *mut i32,
    ) -> u32;
    fn WlanFreeMemory(data: *mut std::ffi::c_void);
    fn WlanCloseHandle(handle: isize, reserved: *mut std::ffi::c_void) -> u32;
}
#[link(name = "user32")]
extern "system" {
    fn GetSystemMetrics(index: i32) -> i32;
}

fn remote_session() -> bool {
    (unsafe { GetSystemMetrics(0x1000) != 0 })
        || std::env::var("SESSIONNAME").is_ok_and(|v| v.to_ascii_uppercase().starts_with("RDP"))
}

fn wifi_details(adapter: &mut AdapterInfo) {
    if !adapter.wireless() || !adapter.connected() {
        return;
    }
    let Some(interface) = guid(&adapter.guid) else {
        return;
    };
    // Native Wi-Fi may deny this read when Windows location/privacy controls
    // restrict connection information. Keep those controls unchanged.
    let result: Result<(u32, u64, u64), String> = unsafe {
        let mut version = 0;
        let mut handle = 0;
        let opened = WlanOpenHandle(2, std::ptr::null_mut(), &mut version, &mut handle);
        if opened != 0 {
            Err(format!(
                "Windows Wi-Fi service did not provide signal information (code {opened})."
            ))
        } else {
            let mut size = 0;
            let mut data = std::ptr::null_mut();
            let mut kind = 0;
            // wlan_intf_opcode_current_connection = 7, a documented read-only query.
            let status = WlanQueryInterface(
                handle,
                &interface,
                7,
                std::ptr::null_mut(),
                &mut size,
                &mut data,
                &mut kind,
            );
            let result = if status == 0
                && !data.is_null()
                && size as usize >= std::mem::size_of::<ConnectionAttributes>()
            {
                let info = &*(data as *const ConnectionAttributes);
                Ok((
                    info.association.signal.min(100),
                    u64::from(info.association.receive_kbps) * 1000,
                    u64::from(info.association.transmit_kbps) * 1000,
                ))
            } else {
                Err(format!("Windows did not return Wi-Fi signal information (code {status}). Location/privacy settings or the driver may limit this read."))
            };
            if !data.is_null() {
                WlanFreeMemory(data);
            }
            WlanCloseHandle(handle, std::ptr::null_mut());
            result
        }
    };
    match result {
        Ok((signal, receive, transmit)) => {
            adapter.wifi_signal = Some(signal);
            adapter.wifi_receive_bps = Some(receive);
            adapter.wifi_transmit_bps = Some(transmit);
        }
        Err(error) => adapter.wifi_error = Some(error),
    }
}

fn parse_adapters(text: &str) -> Result<Vec<AdapterInfo>, String> {
    let mut rows: Vec<AdapterInfo> =
        serde_json::from_str(text.trim_start_matches('\u{feff}').trim())
            .map_err(|e| format!("Windows returned unreadable adapter details: {e}"))?;
    if rows
        .iter()
        .any(|a| guid(&a.guid).is_none() || a.index == 0 || a.name.is_empty())
    {
        return Err("Windows returned an incomplete adapter identity. Refresh before reviewing adapter settings.".into());
    }
    rows.sort_by_key(|a| (!a.connected(), !a.is_physical(), a.name.to_lowercase()));
    Ok(rows)
}

/// Read all visible and hidden NetAdapter instances. DNS review uses virtual and
/// VPN rows as guards; the Connection health page emphasizes physical adapters.
pub fn query_adapters() -> Result<Vec<AdapterInfo>, String> {
    let mut rows = parse_adapters(&platform::powershell(
        ADAPTER_SCRIPT,
        Duration::from_secs(40),
    )?)?;
    let remote = remote_session();
    for adapter in &mut rows {
        adapter.remote_session = remote;
        wifi_details(adapter);
    }
    Ok(rows)
}

#[derive(Clone, Deserialize)]
struct AdapterOption {
    name: String,
    value: String,
    keyword: String,
    supported: Vec<String>,
}
fn option_script(adapter: &AdapterInfo) -> Result<String, String> {
    if guid(&adapter.guid).is_none() {
        return Err("Invalid adapter identity".into());
    }
    Ok(format!(
        r#"
$all = @(Get-NetAdapter -IncludeHidden -ErrorAction Stop | Where-Object {{ ([string]$_.InterfaceGuid).Trim('{{}}') -eq {guid} }})
if ($all.Count -ne 1) {{ throw 'This adapter changed or is unavailable. Refresh connection health.' }}
$a = $all[0]
$escaped = [System.Management.Automation.WildcardPattern]::Escape([string]$a.Name)
$rows = @(Get-NetAdapterAdvancedProperty -Name $escaped -ErrorAction Stop | Where-Object {{ $_.Name -ceq $a.Name }} | ForEach-Object {{
    [pscustomobject]@{{ name=[string]$_.DisplayName; value=[string]$_.DisplayValue; keyword=[string]$_.RegistryKeyword; supported=@($_.ValidDisplayValues | ForEach-Object {{ [string]$_ }}) }}
}})
ConvertTo-Json -InputObject $rows -Depth 4 -Compress
"#,
        guid = platform::ps_quote(&adapter.guid)
    ))
}
fn query_options(adapter: AdapterInfo) -> Result<(String, Vec<AdapterOption>), String> {
    let text = platform::powershell(&option_script(&adapter)?, Duration::from_secs(25))?;
    let rows = serde_json::from_str(&text)
        .map_err(|e| format!("Windows returned unreadable adapter options: {e}"))?;
    Ok((adapter.guid, rows))
}

#[derive(Clone, Deserialize, Debug)]
struct ProbeResult {
    name: String,
    outcome: String,
    elapsed_ms: Option<u64>,
    detail: String,
}
fn probe_script(adapter: &AdapterInfo) -> String {
    let gateway = adapter
        .ipv4_gateways
        .iter()
        .chain(adapter.ipv6_gateways.iter())
        .find(|g| g.parse::<IpAddr>().is_ok())
        .map(String::as_str)
        .unwrap_or("");
    format!(
        r#"
$results = New-Object 'System.Collections.Generic.List[object]'
$watch = [Diagnostics.Stopwatch]::StartNew()
try {{
    $dns = [Net.Dns]::BeginGetHostAddresses('www.microsoft.com', $null, $null)
    if (-not $dns.AsyncWaitHandle.WaitOne(5000)) {{ throw 'DNS lookup did not finish within five seconds.' }}
    $addresses = [Net.Dns]::EndGetHostAddresses($dns)
    if ($addresses.Count -eq 0) {{ throw 'The resolver returned no addresses.' }}
    $results.Add([pscustomobject]@{{name='System DNS'; outcome='Resolved'; elapsed_ms=[uint64]$watch.ElapsedMilliseconds; detail='Windows resolved www.microsoft.com using the current resolver.'}})
}} catch {{ $results.Add([pscustomobject]@{{name='System DNS'; outcome='Not confirmed'; elapsed_ms=$null; detail=$_.Exception.Message}}) }}
$watch.Restart()
$response = $null
try {{
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    $request = [Net.HttpWebRequest]::Create('https://www.microsoft.com/')
    $request.Method = 'HEAD'; $request.Timeout = 5000; $request.ReadWriteTimeout = 5000
    $request.MaximumAutomaticRedirections = 3; $request.MaximumResponseHeadersLength = 32
    $request.UserAgent = 'PCSweep connection check'; $request.UseDefaultCredentials = $false
    $response = $request.GetResponse()
    $results.Add([pscustomobject]@{{name='HTTPS reachability'; outcome='Server responded'; elapsed_ms=[uint64]$watch.ElapsedMilliseconds; detail=('www.microsoft.com returned HTTP ' + [int]$response.StatusCode + '. This is a small HEAD request, not a speed test.')}})
}} catch {{
    if ($null -ne $_.Exception.Response) {{
        $response = $_.Exception.Response
        $results.Add([pscustomobject]@{{name='HTTPS reachability'; outcome='Server responded'; elapsed_ms=[uint64]$watch.ElapsedMilliseconds; detail=('The server returned HTTP ' + [int]$response.StatusCode + '; access may be restricted.')}})
    }} else {{ $results.Add([pscustomobject]@{{name='HTTPS reachability'; outcome='Not confirmed'; elapsed_ms=$null; detail=$_.Exception.Message}}) }}
}} finally {{ if ($null -ne $response) {{ $response.Close() }} }}
$gateway = {gateway}
if ($gateway) {{
    $ping = New-Object Net.NetworkInformation.Ping
    try {{
        $reply = $ping.Send($gateway, 1500)
        $ok = $reply.Status -eq [Net.NetworkInformation.IPStatus]::Success
        $results.Add([pscustomobject]@{{name='Gateway ICMP'; outcome=$(if ($ok) {{ 'Reply received' }} else {{ 'No reply' }}); elapsed_ms=$(if ($ok) {{ [uint64]$reply.RoundtripTime }} else {{ $null }}); detail=$(if ($ok) {{ 'The selected adapter gateway answered one ICMP request.' }} else {{ 'Gateways may block ICMP. A missing reply does not mean the connection is broken.' }})}})
    }} catch {{ $results.Add([pscustomobject]@{{name='Gateway ICMP'; outcome='Not confirmed'; elapsed_ms=$null; detail='The gateway did not provide a ping result. ICMP may be restricted.'}}) }}
    finally {{ $ping.Dispose() }}
}} else {{ $results.Add([pscustomobject]@{{name='Gateway ICMP'; outcome='Not available'; elapsed_ms=$null; detail='No default gateway was reported for this adapter.'}}) }}
ConvertTo-Json -InputObject @($results.ToArray()) -Depth 4 -Compress
"#,
        gateway = platform::ps_quote(gateway)
    )
}
fn run_probes(adapter: AdapterInfo) -> Result<Vec<ProbeResult>, String> {
    let text = platform::powershell(&probe_script(&adapter), Duration::from_secs(25))?;
    serde_json::from_str(&text)
        .map_err(|e| format!("Windows returned unreadable connection checks: {e}"))
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct CounterDelta {
    errors: u64,
    discards: u64,
    reset: bool,
}
fn counter_delta(previous: &AdapterInfo, current: &AdapterInfo) -> Option<CounterDelta> {
    if previous.guid != current.guid
        || previous.statistics_error.is_some()
        || current.statistics_error.is_some()
    {
        return None;
    }
    let old = [
        previous.received_errors,
        previous.sent_errors,
        previous.received_discards,
        previous.sent_discards,
    ];
    let new = [
        current.received_errors,
        current.sent_errors,
        current.received_discards,
        current.sent_discards,
    ];
    if old.iter().zip(new.iter()).any(|(a, b)| b < a) {
        return Some(CounterDelta {
            reset: true,
            ..Default::default()
        });
    }
    Some(CounterDelta {
        errors: new[0]
            .saturating_sub(old[0])
            .saturating_add(new[1].saturating_sub(old[1])),
        discards: new[2]
            .saturating_sub(old[2])
            .saturating_add(new[3].saturating_sub(old[3])),
        reset: false,
    })
}

struct Finding {
    warning: bool,
    title: &'static str,
    detail: String,
}
fn findings(adapter: &AdapterInfo, delta: Option<CounterDelta>) -> Vec<Finding> {
    let mut rows = Vec::new();
    if !adapter.connected() {
        rows.push(Finding { warning: false, title: "No active link on this adapter", detail: "This is normal for an unplugged port or unused adapter. Windows may be using a different connection.".into() });
        return rows;
    }
    if adapter.interface_type == 6 && adapter.link_bps > 0 && adapter.link_bps <= 100_000_000 {
        rows.push(Finding { warning: true, title: "Ethernet negotiated at 100 Mbps or below", detail: "If you expect gigabit, check the cable, both ports and their supported speeds. A 100 Mbps device or port may be working as designed.".into() });
    }
    if let Some(signal) = adapter.wifi_signal {
        if signal < 40 {
            rows.push(Finding { warning: true, title: "Weak reported Wi-Fi signal", detail: format!("The driver reports {signal}% quality. Try a closer position or clearer path to the access point, then compare again. Signal quality is not internet speed.") });
        } else if signal < 60 {
            rows.push(Finding { warning: false, title: "Room to improve Wi-Fi signal", detail: format!("The driver reports {signal}% quality. Distance, obstructions and interference can affect performance.") });
        }
    }
    if let Some(delta) = delta {
        if delta.reset {
            rows.push(Finding { warning: false, title: "Adapter counters restarted", detail: "Windows reset one or more counters. Wait for a fresh comparison before judging packet errors.".into() });
        }
        if delta.errors > 0 {
            rows.push(Finding { warning: true, title: "Packet error count increased", detail: format!("{} additional errors since the previous sample. Repeat under normal use; then check cable/signal and the adapter manufacturer's driver if errors keep growing.", delta.errors) });
        }
        if delta.discards > 0 {
            rows.push(Finding { warning: false, title: "Packet discard count increased", detail: format!("{} additional discards since the previous sample. Congestion, filtering or buffer pressure can cause discards; this alone does not establish a faulty adapter.", delta.discards) });
        }
    }
    if adapter.ipv4_gateways.is_empty() && adapter.ipv6_gateways.is_empty() {
        rows.push(Finding { warning: false, title: "No default gateway reported for this adapter", detail: "This adapter may serve a local network only. VPNs or another Windows route can still provide internet access.".into() });
    }
    rows
}
fn rate(bps: u64) -> String {
    if bps == 0 {
        "Not reported".into()
    } else if bps >= 1_000_000_000 {
        format!("{:.1} Gbps", bps as f64 / 1_000_000_000.0)
    } else if bps >= 1_000_000 {
        format!("{:.0} Mbps", bps as f64 / 1_000_000.0)
    } else {
        format!("{:.0} Kbps", bps as f64 / 1_000.0)
    }
}

#[derive(Clone)]
pub enum NetworkAction {
    RunChecks(AdapterInfo),
}
#[derive(Default)]
pub struct NetworkState {
    load: Task<Result<Vec<AdapterInfo>, String>>,
    probes: Task<Result<Vec<ProbeResult>, String>>,
    options_load: Task<Result<(String, Vec<AdapterOption>), String>>,
    adapters: Vec<AdapterInfo>,
    previous: HashMap<String, AdapterInfo>,
    selected: Option<String>,
    show_virtual: bool,
    refreshed: Option<Instant>,
    error: Option<String>,
    options: Option<(String, Vec<AdapterOption>)>,
    options_error: Option<String>,
    probe_rows: Vec<ProbeResult>,
    probe_for: Option<String>,
    probe_error: Option<String>,
}
impl NetworkState {
    pub fn busy(&self) -> bool {
        self.probes.running()
    }
}

impl App {
    fn refresh_network(&mut self, ctx: &egui::Context) {
        if self.network.load.running() {
            return;
        }
        self.network.error = None;
        self.network.refreshed = Some(Instant::now());
        self.network.load = Task::spawn(ctx, query_adapters);
    }
    pub fn poll_network(&mut self, _ctx: &egui::Context) {
        if let Some(result) = self.network.load.poll() {
            match result.and_then(|r| r) {
                Ok(rows) => {
                    self.network.previous = self
                        .network
                        .adapters
                        .drain(..)
                        .map(|a| (a.guid.clone(), a))
                        .collect();
                    if self
                        .network
                        .selected
                        .as_ref()
                        .is_none_or(|id| !rows.iter().any(|a| &a.guid == id))
                    {
                        self.network.selected = rows
                            .iter()
                            .find(|a| a.is_physical() && a.connected())
                            .or_else(|| rows.iter().find(|a| a.is_physical()))
                            .or_else(|| rows.first())
                            .map(|a| a.guid.clone());
                    }
                    self.network.adapters = rows;
                }
                Err(error) => self.network.error = Some(error),
            }
        }
        if let Some(result) = self.network.probes.poll() {
            match result.and_then(|r| r) {
                Ok(rows) => {
                    self.network.probe_rows = rows;
                    util::log("Small connection checks completed; results are reachability timings, not a bandwidth measurement.");
                }
                Err(error) => self.network.probe_error = Some(error),
            }
        }
        if let Some(result) = self.network.options_load.poll() {
            match result.and_then(|r| r) {
                Ok(rows) => self.network.options = Some(rows),
                Err(error) => self.network.options_error = Some(error),
            }
        }
    }
    pub fn execute_network(&mut self, action: NetworkAction, ctx: &egui::Context) {
        if self.network.probes.running() {
            self.toast(ToastKind::Info, "Connection checks are already running.");
            return;
        }
        match action {
            NetworkAction::RunChecks(adapter) => {
                self.network.probe_rows.clear();
                self.network.probe_error = None;
                self.network.probe_for = Some(adapter.name.clone());
                self.network.probes = Task::spawn(ctx, move || run_probes(adapter));
            }
        }
    }
    pub fn page_network(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if self
            .network
            .refreshed
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(15))
        {
            self.refresh_network(&ctx);
        }
        widgets::page_header(ui, "Connection health", "Local link rates, driver details and measured changes. Optional small checks report reachability, not internet speed.");
        let mut refresh = false;
        let mut check = None;
        let mut inspect = None;
        let mut launch_error = None;
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !self.network.load.running(),
                    widgets::small("Refresh adapters"),
                )
                .clicked()
            {
                refresh = true;
            }
            ui.checkbox(
                &mut self.network.show_virtual,
                "Include virtual / VPN adapters",
            );
            if self.network.load.running() {
                ui.spinner();
                ui.label(widgets::faint("Reading adapter details…"));
            }
        });
        if let Some(error) = &self.network.error {
            ui.colored_label(WARN, error);
        }
        ScrollArea::vertical().id_salt("network_health").auto_shrink([false, false]).show(ui, |ui| {
            let visible: Vec<_> = self.network.adapters.iter().filter(|a| self.network.show_virtual || a.is_physical()).collect();
            if visible.is_empty() && !self.network.load.running() { ui.label(widgets::dim("No physical adapters were reported. Include virtual adapters to review Windows' other connections.")); }
            for adapter in visible {
                widgets::tight_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui.selectable_label(self.network.selected.as_ref() == Some(&adapter.guid), &adapter.name).clicked() { self.network.selected = Some(adapter.guid.clone()); }
                        ui.label(RichText::new(&adapter.status).color(if adapter.connected() { GOOD } else { TEXT_FAINT }));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| { ui.label(widgets::strong(rate(adapter.link_bps), 14.0)); });
                    });
                    ui.add(Label::new(widgets::faint(&adapter.description)).truncate()).on_hover_text(&adapter.description);
                }); ui.add_space(4.0);
            }
            let Some(adapter) = self.network.selected.as_ref().and_then(|id| self.network.adapters.iter().find(|a| &a.guid == id)) else { return; };
            let delta = self.network.previous.get(&adapter.guid).and_then(|old| counter_delta(old, adapter));
            widgets::section(ui, &format!("{} · adapter details", adapter.name));
            widgets::card(ui, |ui| {
                ui.label(widgets::strong(&adapter.description, 16.0));
                ui.horizontal_wrapped(|ui| { widgets::badge(ui, &format!("Local receive: {}", rate(adapter.receive_link_bps)), ACCENT); widgets::badge(ui, &format!("Local transmit: {}", rate(adapter.transmit_link_bps)), ACCENT); });
                ui.label(widgets::faint("These are negotiated rates between this adapter and its local network. They do not measure your internet connection or guarantee application throughput."));
                if let Some(signal) = adapter.wifi_signal { ui.horizontal(|ui| { ui.label(widgets::dim(format!("Wi-Fi signal quality: {signal}%"))); widgets::bar(ui, signal as f32 / 100.0, if signal < 40 { WARN } else { GOOD }, egui::vec2(160.0, 7.0)); }); }
                if let Some(error) = &adapter.wifi_error { ui.label(widgets::faint(error)); }
                ui.label(widgets::dim(format!("Driver: {} · {}", if adapter.driver_provider.is_empty() { "Provider not reported" } else { &adapter.driver_provider }, if adapter.driver_version.is_empty() { "Version not reported" } else { &adapter.driver_version })));
                ui.label(widgets::faint(format!("Driver date: {}. A date alone does not establish whether a newer driver is needed.", if adapter.driver_date.is_empty() { "Not reported" } else { &adapter.driver_date })));
                ui.add(Label::new(widgets::faint(format!("Interface {} · {}", adapter.index, adapter.guid))).truncate()).on_hover_text(&adapter.guid);
                if adapter.remote_session { ui.colored_label(WARN, "Remote session detected. Connection health does not change adapter settings."); }
                if adapter.domain_authenticated || adapter.domain_member == Some(true) { ui.label(widgets::faint("Domain-managed connection: follow your administrator's network guidance.")); }
                if let Some(error) = &adapter.statistics_error { ui.label(widgets::faint(format!("Packet counters unavailable: {error}"))); }
                else { ui.label(widgets::dim(format!("Since driver counters started: {} received / {} sent · {} errors · {} discards", util::fmt_bytes(adapter.received_bytes), util::fmt_bytes(adapter.sent_bytes), adapter.received_errors.saturating_add(adapter.sent_errors), adapter.received_discards.saturating_add(adapter.sent_discards))));
                    if delta.is_none() { ui.label(widgets::faint("A second refresh is needed to compare packet errors and discards. Adapter counters can restart when Windows reconnects.")); }
                }
            });
            for finding in findings(adapter, delta) {
                widgets::banner(ui, if finding.warning { WARN } else { ACCENT }, ic::INFO, |ui| { ui.label(widgets::strong(finding.title, 14.0)); ui.label(widgets::dim(finding.detail)); });
            }
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(adapter.connected() && !self.network.probes.running(), widgets::small("Run small connection checks…")).clicked() { check = Some(adapter.clone()); }
                if ui.add_enabled(!self.network.options_load.running(), widgets::small("Review supported adapter options")).clicked() { inspect = Some(adapter.clone()); }
                if ui.add(widgets::small("Adapter settings")).clicked() { launch_error = util::open_system_document("control.exe", "ncpa.cpl").err(); }
                if ui.add(widgets::small("Device Manager")).clicked() { launch_error = util::open_system_document("mmc.exe", "devmgmt.msc").err(); }
            });
            ui.label(widgets::faint("Driver options vary by hardware and workload. Review the vendor's documentation before changing them in Windows. There is no universal 'maximum speed' setting; PCSweep keeps IPv6, offloads and adapter settings unchanged."));
            if self.network.options_load.running() { ui.spinner(); }
            if let Some(error) = &self.network.options_error { ui.colored_label(WARN, error); }
            if let Some((id, options)) = self.network.options.as_ref().filter(|(id,_)| id == &adapter.guid) {
                ui.label(widgets::strong(format!("{} driver-reported options", options.len()), 14.0));
                ScrollArea::vertical().id_salt(("network_options", id)).max_height(220.0).show_rows(ui, 46.0, options.len(), |ui, range| {
                    for option in &options[range] { ui.vertical(|ui| { ui.add(Label::new(widgets::dim(format!("{}: {}", if option.name.is_empty() { &option.keyword } else { &option.name }, option.value))).truncate()).on_hover_text(format!("{}\nSupported: {}", option.keyword, if option.supported.is_empty() { "Driver did not publish a choice list".into() } else { option.supported.join(", ") })); ui.add(Label::new(widgets::faint(if option.supported.is_empty() { "Choice list not published by driver".into() } else { format!("Supported: {}", option.supported.join(", ")) })).truncate()); }); }
                });
            }
            if self.network.probes.running() { ui.horizontal(|ui| { ui.spinner(); ui.label(widgets::dim("Running the small checks you approved…")); }); }
            if let Some(error) = &self.network.probe_error { ui.colored_label(WARN, error); }
            if !self.network.probe_rows.is_empty() {
                widgets::section(ui, &format!("Reachability checks · {}", self.network.probe_for.as_deref().unwrap_or("current route")));
                ui.label(widgets::faint("DNS and HTTPS follow Windows' current route, which may use a different adapter or VPN. Timings can vary; no bandwidth estimate is inferred."));
                for result in &self.network.probe_rows { widgets::tight_frame().show(ui, |ui| { ui.label(widgets::strong(format!("{} · {}{}", result.name, result.outcome, result.elapsed_ms.map(|ms|format!(" · {ms} ms")).unwrap_or_default()), 14.0)); ui.label(widgets::dim(&result.detail)); }); }
            }
        });
        if refresh {
            self.refresh_network(&ctx);
        }
        if let Some(error) = launch_error {
            self.network.error = Some(error);
        }
        if let Some(adapter) = inspect {
            self.network.options_error = None;
            self.network.options_load = Task::spawn(&ctx, move || query_options(adapter));
        }
        if let Some(adapter) = check {
            self.modal = Some(Modal { title: "Run small connection checks?".into(), body: "Resolve www.microsoft.com, send a small HTTPS HEAD request to Microsoft, and send one ping to this adapter's reported gateway. This is a reachability check; it does not download a test file or measure bandwidth.".into(), details: vec![format!("Selected adapter: {}", adapter.name)], note: Some("DNS/HTTPS use Windows' current route and resolver. Firewalls may block ping; no reply is not treated as proof of a broken connection.".into()), confirm: "Run checks".into(), danger: false, action: Action::Network(NetworkAction::RunChecks(adapter)) });
        }
    }
}

#[cfg(test)]
pub(crate) fn preview_fixture() -> NetworkState {
    let adapter = AdapterInfo {
        guid: "00112233-4455-6677-8899-aabbccddeeff".into(),
        index: 7,
        name: "Ethernet — primary office connection with a deliberately long interface alias"
            .into(),
        description: "Example 2.5GbE PCI Express Network Adapter".into(),
        status: "Up".into(),
        physical: true,
        hardware: true,
        interface_type: 6,
        link_bps: 100_000_000,
        receive_link_bps: 100_000_000,
        transmit_link_bps: 100_000_000,
        driver_version: "2.1.0.8".into(),
        driver_provider: "Example Networks".into(),
        driver_date: "2026-05-01".into(),
        ipv4_gateways: vec!["192.0.2.1".into()],
        received_bytes: 5_000_000_000,
        sent_bytes: 800_000_000,
        received_errors: 3,
        ..Default::default()
    };
    let mut previous = adapter.clone();
    previous.received_errors = 0;
    NetworkState {
        selected: Some(adapter.guid.clone()),
        previous: HashMap::from([(adapter.guid.clone(), previous)]),
        adapters: vec![adapter],
        refreshed: Some(Instant::now()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn adapter() -> AdapterInfo {
        AdapterInfo {
            guid: "00112233-4455-6677-8899-aabbccddeeff".into(),
            index: 4,
            name: "Ethernet [example]".into(),
            status: "Up".into(),
            physical: true,
            hardware: true,
            interface_type: 6,
            link_bps: 100_000_000,
            ipv4_gateways: vec!["192.0.2.1".into()],
            ..Default::default()
        }
    }
    #[test]
    fn parses_structured_adapters_with_physical_virtual_and_domain_guards() {
        let mut physical = adapter();
        physical.domain_member = Some(true);
        physical.remote_session = true;
        let mut vpn = adapter();
        vpn.guid = "10213243-5465-7687-98a9-bacbdcedfe0f".into();
        vpn.virtual_adapter = true;
        vpn.physical = false;
        let rows = parse_adapters(&serde_json::to_string(&vec![physical, vpn]).unwrap()).unwrap();
        assert!(rows[0].is_physical());
        assert!(rows[0].domain_member.unwrap());
        assert!(rows[0].remote_session);
        assert!(!rows[1].is_physical());
        assert!(parse_adapters("not-json").is_err());
        assert!(parse_adapters(r#"[{"name":"missing identity"}]"#).is_err());
    }
    #[test]
    fn findings_distinguish_negotiated_links_signal_and_actual_counter_growth() {
        let mut a = adapter();
        assert!(findings(&a, None)
            .iter()
            .any(|f| f.title.contains("100 Mbps")));
        a.interface_type = 71;
        a.wifi_signal = Some(25);
        assert!(findings(&a, None).iter().any(|f| f.title.contains("Weak")));
        assert!(!findings(&a, None)
            .iter()
            .any(|f| f.title.contains("Packet error")));
        assert!(findings(
            &a,
            Some(CounterDelta {
                errors: 2,
                ..Default::default()
            })
        )
        .iter()
        .any(|f| f.title.contains("Packet error")));
        a.status = "Disconnected".into();
        assert!(findings(&a, None).iter().all(|f| !f.warning));
    }
    #[test]
    fn counter_restart_is_not_reported_as_enormous_error_growth() {
        let mut old = adapter();
        old.received_errors = 100;
        let mut new = old.clone();
        new.received_errors = 3;
        assert_eq!(
            counter_delta(&old, &new).unwrap(),
            CounterDelta {
                reset: true,
                ..Default::default()
            }
        );
        new.received_errors = 104;
        new.sent_discards = 2;
        assert_eq!(
            counter_delta(&old, &new).unwrap(),
            CounterDelta {
                errors: 4,
                discards: 2,
                reset: false
            }
        );
        new.statistics_error = Some("denied".into());
        assert!(counter_delta(&old, &new).is_none());
    }
    #[test]
    fn native_wifi_layout_and_guid_match_documented_windows_types() {
        assert_eq!(std::mem::size_of::<Association>(), 68);
        assert_eq!(std::mem::size_of::<ConnectionAttributes>(), 604);
        let id = guid("00112233-4455-6677-8899-aabbccddeeff").unwrap();
        assert_eq!(id.data1, 0x00112233);
        assert_eq!(id.data4, [0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        assert!(guid("bad").is_none());
    }
    #[test]
    fn read_only_scripts_parse_and_never_reconfigure_an_adapter() {
        let a = adapter();
        for script in [
            ADAPTER_SCRIPT.to_string(),
            option_script(&a).unwrap(),
            probe_script(&a),
        ] {
            assert!(!script.contains("Set-Net"));
            assert!(!script.contains("Disable-Net"));
            assert!(!script.contains("netsh"));
            let parser=format!("$tokens=$null; $errors=$null; [void][System.Management.Automation.Language.Parser]::ParseInput({},[ref]$tokens,[ref]$errors); if ($errors.Count) {{ throw (($errors | ForEach-Object {{ $_.Message }}) -join '; ') }}; 'syntax-ok'",platform::ps_quote(&script));
            assert_eq!(
                platform::powershell(&parser, Duration::from_secs(20))
                    .unwrap()
                    .trim(),
                "syntax-ok"
            );
        }
    }
    #[test]
    fn windows_read_only_adapter_inventory_smoke() {
        let rows = query_adapters()
            .expect("Windows NetAdapter inventory must return structured rows or explicit errors");
        for row in rows {
            assert!(guid(&row.guid).is_some());
            assert!(row.index > 0);
        }
    }
    #[test]
    fn probe_only_uses_a_valid_ip_gateway_and_fixed_https_target() {
        let mut a = adapter();
        a.ipv4_gateways = vec!["'; throw 'injected".into()];
        a.ipv6_gateways = vec!["2001:db8::1".into()];
        let script = probe_script(&a);
        assert!(script.contains("$gateway = '2001:db8::1'"));
        assert!(!script.contains("injected"));
        assert!(script.contains("$request.Method = 'HEAD'"));
        assert!(script.contains("missing reply does not mean"));
        assert_eq!(rate(1_000_000_000), "1.0 Gbps");
        assert_eq!(rate(0), "Not reported");
    }
}
