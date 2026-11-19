//! Browser setup and an opt-in, reversible DNS trial. Browser permissions stay
//! with the browser; no extensions are sideloaded or forced by policy.
use crate::{
    app::{Action, App, Modal, ToastKind},
    dns::{self, Config, Trial},
    network::{self, AdapterInfo},
    platform,
    task::Task,
    theme::*,
    util,
    widgets::{self, ic},
};
use eframe::egui::{self, ScrollArea, Ui};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use winreg::{enums::*, RegKey};

const BROWSERS:[(&str,&str,&str,&str);3]=[
    ("Firefox","firefox.exe","uBlock Origin","https://addons.mozilla.org/firefox/addon/ublock-origin/"),
    ("Chrome","chrome.exe","uBlock Origin Lite","https://chromewebstore.google.com/detail/ublock-origin-lite/ddkjiahejlhfcafbddmgiahcphecmpfh"),
    ("Edge","msedge.exe","uBlock Origin Lite","https://microsoftedge.microsoft.com/addons/detail/ublock-origin-lite/cimighlppcgcoapaliogpjjdehbnofhn"),
];
#[derive(Clone)]
struct Browser {
    name: String,
    blocker: String,
    url: String,
    path: Option<PathBuf>,
}
fn browsers() -> Vec<Browser> {
    BROWSERS
        .iter()
        .map(|(name, exe, blocker, url)| {
            let path = [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE]
                .iter()
                .find_map(|h| {
                    RegKey::predef(*h)
                        .open_subkey(format!(
                            r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{exe}"
                        ))
                        .ok()?
                        .get_value::<String, _>("")
                        .ok()
                        .map(|s| PathBuf::from(s.trim_matches('"')))
                })
                .filter(|p| p.is_absolute() && p.is_file());
            Browser {
                name: (*name).into(),
                blocker: (*blocker).into(),
                url: (*url).into(),
                path,
            }
        })
        .collect()
}
struct AdapterView {
    adapter: AdapterInfo,
    config: Result<Config, String>,
    guard: Option<String>,
}
struct Inventory {
    browsers: Vec<Browser>,
    adapters: Vec<AdapterView>,
    trial: Option<Trial>,
}
fn inventory() -> Result<Inventory, String> {
    let all = network::query_adapters()?;
    let mut adapters: Vec<_> = all
        .iter()
        .filter(|a| a.is_physical())
        .map(|a| AdapterView {
            adapter: a.clone(),
            config: dns::capture(a),
            guard: dns::eligibility(a, &all).err(),
        })
        .collect();
    adapters.sort_by_key(|view| {
        (
            view.guard.is_some(),
            view.config.is_err(),
            view.adapter.recommended_order(),
        )
    });
    Ok(Inventory {
        browsers: browsers(),
        adapters,
        trial: dns::read_trial()?,
    })
}

