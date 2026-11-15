//! A narrowly scoped DNS trial with an administrator-protected recovery record.
//! The watchdog is the same executable, never downloaded or copied helper code.
use crate::{
    network::{self, AdapterInfo},
    platform, util,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    net::IpAddr,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_sys::core::GUID;
use winreg::{enums::*, RegKey};

const KEY: &str = r"SOFTWARE\PCSweep\DnsUndo";
const VALUE: &str = "TrialV1";
const TRIAL_SECONDS: u64 = 90;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Family {
    pub servers: Vec<String>,
}
impl Family {
    pub fn automatic(&self) -> bool {
        self.servers.is_empty()
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    pub v4: Family,
    pub v6: Family,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Trial {
    pub schema: u32,
    pub token: String,
    pub adapter: AdapterInfo,
    pub before: Config,
    pub applied: Config,
    pub phase: String,
    pub started: u64,
    pub deadline: u64,
    pub ready: bool,
    pub result: String,
    #[serde(default)]
    recovery_expected: Option<Config>,
}
impl Trial {
    pub fn pending(&self) -> bool {
        matches!(
            self.phase.as_str(),
            "Prepared" | "Applying" | "Applied" | "Restoring"
        )
    }
    fn validate(&self) -> Result<(), String> {
        parse_guid(&self.adapter.guid)?;
        if self.schema != 1
            || self.token.is_empty()
            || self.token.len() > 80
            || !self.token.bytes().all(|c| c.is_ascii_digit() || c == b'-')
            || self.adapter.index == 0
            || self.adapter.pnp_device_id.is_empty()
            || self.deadline < self.started
            || self.deadline - self.started > 120
            || !matches!(
                self.phase.as_str(),
                "Prepared"
                    | "Applying"
                    | "Applied"
                    | "Restoring"
                    | "Restored"
                    | "Kept"
                    | "Conflict"
                    | "Failed"
                    | "Dismissed"
            )
        {
            return Err("The saved DNS trial is invalid. No DNS setting was changed.".into());
        }
        validate_config(&self.before)?;
        if let Some(expected) = &self.recovery_expected {
            validate_config(expected)?;
        }
        validate_config(&self.applied)
    }
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn parse_guid(s: &str) -> Result<GUID, String> {
    let s = s.trim_matches(['{', '}']).replace('-', "");
    if s.len() != 32 || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("Invalid adapter identity.".into());
    }
    let number =
        |a, b| u64::from_str_radix(&s[a..b], 16).map_err(|_| "Invalid adapter identity".to_owned());
    let mut data4 = [0; 8];
    for (i, c) in data4.iter_mut().enumerate() {
        *c = number(16 + i * 2, 18 + i * 2)? as u8;
    }
    Ok(GUID {
        data1: number(0, 8)? as u32,
        data2: number(8, 12)? as u16,
        data3: number(12, 16)? as u16,
        data4,
    })
}
fn validate_family(f: &Family, v6: bool) -> Result<(), String> {
    if f.servers.len() > 8 {
        return Err("Too many DNS servers.".into());
    }
    for s in &f.servers {
        let ip: IpAddr = s.parse().map_err(|_| "DNS servers must be IP addresses.")?;
        if ip.is_ipv6() != v6 || ip.is_unspecified() || ip.is_multicast() {
            return Err("DNS server address is invalid for this address family.".into());
        }
    }
    Ok(())
}
fn validate_config(c: &Config) -> Result<(), String> {
    validate_family(&c.v4, false)?;
    validate_family(&c.v6, true)
}
pub fn adguard() -> Config {
    Config {
        v4: Family {
            servers: vec!["94.140.14.14".into(), "94.140.15.15".into()],
        },
        v6: Family {
            servers: vec!["2a10:50c0::ad1:ff".into(), "2a10:50c0::ad2:ff".into()],
        },
    }
}
pub fn custom(text: &str, before: &Config) -> Result<Config, String> {
    let mut c = Config::default();
    for s in text
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|s| !s.is_empty())
    {
        let ip: IpAddr = s
            .parse()
            .map_err(|_| "Enter only the IP addresses of your existing DNS resolver.")?;
        if ip.is_ipv4() {
            c.v4.servers.push(ip.to_string())
        } else {
            c.v6.servers.push(ip.to_string())
        }
    }
    if c.v4.automatic() && c.v6.automatic() {
        return Err("Enter at least one DNS server.".into());
    }
    // A family without a custom address retains its existing source and servers.
    if c.v4.automatic() {
        c.v4 = before.v4.clone()
    }
    if c.v6.automatic() {
        c.v6 = before.v6.clone()
    }
    validate_config(&c)?;
    Ok(c)
}

fn secure_key() -> Result<RegKey, String> {
    if !crate::system::is_admin() {
        return Err(
            "Restart as administrator to change DNS. The current settings have been kept.".into(),
        );
    }
    let (key, _) = RegKey::predef(HKEY_LOCAL_MACHINE)
        .create_subkey_with_flags(KEY, KEY_ALL_ACCESS)
        .map_err(|e| e.to_string())?;
    #[link(name = "advapi32")]
    extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text: *const u16,
            revision: u32,
            descriptor: *mut *mut std::ffi::c_void,
            size: *mut u32,
        ) -> i32;
        fn RegSetKeySecurity(
            key: isize,
            information: u32,
            descriptor: *const std::ffi::c_void,
        ) -> i32;
    }
    let text: Vec<u16> = "D:P(A;;KA;;;SY)(A;;KA;;;BA)(A;;KR;;;BU)"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err("Could not protect the DNS undo record.".into());
    }
    let status = unsafe { RegSetKeySecurity(key.raw_handle(), 4, descriptor) };
    unsafe {
        windows_sys::Win32::Foundation::LocalFree(descriptor);
    }
    if status != 0 {
        return Err(format!("Could not secure DNS recovery (code {status})."));
    }
    Ok(key)
}
pub fn read_trial() -> Result<Option<Trial>, String> {
    let key = match RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(KEY) {
        Ok(k) => k,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let text: String = match key.get_value(VALUE) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if text.len() > 65536 {
        return Err("The DNS recovery record is too large.".into());
    }
    let t: Trial = serde_json::from_str(&text)
        .map_err(|_| "The DNS recovery record could not be read. Changes are blocked.")?;
    t.validate()?;
    Ok(Some(t))
}
fn save_trial(t: &Trial) -> Result<(), String> {
    t.validate()?;
    let key = secure_key()?;
    key.set_value(VALUE, &serde_json::to_string(t).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let status = unsafe { windows_sys::Win32::System::Registry::RegFlushKey(key.raw_handle()) };
    if status != 0 {
        return Err("Could not persist DNS recovery. No further changes were attempted.".into());
    }
    Ok(())
}
struct Lock(isize);
impl Lock {
    fn take() -> Result<Self, String> {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Threading::{CreateMutexW, WaitForSingleObject},
        };
        let name: Vec<u16> = "Global\\PCSweep.DnsTrial.v1"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let h = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if h == 0 {
            return Err("DNS recovery is in use or unavailable.".into());
        }
        let wait = unsafe { WaitForSingleObject(h, 0) };
        if wait == 0 || wait == 0x80 {
            Ok(Self(h))
        } else {
            unsafe { CloseHandle(h) };
            Err("Another DNS operation is running. Wait and refresh.".into())
        }
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::System::Threading::ReleaseMutex(self.0);
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

fn static_family(guid: &str, v6: bool) -> Result<Family, String> {
    parse_guid(guid)?;
    let protocol = if v6 { "Tcpip6" } else { "Tcpip" };
    let path =
        format!(r"SYSTEM\CurrentControlSet\Services\{protocol}\Parameters\Interfaces\{{{guid}}}");
    let key = match RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(path) {
        Ok(k) => k,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Family::default()),
        Err(e) => return Err(e.to_string()),
    };
    let raw = match key.get_raw_value("NameServer") {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Family::default()),
        Err(e) => return Err(e.to_string()),
    };
    if !matches!(raw.vtype, REG_SZ | REG_MULTI_SZ)
        || raw.bytes.len() % 2 != 0
        || raw.bytes.len() > 8192
    {
        return Err("This adapter's DNS source is not recognized. Use Windows Settings.".into());
    }
    let units: Vec<u16> = raw
        .bytes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    let s = String::from_utf16(&units).map_err(|_| "DNS source contains invalid text.")?;
    let family = Family {
        servers: s
            .split(|c: char| c.is_whitespace() || c == ',' || c == ';' || c == '\0')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
    };
    validate_family(&family, v6)?;
    Ok(family)
}
pub fn capture(adapter: &AdapterInfo) -> Result<Config, String> {
    Ok(Config {
        v4: static_family(&adapter.guid, false)?,
        v6: static_family(&adapter.guid, true)?,
    })
}
fn has_configuration(path: &str) -> Result<bool, String> {
    let key = match RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(path) {
        Ok(key) => key,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => {
            return Err(format!(
                "DNS safeguards could not be read. No change is allowed: {e}"
            ))
        }
    };
    for result in [
        key.enum_values().next().map(|r| r.map(|_| ())),
        key.enum_keys().next().map(|r| r.map(|_| ())),
    ] {
        match result {
            Some(Ok(())) => return Ok(true),
            Some(Err(e)) => {
                return Err(format!(
                    "DNS safeguards could not be inspected. No change is allowed: {e}"
                ))
            }
            None => {}
        }
    }
    Ok(false)
}

fn vpn_guard_script(all: &[AdapterInfo]) -> String {
    let indices = all
        .iter()
        .map(|a| a.index.to_string())
        .collect::<Vec<_>>()
        .join(",");
    // .NET includes built-in Windows RAS/PPP and tunnel interfaces that the
    // NetAdapter provider may omit. Never infer that their absence means safe.
    format!(
        r#"
$known=@({indices})
$blocked=$false
foreach($i in [Net.NetworkInformation.NetworkInterface]::GetAllNetworkInterfaces()){{
    if($i.OperationalStatus -ne [Net.NetworkInformation.OperationalStatus]::Up -or $i.NetworkInterfaceType -eq [Net.NetworkInformation.NetworkInterfaceType]::Loopback){{continue}}
    if($i.NetworkInterfaceType -eq [Net.NetworkInformation.NetworkInterfaceType]::Ppp -or $i.NetworkInterfaceType -eq [Net.NetworkInformation.NetworkInterfaceType]::Tunnel){{$blocked=$true;break}}
    $properties=$i.GetIPProperties();$indices=@()
    if($i.Supports([Net.NetworkInformation.NetworkInterfaceComponent]::IPv4)){{$indices+=($properties.GetIPv4Properties()).Index}}
    if($i.Supports([Net.NetworkInformation.NetworkInterfaceComponent]::IPv6)){{$indices+=($properties.GetIPv6Properties()).Index}}
    if($indices.Count -eq 0 -or @($indices | Where-Object {{$known -contains $_}}).Count -eq 0){{$blocked=$true;break}}
}}
$blocked|ConvertTo-Json -Compress
"#
    )
}
pub fn eligibility(adapter: &AdapterInfo, all: &[AdapterInfo]) -> Result<(), String> {
    if !adapter.is_physical()
        || !adapter.connected()
        || !matches!(adapter.interface_type, 6 | 71)
        || adapter.pnp_device_id.is_empty()
    {
        return Err("Select a connected physical Wi-Fi or Ethernet adapter.".into());
    }
    if adapter.remote_session {
        return Err("DNS changes are unavailable in a remote desktop session.".into());
    }
    if adapter.domain_authenticated || adapter.domain_member != Some(false) {
        return Err("This network may be managed. Review DNS with your administrator.".into());
    }
    if all.iter().any(|a| {
        a.connected()
            && !a.is_physical()
            && (!a.ipv4_gateways.is_empty()
                || !a.ipv6_gateways.is_empty()
                || [
                    "vpn",
                    "wireguard",
                    "wintun",
                    "tailscale",
                    "openvpn",
                    "zerotier",
                    "tunnel",
                ]
                .iter()
                .any(|p| {
                    format!("{} {}", a.name, a.description)
                        .to_lowercase()
                        .contains(p)
                }))
    }) {
        return Err("A VPN or virtual route is active. Keep its DNS configuration and use browser blocking instead.".into());
    }
    let blocked: bool = serde_json::from_str(&platform::powershell(
        &vpn_guard_script(all),
        Duration::from_secs(15),
    )?)
    .map_err(|_| "Windows did not confirm the VPN safeguard. No DNS setting was changed.")?;
    if blocked {
        return Err("Windows reports an active PPP, tunnel or unmapped interface. Keep its network configuration and use browser blocking instead.".into());
    }
    for path in [
        r"SOFTWARE\Policies\Microsoft\Windows NT\DNSClient",
        r"SYSTEM\CurrentControlSet\Services\Dnscache\Parameters\DnsPolicyConfig",
        r"SOFTWARE\Microsoft\Provisioning\OMADM\Accounts",
    ] {
        if has_configuration(path)? {
            return Err("Windows has DNS policy, managed enrollment or protected DNS configuration. Review DNS with your administrator; PCSweep will not override it.".into());
        }
    }
    let path = format!(
        r"SYSTEM\CurrentControlSet\Services\Dnscache\InterfaceSpecificParameters\{{{}}}\DohInterfaceSettings",
        adapter.guid
    );
    if has_configuration(&path)? {
        return Err("This adapter has encrypted DNS settings. Keep them and review DNS in Windows Settings.".into());
    }
    let cache = match RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"SYSTEM\CurrentControlSet\Services\Dnscache\Parameters")
    {
        Ok(key) => Some(key),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(format!(
                "Windows encrypted-DNS configuration could not be read: {e}"
            ))
        }
    };
    if let Some(cache) = cache {
        match cache.get_value::<u32, _>("EnableAutoDoh") {
            Ok(value) if value != 0 => return Err("Windows automatic encrypted DNS is configured. Keep it and review DNS in Windows Settings.".into()),
            Ok(_) => {},
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(e) => return Err(format!("Windows encrypted-DNS configuration could not be verified: {e}")),
        }
    }
    capture(adapter)?;
    load_setter()?;
    Ok(())
}
type Setter = unsafe extern "system" fn(
    GUID,
    *const windows_sys::Win32::NetworkManagement::IpHelper::DNS_INTERFACE_SETTINGS,
) -> u32;
fn load_setter() -> Result<Setter, String> {
    use windows_sys::Win32::System::LibraryLoader::{
        GetModuleHandleW, GetProcAddress, LoadLibraryExW,
    };
    let name: Vec<u16> = "iphlpapi.dll".encode_utf16().chain(Some(0)).collect();
    let mut module = unsafe { GetModuleHandleW(name.as_ptr()) };
    if module == 0 {
        module = unsafe { LoadLibraryExW(name.as_ptr(), 0, 0x800) }
    }
    let f = unsafe { GetProcAddress(module, b"SetInterfaceDnsSettings\0".as_ptr()) }
        .ok_or("This Windows build does not support the DNS trial API. Use Windows Settings.")?;
    Ok(unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, Setter>(f) })
}
fn set_family(adapter: &AdapterInfo, family: &Family, v6: bool) -> Result<(), String> {
    if family.automatic() {
        return Err(
            "Automatic DNS is restored through the documented Windows reset command.".into(),
        );
    }
    let mut names: Vec<u16> = family
        .servers
        .join(",")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut settings: windows_sys::Win32::NetworkManagement::IpHelper::DNS_INTERFACE_SETTINGS =
        unsafe { std::mem::zeroed() };
    settings.Version = 1;
    settings.Flags = 2 | if v6 { 1 } else { 0 };
    settings.NameServer = names.as_mut_ptr();
    let status = unsafe { load_setter()?(parse_guid(&adapter.guid)?, &settings) };
    if status != 0 {
        return Err(format!("Windows refused the DNS setting (code {status})."));
    }
    Ok(())
}
fn fresh(original: &AdapterInfo) -> Result<AdapterInfo, String> {
    network::query_adapters()?
        .into_iter()
        .find(|a| {
            a.guid.eq_ignore_ascii_case(&original.guid)
                && a.pnp_device_id
                    .eq_ignore_ascii_case(&original.pnp_device_id)
                && a.is_physical()
        })
        .ok_or("The adapter identity changed. No other adapter will be changed.".into())
}
fn reset(adapter: &AdapterInfo) -> Result<(), String> {
    let script=format!("$a=@(Get-NetAdapter -IncludeHidden | Where-Object {{ ([string]$_.InterfaceGuid).Trim('{{}}') -eq {} -and $_.PnPDeviceID -ceq {} }}); if($a.Count -ne 1){{throw 'Adapter identity changed'}}; Set-DnsClientServerAddress -InterfaceIndex $a[0].ifIndex -ResetServerAddresses -ErrorAction Stop",platform::ps_quote(&adapter.guid),platform::ps_quote(&adapter.pnp_device_id));
    platform::powershell(&script, Duration::from_secs(30))?;
    Ok(())
}
fn compatible_recovery(current: &Config, t: &Trial) -> bool {
    if current == &t.before {
        return true;
    }
    match t.phase.as_str() {
        "Prepared" | "Restored" | "Conflict" => false,
        "Kept" | "Applied" => current == &t.applied,
        "Failed" => t.recovery_expected.as_ref().is_some_and(|v| v == current),
        "Applying" | "Restoring" => {
            let family = |current: &Family, before: &Family, applied: &Family| {
                current == before
                    || current == applied
                    || (t.phase == "Restoring" && current.automatic())
            };
            family(&current.v4, &t.before.v4, &t.applied.v4)
                && family(&current.v6, &t.before.v6, &t.applied.v6)
        }
        _ => false,
    }
}
trait Recovery {
    fn capture(&mut self) -> Result<Config, String>;
    fn reset(&mut self) -> Result<(), String>;
    fn set(&mut self, family: &Family, v6: bool) -> Result<(), String>;
    fn save(&mut self, trial: &Trial) -> Result<(), String>;
}
struct WindowsRecovery(AdapterInfo);
impl Recovery for WindowsRecovery {
    fn capture(&mut self) -> Result<Config, String> {
        capture(&self.0)
    }
    fn reset(&mut self) -> Result<(), String> {
        reset(&self.0)
    }
    fn set(&mut self, family: &Family, v6: bool) -> Result<(), String> {
        set_family(&self.0, family, v6)
    }
    fn save(&mut self, trial: &Trial) -> Result<(), String> {
        save_trial(trial)
    }
}
fn restore_transaction(backend: &mut impl Recovery, t: &mut Trial) -> Result<String, String> {
    let current = backend.capture()?;
    if !compatible_recovery(&current, t) {
        t.phase = "Conflict".into();
        t.result="DNS changed outside PCSweep. Its current configuration was preserved; the original record remains available.".into();
        backend.save(t)?;
        return Err(t.result.clone());
    }
    t.phase = "Restoring".into();
    backend.save(t)?;
    if current != t.before {
        backend.reset()?;
        if !t.before.v4.automatic() {
            backend.set(&t.before.v4, false)?;
        }
        if !t.before.v6.automatic() {
            backend.set(&t.before.v6, true)?;
        }
    }
    if backend.capture()? != t.before {
        return Err("Windows did not confirm the original DNS settings. The recovery record has been kept; try Undo again.".into());
    }
    t.phase = "Restored".into();
    t.result = "Original automatic/manual DNS settings restored for both address families.".into();
    backend.save(t)?;
    Ok(t.result.clone())
}
pub fn restore(token: &str) -> Result<String, String> {
    let _lock = Lock::take()?;
    let mut t = read_trial()?.ok_or("No DNS trial was found.")?;
    if t.token != token {
        return Err("The DNS trial changed. Refresh before restoring.".into());
    }
    let result = fresh(&t.adapter)
        .and_then(|adapter| restore_transaction(&mut WindowsRecovery(adapter), &mut t));
    match result {
        Ok(message) => Ok(message),
        Err(e) => {
            if t.phase != "Conflict" {
                // A failed restore must not leave the UI permanently locked. Keep
                // the observed intermediate state so retries cannot erase a later
                // change made in Windows Settings.
                t.recovery_expected = capture(&t.adapter)
                    .ok()
                    .filter(|current| compatible_recovery(current, &t));
                t.phase = "Failed".into();
                t.result = format!("DNS recovery needs review: {e} The original record was kept. Retry Undo here or review Windows DNS settings.");
                save_trial(&t)?;
            }
            Err(t.result)
        }
    }
}
pub fn keep(token: &str) -> Result<String, String> {
    let _lock = Lock::take()?;
    let mut t = read_trial()?.ok_or("No DNS trial was found.")?;
    if t.token != token || t.phase != "Applied" || now() >= t.deadline {
        return Err("This DNS trial has ended. Refresh to see its current state.".into());
    }
    if capture(&fresh(&t.adapter)?)? != t.applied {
        return Err("DNS changed outside PCSweep; the new configuration was preserved.".into());
    }
    t.phase = "Kept".into();
    t.result = "DNS filtering configuration kept. Undo remains available here.".into();
    save_trial(&t)?;
    Ok(t.result)
}
/// Explicitly release a failed/conflicting backup without touching networking.
/// This is a separate reviewed choice so a legitimate outside DNS change does
/// not permanently prevent future trials or silently discard its Undo record.
pub fn dismiss(token: &str) -> Result<String, String> {
    let _lock = Lock::take()?;
    let mut t = read_trial()?.ok_or("No DNS recovery record was found.")?;
    if t.token != token || !matches!(t.phase.as_str(), "Failed" | "Conflict") {
        return Err("The DNS recovery state changed. Refresh before dismissing it.".into());
    }
    t.phase = "Dismissed".into();
    t.result = "Recovery dismissed by your choice. Current DNS settings were kept; the previous configuration is no longer offered as Undo.".into();
    save_trial(&t)?;
    Ok(t.result)
}
pub fn begin(adapter: AdapterInfo, expected: Config, desired: Config) -> Result<Trial, String> {
    validate_config(&desired)?;
    let all = network::query_adapters()?;
    let current = all
        .iter()
        .find(|a| a.guid == adapter.guid && a.pnp_device_id == adapter.pnp_device_id)
        .ok_or("The selected adapter changed. Refresh first.")?;
    eligibility(current, &all)?;
    let before = capture(current)?;
    if before != expected {
        return Err("DNS changed after review. Refresh before starting a trial.".into());
    }
    if before == desired {
        return Err("This adapter already has those DNS settings.".into());
    }
    let token = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let mut t = Trial {
        schema: 1,
        token: token.clone(),
        adapter: current.clone(),
        before,
        applied: desired,
        phase: "Prepared".into(),
        started: now(),
        deadline: now() + TRIAL_SECONDS,
        ready: false,
        result: String::new(),
        recovery_expected: None,
    };
    // Secure the executable before committing a pending recovery record. If this
    // fails, no journal and no network settings have changed.
    use std::os::windows::fs::OpenOptionsExt;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let _image = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&exe)
        .map_err(|e| e.to_string())?;
    let exe_text = exe.to_str().ok_or("App path could not be encoded")?;
    {
        let _lock = Lock::take()?;
        if read_trial()?
            .is_some_and(|old| old.pending() || matches!(old.phase.as_str(), "Failed" | "Conflict"))
        {
            return Err("Resolve the saved DNS recovery before starting another trial.".into());
        }
        save_trial(&t)?;
    }
    let spawn = util::hidden_command(exe_text)
        .args(["--pcsweep-dns-watchdog", &token])
        .spawn();
    let mut child = match spawn {
        Ok(child) => child,
        Err(e) => {
            let _lock = Lock::take()?;
            t.phase = "Restored".into();
            t.result = "Recovery could not start. No DNS setting was changed.".into();
            save_trial(&t)?;
            return Err(format!("Could not start DNS recovery: {e}"));
        }
    };
    let wait = Instant::now();
    loop {
        if read_trial()?.is_some_and(|v| v.token == token && v.ready) {
            break;
        }
        if wait.elapsed() > Duration::from_secs(10)
            || child.try_wait().map_err(|e| e.to_string())?.is_some()
        {
            let _ = restore(&token);
            return Err("DNS recovery did not start. The original settings were kept.".into());
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    let result = (|| {
        let _lock = Lock::take()?;
        t = read_trial()?.ok_or("DNS recovery record disappeared")?;
        if t.token != token || capture(current)? != t.before {
            return Err("DNS changed while the trial was being prepared.".into());
        }
        t.phase = "Applying".into();
        save_trial(&t)?;
        if !t.applied.v4.automatic() {
            set_family(current, &t.applied.v4, false)?;
        }
        if !t.applied.v6.automatic() {
            set_family(current, &t.applied.v6, true)?;
        }
        if capture(current)? != t.applied {
            return Err("Windows did not confirm both DNS settings.".into());
        }
        t.phase = "Applied".into();
        save_trial(&t)?;
        Ok(t.clone())
    })();
    if result.is_err() {
        let _ = restore(&token);
    }
    result
}
pub fn run_helper_mode() -> Option<i32> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_none_or(|a| a != "--pcsweep-dns-watchdog") {
        return None;
    }
    let run = (|| {
        if args.len() != 3 || !crate::system::is_admin() {
            return Err("DNS recovery requires its administrator session.".to_owned());
        }
        {
            let _lock = Lock::take()?;
            let mut t = read_trial()?.ok_or("No DNS recovery record")?;
            if t.token != args[2] || !t.pending() {
                return Err("DNS recovery identity changed".into());
            }
            t.ready = true;
            save_trial(&t)?;
        }
        let start = Instant::now();
        loop {
            let t = read_trial()?.ok_or("DNS recovery record disappeared")?;
            if t.token != args[2] || !t.pending() {
                return Ok(());
            }
            if now() >= t.deadline || start.elapsed() > Duration::from_secs(120) {
                match restore(&args[2]) {
                    Ok(_) => return Ok(()),
                    Err(e)
                        if e.contains("Another DNS operation")
                            && start.elapsed() < Duration::from_secs(180) => {}
                    Err(e) => return Err(e),
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    })();
    match run {
        Ok(()) => Some(0),
        Err(e) => {
            util::log(format!("DNS recovery: {e}"));
            Some(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct FakeRecovery {
        current: Config,
        journal: Option<Trial>,
        fail_save: bool,
        fail_v6: bool,
        writes: usize,
    }
    impl FakeRecovery {
        fn new(current: Config) -> Self {
            Self {
                current,
                journal: None,
                fail_save: false,
                fail_v6: false,
                writes: 0,
            }
        }
    }
    impl Recovery for FakeRecovery {
        fn capture(&mut self) -> Result<Config, String> {
            Ok(self.current.clone())
        }
        fn reset(&mut self) -> Result<(), String> {
            self.writes += 1;
            self.current = Config::default();
            Ok(())
        }
        fn set(&mut self, family: &Family, v6: bool) -> Result<(), String> {
            if v6 && self.fail_v6 {
                return Err("injected IPv6 refusal".into());
            }
            self.writes += 1;
            if v6 {
                self.current.v6 = family.clone();
            } else {
                self.current.v4 = family.clone();
            }
            Ok(())
        }
        fn save(&mut self, trial: &Trial) -> Result<(), String> {
            if self.fail_save {
                return Err("injected persistence failure".into());
            }
            self.journal = Some(trial.clone());
            Ok(())
        }
    }
    fn record() -> Trial {
        Trial {
            schema: 1,
            token: "1-2".into(),
            adapter: AdapterInfo {
                guid: "11111111-2222-3333-4444-555555555555".into(),
                index: 2,
                pnp_device_id: "PCI\\example".into(),
                ..Default::default()
            },
            before: Config {
                v4: Family::default(),
                v6: Family {
                    servers: vec!["::1".into()],
                },
            },
            applied: adguard(),
            phase: "Applied".into(),
            started: 100,
            deadline: 190,
            ready: true,
            result: String::new(),
            recovery_expected: None,
        }
    }
    #[test]
    fn addresses_and_journal_scope_are_strict() {
        assert!(adguard().v4.servers[0].parse::<IpAddr>().is_ok());
        let mut t = record();
        assert!(t.validate().is_ok());
        t.adapter.guid = "wrong".into();
        assert!(t.validate().is_err());
        assert!(custom("example.com", &Config::default()).is_err());
        assert!(custom("0.0.0.0", &Config::default()).is_err());
    }
    #[test]
    fn omitted_custom_family_retains_original_source() {
        let c = record().before;
        let new = custom("192.168.1.2", &c).unwrap();
        assert_eq!(new.v6, c.v6);
        assert!(!new.v4.automatic());
    }
    #[test]
    fn partial_apply_is_recoverable_but_outside_dns_is_not() {
        let mut t = record();
        t.phase = "Applying".into();
        let partial = Config {
            v4: t.applied.v4.clone(),
            v6: t.before.v6.clone(),
        };
        assert!(compatible_recovery(&partial, &t));
        let outside = Config {
            v4: Family {
                servers: vec!["8.8.8.8".into()],
            },
            ..t.applied.clone()
        };
        assert!(!compatible_recovery(&outside, &t));
        t.phase = "Kept".into();
        assert!(!compatible_recovery(&partial, &t));
        assert!(compatible_recovery(&t.before, &t));
    }
    #[test]
    fn automatic_outside_change_and_failed_retry_are_preserved() {
        let mut t = record();
        let automatic = Config::default();
        assert!(!compatible_recovery(&automatic, &t));
        t.phase = "Kept".into();
        assert!(!compatible_recovery(&automatic, &t));
        t.phase = "Restoring".into();
        assert!(compatible_recovery(&automatic, &t));
        t.recovery_expected = Some(automatic.clone());
        t.phase = "Failed".into();
        assert!(compatible_recovery(&automatic, &t));
        assert!(!compatible_recovery(&t.applied, &t));
        t.phase = "Conflict".into();
        assert!(!compatible_recovery(&t.applied, &t));
    }
    #[test]
    fn rollback_restores_both_families_and_original_sources() {
        let mut t = record();
        let mut backend = FakeRecovery::new(t.applied.clone());
        restore_transaction(&mut backend, &mut t).unwrap();
        assert_eq!(backend.current, t.before);
        assert!(backend.current.v4.automatic());
        assert!(!backend.current.v6.automatic());
        assert_eq!(backend.journal.unwrap().phase, "Restored");
    }
    #[test]
    fn failed_rollback_keeps_backup_and_can_retry_partial_restore() {
        let mut t = record();
        let original = t.before.clone();
        let mut backend = FakeRecovery::new(t.applied.clone());
        backend.fail_v6 = true;
        assert!(restore_transaction(&mut backend, &mut t).is_err());
        assert_eq!(backend.journal.as_ref().unwrap().before, original);
        assert_eq!(t.phase, "Restoring");
        t.recovery_expected = Some(backend.current.clone());
        t.phase = "Failed".into();
        backend.fail_v6 = false;
        restore_transaction(&mut backend, &mut t).unwrap();
        assert_eq!(backend.current, original);
    }
    #[test]
    fn rollback_cannot_write_without_journal_or_overwrite_outside_dns() {
        let mut t = record();
        let mut backend = FakeRecovery::new(t.applied.clone());
        backend.fail_save = true;
        assert!(restore_transaction(&mut backend, &mut t).is_err());
        assert_eq!(backend.writes, 0);
        backend.fail_save = false;
        t.phase = "Applied".into();
        backend.current = Config::default();
        assert!(restore_transaction(&mut backend, &mut t).is_err());
        assert_eq!(backend.writes, 0);
        assert_eq!(t.phase, "Conflict");
    }
    #[test]
    fn managed_virtual_remote_and_unknown_domain_are_guarded() {
        let mut a = record().adapter;
        a.status = "Up".into();
        a.physical = true;
        a.hardware = true;
        a.interface_type = 6;
        a.domain_member = Some(false);
        let mut remote = a.clone();
        remote.remote_session = true;
        assert!(eligibility(&remote, &[remote.clone()]).is_err());
        a.domain_member = None;
        assert!(eligibility(&a, &[a.clone()]).is_err());
    }
    #[test]
    fn native_read_and_capability_probe_do_not_mutate_dns() {
        let _ = load_setter();
        let adapters = network::query_adapters().unwrap();
        let script = vpn_guard_script(&adapters);
        let parser = format!("$tokens=$null;$errors=$null;[void][System.Management.Automation.Language.Parser]::ParseInput({},[ref]$tokens,[ref]$errors);if($errors.Count){{throw (($errors|ForEach-Object {{$_.Message}})-join '; ')}};'syntax-ok'", platform::ps_quote(&script));
        assert_eq!(
            platform::powershell(&parser, Duration::from_secs(20))
                .unwrap()
                .trim(),
            "syntax-ok"
        );
        let _: bool =
            serde_json::from_str(&platform::powershell(&script, Duration::from_secs(15)).unwrap())
                .unwrap();
        for a in adapters.iter().filter(|a| a.is_physical()).take(2) {
            let _ = capture(a);
        }
    }
}
