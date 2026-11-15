//! Current-user MSIX inventory and explicitly selected app updates.
//! Package operations use exact identities and recheck the reviewed version.
use crate::platform;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct StorePackage {
    pub name: String,
    pub identity: String,
    pub full_name: String,
    pub family: String,
    pub version: String,
    pub publisher: String,
    pub location: String,
    pub framework: bool,
    pub resource: bool,
    pub non_removable: bool,
    pub system: bool,
    pub dependents: Vec<String>,
}

impl Default for StorePackage {
    fn default() -> Self {
        Self {
            name: String::new(),
            identity: String::new(),
            full_name: String::new(),
            family: String::new(),
            version: String::new(),
            publisher: String::new(),
            location: String::new(),
            framework: false,
            resource: false,
            non_removable: false,
            system: false,
            dependents: Vec::new(),
        }
    }
}

fn identity_valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 512
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-~".contains(&c))
}

/// Deliberately conservative: these packages provide app deployment, security,
/// account sign-in or Windows shell services, rather than ordinary optional apps.
fn critical_identity(identity: &str) -> bool {
    let name = identity.to_ascii_lowercase();
    [
        "sechealth",
        "security",
        "defender",
        "windowsstore",
        "storepurchaseapp",
        "desktopappinstaller",
        "shellexperiencehost",
        "startmenuexperiencehost",
        "cloudexperiencehost",
        "aad.brokerplugin",
        "accountscontrol",
        "microsoftwindows.client",
        "windowsappruntime",
        "vclibs",
        "ui.xaml",
        "net.native",
        "immersivecontrolpanel",
        "win32webviewhost",
        "windows.search",
    ]
    .iter()
    .any(|part| name.contains(part))
}

impl StorePackage {
    pub fn protection_reason(&self) -> Option<String> {
        if !identity_valid(&self.full_name)
            || !identity_valid(&self.family)
            || !identity_valid(&self.identity)
        {
            return Some("Package identity could not be verified".into());
        }
        if self.non_removable || self.system || critical_identity(&self.identity) {
            return Some("Windows, security or app deployment component".into());
        }
        if self.framework || self.resource {
            return Some("Shared framework or language/resource package".into());
        }
        if !self.dependents.is_empty() {
            return Some(format!("Required by {}", self.dependents.join(", ")));
        }
        None
    }

    pub fn store_uri(&self) -> Option<String> {
        identity_valid(&self.family).then(|| format!("ms-windows-store://pdp/?PFN={}", self.family))
    }
}