#[derive(Clone)]
pub enum AdsAction {
    Begin {
        adapter: AdapterInfo,
        before: Config,
        desired: Config,
    },
    Keep(String),
    Undo(String),
    Dismiss(String),
}
impl AdsAction {
    pub fn is_resolution(&self) -> bool {
        matches!(self, Self::Keep(_) | Self::Undo(_) | Self::Dismiss(_))
    }
}
struct Probe {
    usable: bool,
    message: String,
    filter: String,
}
fn probe_script(config: &Config, adapter: &AdapterInfo) -> Result<String, String> {
    let servers = |family: &dns::Family,
                   v6: bool,
                   enabled: Option<bool>|
     -> Result<String, String> {
        if enabled == Some(false) {
            return Ok(String::new());
        }
        family
            .servers
            .iter()
            .map(|server| {
                let address: std::net::IpAddr =
                    server.parse().map_err(|_| "Invalid resolver address")?;
                if address.is_ipv6() != v6 {
                    return Err("Resolver address family does not match its configuration".into());
                }
                Ok(platform::ps_quote(server))
            })
            .collect::<Result<Vec<_>, String>>()
            .map(|values| values.join(","))
    };
    let v4 = servers(&config.v4, false, adapter.ipv4_enabled)?;
    let v6 = servers(&config.v6, true, adapter.ipv6_enabled)?;
    if v4.is_empty() && v6.is_empty() {
        return Err("No enabled protocol has a selected resolver to check.".into());
    }
    Ok(format!(
        r#"
$server4=@({v4});$server6=@({v6})
$passed4=0;$passed6=0;$messages=New-Object 'System.Collections.Generic.List[string]'
$filters=New-Object 'System.Collections.Generic.List[string]'
foreach($family in @(4,6)){{
    $servers=$(if($family -eq 4){{$server4}}else{{$server6}})
    foreach($server in $servers){{
        try{{
            $test=@(Resolve-DnsName -Name 'www.microsoft.com' -Type A -Server $server -DnsOnly -NoHostsFile -QuickTimeout -ErrorAction Stop | Where-Object {{$_.IPAddress}})
            if($test.Count -eq 0){{throw 'No addresses returned'}}
            if($family -eq 4){{$passed4++}}else{{$passed6++}}
            $messages.Add(('IPv'+$family+' '+$server+': resolved a normal website'))
        }}catch{{$messages.Add(('IPv'+$family+' '+$server+': not confirmed - '+$_.Exception.Message))}}
        try{{
            $ads=@(Resolve-DnsName -Name 'doubleclick.net' -Type A -Server $server -DnsOnly -NoHostsFile -QuickTimeout -ErrorAction Stop | Where-Object {{$_.IPAddress}})
            if($ads.Count -gt 0 -and @($ads|Where-Object {{$_.IPAddress -ne '0.0.0.0' -and $_.IPAddress -ne '::'}}).Count -eq 0){{$filters.Add(($server+': ad-domain probe blocked'))}}
            else{{$filters.Add(($server+': ad-domain blocking not confirmed'))}}
        }}catch{{
            if($_.Exception.NativeErrorCode -eq 9003 -or $_.FullyQualifiedErrorId -like 'DNS_ERROR_RCODE_NAME_ERROR*'){{$filters.Add(($server+': ad-domain probe returned no such name'))}}
            else{{$filters.Add(($server+': ad-domain probe not confirmed; a timeout does not prove blocking'))}}
        }}
    }}
}}
$systemOk=$false
try{{
    $system=[Net.Dns]::BeginGetHostAddresses('www.microsoft.com',$null,$null)
    if(-not $system.AsyncWaitHandle.WaitOne(5000)){{throw 'Windows lookup timed out'}}
    $systemOk=([Net.Dns]::EndGetHostAddresses($system).Count -gt 0)
    if(-not $systemOk){{throw 'Windows returned no addresses'}}
    $messages.Add('Windows system lookup: resolved a normal website')
}}catch{{$messages.Add(('Windows system lookup: not confirmed - '+$_.Exception.Message))}}
$required4={required4};$required6={required6}
$usable=$systemOk -and ($passed4+$passed6 -gt 0) -and (-not $required4 -or $passed4 -gt 0) -and (-not $required6 -or $passed6 -gt 0)
if($server4.Count -gt 0 -and -not $required4){{$messages.Add('IPv4 has no reported default gateway; unreachable IPv4 servers may need a route before they can be used')}}
if($server6.Count -gt 0 -and -not $required6){{$messages.Add('IPv6 has no reported default gateway; unreachable IPv6 servers may need a route before they can be used')}}
$messages.Add('Resolver checks do not prove complete internet access or browser ad blocking')
[pscustomobject]@{{usable=[bool]$usable;message=($messages -join "`n");filter=($filters -join "`n")}}|ConvertTo-Json -Compress
"#,
        required4 = if !adapter.ipv4_gateways.is_empty() && !v4.is_empty() {
            "$true"
        } else {
            "$false"
        },
        required6 = if !adapter.ipv6_gateways.is_empty() && !v6.is_empty() {
            "$true"
        } else {
            "$false"
        },
    ))
}
fn probe(config: Config, adapter: AdapterInfo) -> Result<Probe, String> {
    #[derive(serde::Deserialize)]
    struct Response {
        usable: bool,
        message: String,
        filter: String,
    }
    // Every selected server is queried, including IPv6. The outer process has a
    // fixed deadline; its termination cannot leave an unbounded UI worker.
    let s = platform::powershell(&probe_script(&config, &adapter)?, Duration::from_secs(65))?;
    let r: Response =
        serde_json::from_str(&s).map_err(|_| "The DNS check returned unreadable results")?;
    Ok(Probe {
        usable: r.usable,
        message: r.message,
        filter: r.filter,
    })
}
fn preferred_adapter(adapters: &[AdapterView]) -> Option<&AdapterView> {
    adapters
        .iter()
        .filter(|view| view.guard.is_none() && view.config.is_ok())
        .min_by_key(|view| view.adapter.recommended_order())
        .or_else(|| {
            adapters
                .iter()
                .min_by_key(|view| view.adapter.recommended_order())
        })
}

pub struct AdsState {
    load: Task<Result<Inventory, String>>,
    change: Task<Result<String, String>>,
    checks: Task<Result<Probe, String>>,
    inventory: Option<Inventory>,
    loaded: bool,
    selected: Option<String>,
    custom: bool,
    custom_text: String,
    trial: Option<Trial>,
    probe: Option<Probe>,
    probe_token: Option<String>,
    error: Option<String>,
    message: Option<String>,
    poll_time: Option<Instant>,
    reveal_trial_frames: u8,
}
impl Default for AdsState {
    fn default() -> Self {
        Self {
            load: Task::default(),
            change: Task::default(),
            checks: Task::default(),
            inventory: None,
            loaded: false,
            selected: None,
            custom: false,
            custom_text: String::new(),
            trial: None,
            probe: None,
            probe_token: None,
            error: None,
            message: None,
            poll_time: None,
            reveal_trial_frames: 0,
        }
    }
}
impl AdsState {
    pub fn busy(&self) -> bool {
        self.change.running() || self.trial_pending()
    }
    pub fn trial_pending(&self) -> bool {
        self.trial.as_ref().is_some_and(Trial::pending)
    }
    pub fn reveal_trial(&mut self) {
        // Keep the decision visible while egui settles the newly added card.
        self.reveal_trial_frames = 3;
    }
}
fn config_label(config: &Config) -> String {
    let part = |f: &dns::Family| {
        if f.automatic() {
            "Automatic (DHCP/router)".into()
        } else {
            f.servers.join(", ")
        }
    };
    format!(
        "Adapter IPv4: {}\nAdapter IPv6: {}",
        part(&config.v4),
        part(&config.v6)
    )
}
impl App {
    fn refresh_ads(&mut self, ctx: &egui::Context) {
        if !self.ads.load.running() && !self.ads.change.running() {
            self.ads.load = Task::spawn(ctx, inventory);
        }
    }
    pub fn poll_ads(&mut self, ctx: &egui::Context) {
        if let Some(r) = self.ads.load.poll() {
            self.ads.loaded = true;
            match r.and_then(|r| r) {
                Ok(i) => {
                    if self.ads.selected.as_ref().is_none_or(|selected| {
                        !i.adapters.iter().any(|v| &v.adapter.guid == selected)
                    }) {
                        self.ads.selected =
                            preferred_adapter(&i.adapters).map(|v| v.adapter.guid.clone());
                    }
                    self.ads.error = None;
                    self.ads.trial = i.trial.clone();
                    self.ads.inventory = Some(i)
                }
                Err(e) => self.ads.error = Some(e),
            }
        }
        if let Some(r) = self.ads.change.poll() {
            match r.and_then(|r| r) {
                Ok(message) => {
                    util::log(&message);
                    self.ads.message = Some(message);
                    self.ads.trial = dns::read_trial().ok().flatten();
                    if self
                        .ads
                        .trial
                        .as_ref()
                        .is_some_and(|t| t.phase == "Applied")
                    {
                        self.ads.reveal_trial();
                    }
                }
                Err(e) => {
                    util::log(format!("DNS trial: {e}"));
                    self.ads.error = Some(e);
                    self.ads.trial = dns::read_trial().ok().flatten();
                }
            }
            self.refresh_ads(ctx);
        }
        if let Some(r) = self.ads.checks.poll() {
            match r.and_then(|r| r) {
                Ok(p) => self.ads.probe = Some(p),
                Err(e) => self.ads.error = Some(e),
            }
        }
        if self
            .ads
            .poll_time
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
        {
            self.ads.poll_time = Some(Instant::now());
            match dns::read_trial() {
                Ok(t) => self.ads.trial = t,
                Err(e) => self.ads.error = Some(e),
            }
        }
        if let Some(t) = &self.ads.trial {
            if t.phase == "Applied"
                && self.ads.probe_token.as_deref() != Some(&t.token)
                && !self.ads.checks.running()
                && !self.ads.change.running()
            {
                self.ads.probe = None;
                self.ads.probe_token = Some(t.token.clone());
                let config = t.applied.clone();
                let adapter = t.adapter.clone();
                self.ads.checks = Task::spawn(ctx, move || probe(config, adapter));
            }
        }
    }
    pub fn execute_ads(&mut self, action: AdsAction, ctx: &egui::Context) {
        if self.ads.change.running() {
            self.toast(ToastKind::Info, "A DNS operation is still finishing.");
            return;
        }
        if !self.is_admin {
            self.toast(ToastKind::Info,"Restart as administrator to apply or restore DNS. Browser setup works in a normal session.");
            return;
        }
        self.ads.error = None;
        self.ads.probe = None;
        self.ads.change = Task::spawn(ctx, move || match action {
            AdsAction::Begin {
                adapter,
                before,
                desired,
            } => {
                if dns::capture(&adapter)? != before {
                    return Err("DNS changed after review. Refresh and try again.".into());
                }
                dns::begin(adapter,before,desired).map(|_|"DNS trial started. Check the results, then Keep or restore before the timer ends.".into())
            }
            AdsAction::Keep(token) => dns::keep(&token),
            AdsAction::Undo(token) => dns::restore(&token),
            AdsAction::Dismiss(token) => dns::dismiss(&token),
        });
    }
    pub fn page_ads(&mut self, ui: &mut Ui) {
        let globally_busy = self.mutations_running();

        let ctx = ui.ctx().clone();

        if !self.ads.loaded && !self.ads.load.running() {
            self.refresh_ads(&ctx);
        }

        let mut refresh = false;

        let mut action = None;

        let mut ask = None;
        let mut dismiss = None;
        let mut restart_admin = false;

        let busy = self.ads.change.running();

        let mut scroll = ScrollArea::vertical()
            .id_salt("ads_page")
            .auto_shrink([false, false]);
        if self.ads.reveal_trial_frames > 0 {
            self.ads.reveal_trial_frames -= 1;
            scroll = scroll.vertical_scroll_offset(0.0);
            ctx.request_repaint();
        }
        scroll.show(ui, |ui| {
            widgets::page_header(ui, "Ads and tracking", "Combine browser blocking with optional DNS filtering. Your choices stay explicit and reversible.");

            if let Some(e) = &self.ads.error {
                ui.colored_label(color_warning(), e);
            }
            if let Some(m) = &self.ads.message {
                ui.label(widgets::dim(m));
            }

            if let Some(t) = &self.ads.trial {
                if t.pending() {
                    widgets::card(ui, |ui| {
                        ui.label(widgets::strong(
                            format!(
                                "DNS trial · {} · {} seconds remaining",
                                t.adapter.name,
                                t.deadline.saturating_sub(dns::now())
                            ),
                            18.0,
                        ));
                        ui.label(widgets::dim("The separate recovery process restores the previous settings unless you keep this trial. Closing PCSweep does not keep it."));
                        if !t.result.is_empty() {
                            ui.label(widgets::faint(&t.result));
                        }

                        if self.ads.checks.running() {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("Checking selected IPv4/IPv6 resolvers and Windows lookup…");
                            });
                        }
                        if let Some(p) = &self.ads.probe {
                            ui.label(widgets::dim(if p.usable {
                                "Windows lookup and a selected resolver for each routed protocol responded."
                            } else {
                                "Connection checks did not pass. Review the details or restore the original DNS."
                            }));
                            egui::CollapsingHeader::new("Resolver check details")
                                .id_salt(("dns_probe_details", &t.token))
                                .default_open(false)
                                .show(ui, |ui| {
                                    ui.label(widgets::dim(&p.message));
                                    ui.label(widgets::faint(&p.filter));
                                });
                        }

                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .add_enabled(
                                    !busy
                                        && t.phase == "Applied"
                                        && self.ads.probe.as_ref().is_some_and(|p| p.usable),
                                    widgets::primary("Keep these DNS settings"),
                                )
                                .clicked()
                            {
                                action = Some(AdsAction::Keep(t.token.clone()));
                            }
                            if ui
                                .add_enabled(!busy, widgets::ghost("Restore now"))
                                .clicked()
                            {
                                action = Some(AdsAction::Undo(t.token.clone()));
                            }
                        });
                    });
                } else if t.phase == "Kept"
                    || t.phase == "Failed"
                    || t.phase == "Conflict"
                    || t.phase == "Restoring"
                {
                    widgets::card(ui, |ui| {
                        ui.label(widgets::strong(
                            format!("Last DNS change · {}", t.adapter.name),
                            15.0,
                        ));
                        ui.label(widgets::dim(&t.result));
                        if ui
                            .add_enabled(!busy, widgets::small("Undo last DNS change"))
                            .clicked()
                        {
                            action = Some(AdsAction::Undo(t.token.clone()));
                        }
                        if matches!(t.phase.as_str(), "Failed" | "Conflict")
                            && ui
                                .add_enabled(
                                    !busy,
                                    widgets::small("Keep current DNS and dismiss recovery…"),
                                )
                                .clicked()
                        {
                            dismiss = Some(t.token.clone());
                        }
                    });
                } else if !t.result.is_empty() {
                    ui.label(widgets::dim(&t.result));
                }
            }

            widgets::banner(ui, color_accent(), ic::INFO, |ui| {
                ui.vertical(|ui| {
                    ui.label("DNS blocks many ad/tracker domains across apps. A browser blocker handles page-level filtering.");
                    ui.label(widgets::faint("Neither layer blocks every ad. Same-domain YouTube/Twitch ads, sponsored posts and apps using their own DNS can remain."));
                });
            });

            widgets::section(ui, "DNS ad and tracker filtering");

            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        !self.ads.load.running() && !busy,
                        widgets::small("Refresh adapters"),
                    )
                    .clicked()
                {
                    refresh = true;
                }
                if self.ads.load.running() {
                    ui.spinner();
                    ui.label(widgets::dim("Reading adapters and DNS safeguards…"));
                }
                if ui.add(widgets::small("Windows DNS settings")).clicked() {
                    util::open_uri("ms-settings:network-status");
                }
            });

            if let Some(inventory) = &self.ads.inventory {
                for view in &inventory.adapters {
                    if ui
                        .selectable_label(
                            self.ads.selected.as_ref() == Some(&view.adapter.guid),
                            format!("{} · {} · {}{}", view.adapter.name, view.adapter.description, view.adapter.status,
                                if preferred_adapter(&inventory.adapters).is_some_and(|preferred|preferred.adapter.guid == view.adapter.guid) && view.guard.is_none() { " · Recommended" } else { "" }),
                        )
                        .clicked()
                    {
                        self.ads.selected = Some(view.adapter.guid.clone());
                    }
                }

                if let Some(view) = inventory
                    .adapters
                    .iter()
                    .find(|v| Some(&v.adapter.guid) == self.ads.selected.as_ref())
                {
                    widgets::card(ui, |ui| {
                        ui.label(widgets::strong(
                            format!("DNS for {}", view.adapter.name),
                            17.0,
                        ));
                        match &view.config {
                            Ok(c) => {
                                ui.label(widgets::dim(config_label(c)));
                                if c == &dns::adguard() { widgets::badge(ui,"AdGuard DNS configured",color_success()); }
                            }
                            Err(e) => {
                                ui.colored_label(color_warning(), e);
                            }
                        }

                        if let Some(g) = &view.guard {
                            ui.colored_label(color_warning(), g);
                            if let Some(preferred) = preferred_adapter(&inventory.adapters).filter(|v|v.guard.is_none() && v.adapter.guid != view.adapter.guid) {
                                if ui.add(widgets::small(format!("Use recommended adapter: {}",preferred.adapter.name))).clicked() {
                                    self.ads.selected = Some(preferred.adapter.guid.clone());
                                }
                            }
                        }
                        ui.add_space(4.0);

                        if view.adapter.ipv4_enabled == Some(false) || view.adapter.ipv6_enabled == Some(false) {
                            ui.label(widgets::faint("A network protocol is disabled on this adapter. AdGuard applies to enabled protocols; the disabled protocol's saved DNS stays unchanged."));
                        }
                        ui.radio_value(&mut self.ads.custom, false, "AdGuard public DNS · ad/tracker domain filtering");
                        ui.radio_value(
                            &mut self.ads.custom,
                            true,
                            "Use my existing local or custom resolver",
                        );

                        if self.ads.custom {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.ads.custom_text)
                                    .hint_text("IP addresses separated by commas")
                                    .desired_width(ui.available_width().min(520.0)),
                            );
                            ui.label(widgets::faint("A family without a custom address keeps its current configuration and may use a different resolver."));
                        }

                        ui.label(widgets::faint("This trial configures classic DNS only. Existing encrypted/managed DNS is protected. A public provider receives DNS requests under its own privacy policy; this is not anonymous browsing."));

                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .add(widgets::small("Provider and privacy details"))
                                .clicked()
                            {
                                util::open_uri("https://adguard-dns.io/en/privacy.html");
                            }
                            if ui
                                .add(widgets::small("AdGuard Home / Pi-hole guidance"))
                                .clicked()
                            {
                                util::open_uri("https://github.com/AdguardTeam/AdGuardHome");
                            }
                        });

                        if !self.is_admin {
                            ui.label(widgets::dim("Administrator access is needed to apply DNS. Your IP addresses, router and VPN configuration stay as they are."));
                            if ui.add_enabled(!globally_busy,widgets::small("Restart as administrator to apply DNS")).clicked() { restart_admin = true; }
                        }

                        let enabled = self.is_admin
                            && !globally_busy
                            && view.guard.is_none()
                            && view.config.is_ok();

                        if ui
                            .add_enabled(enabled, widgets::primary(if self.ads.custom { "Apply custom DNS…" } else { "Apply AdGuard DNS…" }))
                            .clicked()
                        {
                            if let Ok(before) = &view.config {
                                let desired = if self.ads.custom {
                                    dns::custom_for(&self.ads.custom_text, &view.adapter, before)
                                } else {
                                    Ok(dns::adguard_for(&view.adapter,before))
                                };
                                match desired {
                                    Ok(desired) => {
                                        ask = Some((view.adapter.clone(), before.clone(), desired))
                                    }
                                    Err(e) => self.ads.error = Some(e),
                                }
                            }
                        }
                        ui.label(widgets::faint("A 90-second safety trial starts after confirmation. Keep it to make the change persistent, or restore automatically."));
                    });
                } else {
                    ui.label(widgets::dim("No physical Wi-Fi or Ethernet adapter was found. Refresh after connecting a device. Browser blocking is still available."));
                }
            }
            widgets::section(ui, "Browser extensions");
            ui.label(widgets::dim("DNS filtering works across apps. Add a browser extension too for page-level ads; the browser requires you to confirm its installation and permissions."));

            let fallback: Vec<Browser> = BROWSERS.iter().map(|(name, _, blocker, url)| Browser { name: (*name).into(), blocker: (*blocker).into(), url: (*url).into(), path: None }).collect();
            let browser_detection_complete = self.ads.inventory.is_some();
            let browsers = self
                .ads
                .inventory
                .as_ref()
                .map(|i| &i.browsers)
                .unwrap_or(&fallback);

            for browser in browsers {
                widgets::card(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(widgets::strong(
                            format!("{} · {}", browser.name, browser.blocker),
                            16.0,
                        ));
                        widgets::badge(
                            ui,
                            if browser.path.is_some() {
                                "Browser found"
                            } else if browser_detection_complete {
                                "Not detected"
                            } else {
                                "Checking…"
                            },
                            color_muted(),
                        );
                    });

                    ui.label(widgets::dim(if browser.name == "Firefox" {
                        "Full uBlock Origin is supported on Firefox. Install from the official Mozilla listing."
                    } else {
                        "uBlock Origin Lite supports current Manifest V3 browsers. The browser explains site permissions and filtering modes."
                    }));

                    ui.horizontal_wrapped(|ui| {
                        if ui.add(widgets::small("Add browser extension…")).clicked() {
                            if !self.is_admin {
                                if let Some(path) = &browser.path {
                                    let _ =
                                        std::process::Command::new(path).arg(&browser.url).spawn();
                                } else {
                                    util::open_uri(&browser.url);
                                }
                            } else {
                                util::open_uri(&browser.url);
                            }
                        }

                        if ui.add(widgets::small("Copy store link")).clicked() {
                            ui.output_mut(|o| o.copied_text = browser.url.clone());
                        }
                    });


                });
            }


        });

        if restart_admin {
            self.relaunch_admin(&ctx);
        }
        if refresh {
            self.ads.error = None;
            self.refresh_ads(&ctx);
        }

        if let Some(action) = action {
            self.execute_ads(action, &ctx);
        }

        if let Some(token) = dismiss {
            self.modal = Some(Modal {
                title: "Dismiss saved DNS recovery?".into(),
                body: "Your current DNS configuration will stay as it is. This releases a failed or conflicting recovery record so you can review a new trial.".into(),
                details: Vec::new(),
                note: Some("The previous DNS configuration will no longer be offered as Undo. No network setting is changed by dismissing this record.".into()),
                confirm: "Keep current and dismiss".into(),
                danger: false,
                action: Action::Ads(AdsAction::Dismiss(token)),
            });
        }
        if let Some((adapter, before, desired)) = ask {
            self.modal = Some(Modal {
                title: format!("Try filtering DNS on {}?", adapter.name),
                body: "Windows will use the reviewed DNS servers for this physical adapter. Some apps or networks may need a different resolver. The original automatic/manual configuration is saved before changing anything.".into(),
                details: vec![
                    format!("Current:\n{}", config_label(&before)),
                    format!("Trial:\n{}", config_label(&desired)),
                ],
                note: Some("A recovery process restores the original settings unless you Keep the trial within 90 seconds. DNS requests go to your chosen provider. Encrypted/managed DNS, VPNs and remote sessions are protected.".into()),
                confirm: "Start DNS trial".into(),
                danger: false,
                action: Action::Ads(AdsAction::Begin {
                    adapter,
                    before,
                    desired,
                }),
            });
        }
    }
}