// Get-AppxPackage without -AllUsers inspects ONLY the current account. Frameworks
// remain in the inventory so reverse dependencies can protect shared runtimes.
const STORE_INVENTORY: &str = r#"
$all = @(Get-AppxPackage -ErrorAction Stop)
$labels = @{}
try { Get-StartApps -ErrorAction Stop | ForEach-Object { $labels[($_.AppID -split '!')[0]] = $_.Name } } catch { }
$reverse = @{}
foreach ($candidate in $all) {
    foreach ($dependency in $candidate.Dependencies) {
        $key = [string]$dependency.PackageFullName
        if ($key -and $key -ne $candidate.PackageFullName) {
            if (-not $reverse.ContainsKey($key)) { $reverse[$key] = [System.Collections.Generic.List[string]]::new() }
            $reverse[$key].Add([string]$candidate.Name)
        }
    }
}
$rows = @($all | ForEach-Object {
    $p = $_
    $dependents = @()
    if ($reverse.ContainsKey([string]$p.PackageFullName)) { $dependents = @($reverse[[string]$p.PackageFullName] | Sort-Object -Unique) }
    $label = $labels[$p.PackageFamilyName]
    if ([string]::IsNullOrWhiteSpace($label)) { $label = $p.Name }
    [pscustomobject]@{
        name=[string]$label; identity=[string]$p.Name; full_name=[string]$p.PackageFullName;
        family=[string]$p.PackageFamilyName; version=[string]$p.Version; publisher=[string]$p.Publisher;
        location=[string]$p.InstallLocation; framework=[bool]$p.IsFramework; resource=[bool]$p.IsResourcePackage;
        non_removable=[bool]$p.NonRemovable;
        system=([string]$p.SignatureKind -eq 'System' -or ([string]$p.InstallLocation).StartsWith(($env:SystemRoot + '\SystemApps\'),[StringComparison]::OrdinalIgnoreCase));
        dependents=@($dependents)
    }
})
ConvertTo-Json -InputObject $rows -Depth 5 -Compress
"#;

pub fn store_packages() -> Result<Vec<StorePackage>, String> {
    let text = platform::powershell(STORE_INVENTORY, Duration::from_secs(60))?;
    parse_store_packages(&text)
}

fn parse_store_packages(text: &str) -> Result<Vec<StorePackage>, String> {
    let mut packages: Vec<StorePackage> =
        serde_json::from_str(text.trim_start_matches('\u{feff}').trim())
            .map_err(|e| format!("Windows returned an unreadable Store app inventory: {e}"))?;
    packages.retain(|p| !p.name.trim().is_empty());
    Ok(packages)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AppUpgrade {
    pub name: String,
    pub id: String,
    pub source: String,
    pub installed: String,
    pub available: String,
}

impl AppUpgrade {
    pub fn key(&self) -> String {
        format!("{}|{}", self.source, self.id)
    }
    pub fn valid(&self) -> bool {
        identity_valid(&self.id)
            && identity_valid(&self.source)
            && !self.installed.trim().is_empty()
            && !self.available.trim().is_empty()
            && !self.installed.eq_ignore_ascii_case("unknown")
            && !self.available.eq_ignore_ascii_case("unknown")
            && self.installed != self.available
    }
}

// Validate the signed Microsoft manifest and Windows PowerShell binaries before
// importing; a module with a matching display name alone is not sufficient.
const WINGET_IMPORT: &str = r#"
$module = Get-Module -ListAvailable -Name Microsoft.WinGet.Client | Sort-Object Version -Descending | Select-Object -First 1
if (-not $module) {
    throw "Selected app updates need Microsoft's optional WinGet module. Choose Set up app updates to install it for your account, or use Microsoft Store Library / each app's updater."
}
if ($module.Guid -ne [guid]'e11157e2-cd24-4250-83b8-c6654ea4926a' -or $module.Version -lt [version]'1.8.0' -or [IO.Path]::GetFileName($module.Path) -ne 'Microsoft.WinGet.Client.psd1') {
    throw 'The installed WinGet module identity or version is unsupported. Use the official Microsoft setup documentation.'
}
$base = [IO.Path]::GetFullPath($module.ModuleBase).TrimEnd('\')
$manifest = [IO.Path]::GetFullPath($module.Path)
if (-not $manifest.StartsWith(($base + '\'),[StringComparison]::OrdinalIgnoreCase)) { throw 'The WinGet module path could not be verified.' }
$verifiedFiles = @($manifest, (Join-Path $base 'net48\Microsoft.WinGet.Client.Cmdlets.dll'), (Join-Path $base 'net48\DirectDependencies\Microsoft.WinGet.Client.Engine.dll'))
foreach ($file in $verifiedFiles) {
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw 'The WinGet module is incomplete or does not support Windows PowerShell.' }
    $signature = Get-AuthenticodeSignature -LiteralPath $file -ErrorAction Stop
    if ([string]$signature.Status -ne 'Valid' -or -not $signature.SignerCertificate -or $signature.SignerCertificate.Subject -notmatch '(?i)(^|,\s*)CN=Microsoft Corporation(,|$)') {
        throw 'WinGet module publisher verification failed. Only valid Microsoft-signed modules are accepted.'
    }
}
Import-Module -Name $manifest -ErrorAction Stop -WarningAction SilentlyContinue
foreach ($spec in @(@('Get-WinGetPackage','Id','Source','MatchOption'),@('Update-WinGetPackage','PSCatalogPackage','Version','Mode'))) {
    $command = Get-Command -Name $spec[0] -Module Microsoft.WinGet.Client -ErrorAction Stop
    if ($command.Module.Version -ne $module.Version -or [IO.Path]::GetFullPath($command.Module.ModuleBase) -ne $base) { throw 'The loaded WinGet module does not match the verified module.' }
    foreach ($parameter in $spec[1..($spec.Length-1)]) { if (-not $command.Parameters.ContainsKey($parameter)) { throw 'The installed WinGet module lacks supported selected-update commands.' } }
}
"#;

const WINGET_SETUP: &str = r#"
$repository = Get-PSRepository -Name PSGallery -ErrorAction Stop
if ($repository.SourceLocation.TrimEnd('/') -ne 'https://www.powershellgallery.com/api/v2') {
    throw 'PSGallery is missing or points to a custom source. PCSweep will not change repository configuration. Use Microsoft setup documentation.'
}
$originalTls = [Net.ServicePointManager]::SecurityProtocol
try {
    [Net.ServicePointManager]::SecurityProtocol = $originalTls -bor [Net.SecurityProtocolType]::Tls12
    $nuget = @(Get-PackageProvider -ListAvailable -Name NuGet -ErrorAction SilentlyContinue | Where-Object { $_.Version -ge [version]'2.8.5.201' })
    if ($nuget.Count -eq 0) { Install-PackageProvider -Name NuGet -MinimumVersion '2.8.5.201' -Scope CurrentUser -Force -ErrorAction Stop | Out-Null }
    Install-Module -Name Microsoft.WinGet.Client -Repository PSGallery -Scope CurrentUser -MinimumVersion '1.8.0' -Force -ErrorAction Stop
} finally { [Net.ServicePointManager]::SecurityProtocol = $originalTls }
"#;

fn setup_script() -> String {
    format!("{WINGET_SETUP}\n{WINGET_IMPORT}\n\"Microsoft WinGet module $($module.Version) installed and verified for your account.\"")
}

pub fn available_updates() -> Result<Vec<AppUpgrade>, String> {
    // WinGet's CLI table is localized and truncated; never guess package IDs or
    // versions from it. The official module wraps WinGet's structured COM API.
    let script = format!(
        r#"{}
$rows = @(Get-WinGetPackage -ErrorAction Stop | Where-Object {{ $_.IsUpdateAvailable }} | ForEach-Object {{
    $versions = @($_.AvailableVersions)
    if ($versions.Count -gt 0) {{
        [pscustomobject]@{{name=[string]$_.Name; id=[string]$_.Id; source=[string]$_.Source;
        installed=[string]$_.InstalledVersion; available=[string]$versions[0]}}
    }}
}})
ConvertTo-Json -InputObject $rows -Depth 4 -Compress
"#,
        WINGET_IMPORT
    );
    let text = platform::powershell(&script, Duration::from_secs(120))?;
    parse_upgrades(&text)
}

fn parse_upgrades(text: &str) -> Result<Vec<AppUpgrade>, String> {
    let mut rows: Vec<AppUpgrade> =
        serde_json::from_str(text.trim_start_matches('\u{feff}').trim())
            .map_err(|e| format!("WinGet returned unreadable update details: {e}"))?;
    rows.retain(AppUpgrade::valid);
    rows.sort_by_key(|p| p.name.to_lowercase());
    rows.dedup_by(|a, b| a.key() == b.key());
    Ok(rows)
}

#[derive(Clone)]
pub enum PackageAction {
    SetupUpdates,
    Remove(Vec<StorePackage>),
    Upgrade(Vec<AppUpgrade>),
}

pub struct PackageResult {
    pub name: String,
    pub success: bool,
    pub detail: String,
    pub store_uri: Option<String>,
}

fn ps_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn remove_script(package: &StorePackage) -> Result<String, String> {
    if let Some(reason) = package.protection_reason() {
        return Err(reason);
    }
    Ok(format!(
        r#"
$identity = {full}
$all = @(Get-AppxPackage -ErrorAction Stop)
$matches = @($all | Where-Object {{ $_.PackageFullName -ceq $identity }})
if ($matches.Count -ne 1) {{ throw 'The reviewed package changed or is no longer installed. Refresh the app list.' }}
$p = $matches[0]
if ([string]$p.Name -cne {name} -or [string]$p.PackageFamilyName -cne {family} -or [string]$p.Version -cne {version}) {{
    throw 'Package identity changed. Refresh the app list.'
}}
$critical = '(?i)sechealth|security|defender|windowsstore|storepurchaseapp|desktopappinstaller|shellexperiencehost|startmenuexperiencehost|cloudexperiencehost|aad\.brokerplugin|accountscontrol|microsoftwindows\.client|windowsappruntime|vclibs|ui\.xaml|net\.native|immersivecontrolpanel|win32webviewhost|windows\.search'
if ($p.NonRemovable -or $p.IsFramework -or $p.IsResourcePackage -or [string]$p.SignatureKind -eq 'System' -or $p.Name -match $critical -or ([string]$p.InstallLocation).StartsWith(($env:SystemRoot + '\SystemApps\'),[StringComparison]::OrdinalIgnoreCase)) {{
    throw 'This package is protected. Use Windows Settings to review system components.'
}}
$dependent = @($all | Where-Object {{ $_.PackageFullName -ne $identity -and @($_.Dependencies | ForEach-Object {{ $_.PackageFullName }}) -contains $identity }})
if ($dependent.Count -gt 0) {{ throw 'Other installed packages now depend on this app. Refresh the app list.' }}
Remove-AppxPackage -Package $identity -ErrorAction Stop -Confirm:$false
if (@(Get-AppxPackage | Where-Object {{ $_.PackageFullName -ceq $identity }}).Count -gt 0) {{ throw 'Windows still reports this package installed.' }}
'App removed from your account. You can look for it in Microsoft Store to reinstall; local app data may have been removed.'
"#,
        full = ps_literal(&package.full_name),
        name = ps_literal(&package.identity),
        family = ps_literal(&package.family),
        version = ps_literal(&package.version)
    ))
}

fn upgrade_script(package: &AppUpgrade) -> Result<String, String> {
    if !package.valid() {
        return Err("Update identity or version could not be verified".into());
    }
    Ok(format!(
        r#"{import}
$matches = @(Get-WinGetPackage -Id {id} -Source {source} -MatchOption Equals -ErrorAction Stop | Where-Object {{ $_.Id -ceq {id} -and $_.Source -ceq {source} }})
if ($matches.Count -ne 1) {{ throw 'The reviewed app no longer has an exact WinGet match. Check again.' }}
$p = $matches[0]
if ([string]$p.InstalledVersion -cne {installed} -or -not $p.IsUpdateAvailable -or @($p.AvailableVersions) -notcontains {available}) {{
    throw 'The installed or available version changed. Check updates again.'
}}
$result = Update-WinGetPackage -PSCatalogPackage $p -Version {available} -Mode Default -ErrorAction Stop -Confirm:$false
if (-not $result.Succeeded()) {{ throw ("Installer status: " + $result.Status + "; code: " + $result.InstallerErrorCode) }}
if ($result.RebootRequired) {{ 'Update installed. Windows reports that a restart is required.' }} else {{ 'Update installed successfully.' }}
"#,
        import = WINGET_IMPORT,
        id = ps_literal(&package.id),
        source = ps_literal(&package.source),
        installed = ps_literal(&package.installed),
        available = ps_literal(&package.available)
    ))
}

pub fn run(action: PackageAction) -> Vec<PackageResult> {
    let mut results = Vec::new();
    let operations: Vec<(String, Result<String, String>, Duration, Option<String>)> = match action {
        PackageAction::SetupUpdates => vec![(
            "App update setup".into(),
            Ok(setup_script()),
            Duration::from_secs(600),
            None,
        )],
        PackageAction::Remove(packages) => packages
            .into_iter()
            .map(|p| {
                (
                    p.name.clone(),
                    remove_script(&p),
                    Duration::from_secs(180),
                    p.store_uri(),
                )
            })
            .collect(),
        PackageAction::Upgrade(packages) => packages
            .into_iter()
            .map(|p| {
                (
                    p.name.clone(),
                    upgrade_script(&p),
                    Duration::from_secs(900),
                    None,
                )
            })
            .collect(),
    };
    let mut blocked = false;
    for (name, script, timeout, store_uri) in operations {
        if blocked {
            results.push(PackageResult {
                name,
                success: false,
                detail: "Not started: the preceding operation timed out or its final state is unknown. Refresh before trying again.".into(),
                store_uri,
            });
            continue;
        }
        let result = script.and_then(|script| platform::powershell(&script, timeout));
        let (success, detail) = match result {
            Ok(text) => (true, text.trim().to_string()),
            Err(e) => {
                blocked = e.contains("timed out")
                    || e.contains("did not finish")
                    || e.contains("Could not wait");
                (false, e)
            }
        };
        crate::util::log(format!(
            "App action for {name}: {} — {detail}",
            if success { "completed" } else { "failed" }
        ));
        results.push(PackageResult {
            name,
            success,
            detail,
            store_uri,
        });
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    fn optional() -> StorePackage {
        StorePackage {
            name: "Example App".into(),
            identity: "Example.App".into(),
            full_name: "Example.App_1.0.0.0_x64__abc123".into(),
            family: "Example.App_abc123".into(),
            version: "1.0.0.0".into(),
            ..Default::default()
        }
    }
    #[test]
    fn core_frameworks_and_dependencies_are_protected() {
        let mut p = optional();
        assert!(p.protection_reason().is_none());
        p.framework = true;
        assert!(p.protection_reason().is_some());
        p.framework = false;
        p.dependents.push("Other App".into());
        assert!(p.protection_reason().is_some());
        p.dependents.clear();
        p.identity = "Microsoft.DesktopAppInstaller".into();
        assert!(p.protection_reason().is_some());
        p.identity = "Microsoft.SecHealthUI".into();
        assert!(p.protection_reason().is_some());
    }
    #[test]
    fn removal_is_current_user_and_version_specific() {
        let script = remove_script(&optional()).unwrap();
        assert!(script.contains("PackageFullName -ceq $identity"));
        assert!(script.contains("Version -cne '1.0.0.0'"));
        assert!(!script.contains("-AllUsers"));
        assert!(!script.contains("Provisioned"));
        let mut p = optional();
        p.full_name = "*".into();
        assert!(remove_script(&p).is_err());
    }
    #[test]
    fn structured_inventory_keeps_unknown_location_and_flags() {
        let data = r#"[{"name":"Example","identity":"Example.App","full_name":"Example.App_1_x64__abc","family":"Example.App_abc","framework":true}]"#;
        let p = parse_store_packages(data).unwrap();
        assert_eq!(p.len(), 1);
        assert!(p[0].location.is_empty());
        assert!(p[0].protection_reason().is_some());
        assert!(parse_store_packages("not JSON").is_err());
        assert!(parse_store_packages("[]").unwrap().is_empty());
    }
    #[test]
    fn setup_is_explicit_current_user_and_preserves_policy() {
        let script = setup_script();
        assert!(script.contains(
            "Install-Module -Name Microsoft.WinGet.Client -Repository PSGallery -Scope CurrentUser"
        ));
        assert!(script.contains("Get-AuthenticodeSignature"));
        assert!(script.contains("CN=Microsoft Corporation"));
        assert!(!script.contains("Set-ExecutionPolicy"));
        assert!(!script.contains("Set-PSRepository"));
        assert!(!script.contains("Register-PSRepository"));
        assert!(!WINGET_IMPORT.contains("Install-Module"));
    }

    #[test]
    fn package_mutation_scripts_parse_without_executing() {
        let package = AppUpgrade {
            name: "Example".into(),
            id: "Example.App".into(),
            source: "winget".into(),
            installed: "1.0".into(),
            available: "2.0".into(),
        };
        for script in [
            remove_script(&optional()).unwrap(),
            upgrade_script(&package).unwrap(),
            setup_script(),
            STORE_INVENTORY.into(),
        ] {
            let parser=format!("$tokens=$null; $errors=$null; $null=[Management.Automation.Language.Parser]::ParseInput({},[ref]$tokens,[ref]$errors); if($errors.Count -gt 0){{throw ($errors.Message -join '; ')}}; 'valid'",ps_literal(&script));
            assert_eq!(
                platform::powershell(&parser, Duration::from_secs(30)).unwrap(),
                "valid"
            );
        }
    }

    #[test]
    fn native_store_inventory_is_read_only_and_structured() {
        let packages = store_packages().expect("current-user Appx inventory should be readable");
        assert!(packages.iter().all(|p| !p.name.is_empty()));
    }

    #[test]
    fn update_commands_bind_exact_reviewed_identity_and_versions() {
        let p = AppUpgrade {
            name: "App".into(),
            id: "Publisher.App".into(),
            source: "winget".into(),
            installed: "1.0".into(),
            available: "2.0".into(),
        };
        let script = upgrade_script(&p).unwrap();
        assert!(script.contains("-MatchOption Equals"));
        assert!(script.contains("InstalledVersion -cne '1.0'"));
        assert!(script.contains("-Version '2.0'"));
        assert!(!script.contains("--all"));
        assert_eq!(ps_literal("one'two"), "'one''two'");
    }
}