#[cfg(test)]
pub(crate) fn preview_fixture() -> AdsState {
    let mut state = AdsState::default();
    state.loaded = true;
    state.trial = None;
    let a = AdapterInfo {
        guid: "11111111-2222-3333-4444-555555555555".into(),
        index: 4,
        name: "Wi-Fi".into(),
        description: "Wireless adapter".into(),
        physical: true,
        hardware: true,
        status: "Up".into(),
        oper_status: Some(1),
        default_route: true,
        interface_type: 71,
        domain_member: Some(false),
        pnp_device_id: "PCI\\Preview".into(),
        ..Default::default()
    };
    state.selected = Some(a.guid.clone());
    state.inventory = Some(Inventory {
        browsers: BROWSERS
            .iter()
            .map(|(n, _, b, u)| Browser {
                name: (*n).into(),
                blocker: (*b).into(),
                url: (*u).into(),
                path: None,
            })
            .collect(),
        adapters: vec![AdapterView {
            adapter: a,
            config: Ok(Config::default()),
            guard: None,
        }],
        trial: None,
    });
    state
}
#[cfg(test)]
pub(crate) fn trial_preview_fixture() -> AdsState {
    let mut state = preview_fixture();
    let trial = dns::preview_trial();
    state.selected = Some(trial.adapter.guid.clone());
    if let Some(inventory) = &mut state.inventory {
        inventory.adapters = vec![AdapterView {
            adapter: trial.adapter.clone(),
            config: Ok(trial.applied.clone()),
            guard: None,
        }];
        inventory.trial = Some(trial.clone());
    }
    state.probe = Some(Probe {
        usable: true,
        message: "The selected resolver and Windows resolved a normal website. This is not a complete internet or browser test.".into(),
        filter: "Selected resolver returned blocked addresses for the ad-domain probe".into(),
    });
    state.probe_token = Some(trial.token.clone());
    state.message = Some(
        "DNS trial started. Check the results, then Keep or restore before the timer ends.".into(),
    );
    state.trial = Some(trial);
    state.reveal_trial();
    state
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_selection_uses_an_eligible_active_route_not_the_first_nic() {
        let mut spare = AdapterInfo {
            name: "Ethernet 2".into(),
            oper_status: Some(2),
            ..Default::default()
        };
        let mut active = spare.clone();
        active.name = "Wi-Fi".into();
        active.oper_status = Some(1);
        active.default_route = true;
        spare.guid = "spare".into();
        active.guid = "active".into();
        let views = vec![
            AdapterView {
                adapter: spare,
                config: Ok(Config::default()),
                guard: Some("Cable disconnected".into()),
            },
            AdapterView {
                adapter: active,
                config: Ok(Config::default()),
                guard: None,
            },
        ];
        assert_eq!(preferred_adapter(&views).unwrap().adapter.guid, "active");
    }
    #[test]
    fn only_official_store_targets_are_used() {
        assert!(BROWSERS[0].3.starts_with("https://addons.mozilla.org/"));
        assert!(BROWSERS[1].3.contains("ddkjiahejlhfcafbddmgiahcphecmpfh"));
        assert!(BROWSERS[2].3.contains("cimighlppcgcoapaliogpjjdehbnofhn"));
    }
    #[test]
    fn probe_does_not_accept_executable_server_text() {
        let mut c = Config::default();
        c.v4.servers.push("1.1.1.1';throw 'oops".into());
        let adapter = AdapterInfo::default();
        assert!(probe_script(&c, &adapter).is_err());
        assert!(probe_script(&dns::adguard(), &adapter)
            .unwrap()
            .contains("do not prove complete internet"));
    }
    #[test]
    fn probes_include_every_selected_server_but_skip_disabled_protocols() {
        let mut adapter = AdapterInfo::default();
        let script = probe_script(&dns::adguard(), &adapter).unwrap();
        for address in dns::adguard()
            .v4
            .servers
            .iter()
            .chain(dns::adguard().v6.servers.iter())
        {
            assert!(
                script.contains(address),
                "every selected resolver must be checked"
            );
        }
        adapter.ipv6_enabled = Some(false);
        let script = probe_script(&dns::adguard(), &adapter).unwrap();
        assert!(script.contains("94.140.14.14"));
        assert!(
            !script.contains("2a10:50c0"),
            "dormant IPv6 DNS must not affect Keep eligibility"
        );
        adapter.ipv4_enabled = Some(false);
        assert!(probe_script(&dns::adguard(), &adapter).is_err());
    }
    #[test]
    fn native_dns_probe_script_parses_without_sending_queries() {
        let script = probe_script(&dns::adguard(), &AdapterInfo::default()).unwrap();
        let parser = format!("$tokens=$null;$errors=$null;[void][System.Management.Automation.Language.Parser]::ParseInput({},[ref]$tokens,[ref]$errors);if($errors.Count){{throw (($errors|ForEach-Object {{$_.Message}})-join '; ')}};'syntax-ok'",platform::ps_quote(&script));
        assert_eq!(
            platform::powershell(&parser, Duration::from_secs(20))
                .unwrap()
                .trim(),
            "syntax-ok"
        );
    }
    #[test]
    fn pending_resolution_can_bypass_only_its_own_guard() {
        assert!(AdsAction::Undo("1-2".into()).is_resolution());
        assert!(AdsAction::Keep("1-2".into()).is_resolution());
    }
}
