# PCSweep

A careful cleanup, privacy and PC health tool for Windows 10 and 11, written in Rust.

PCSweep shows you exactly where your disk space goes, finds junk that is safe to remove, and only deletes after you've reviewed it and confirmed.

## Download

**[Download the latest PCSweep.exe](https://github.com/kashrtx/pcsweep/releases/latest/download/PCSweep.exe)** · [Release notes and portable ZIP](https://github.com/kashrtx/pcsweep/releases/latest)

It's a single file with no installer. Double-click it to run.

- **Windows SmartScreen** may warn about an unrecognized app because the .exe isn't code-signed. Click **More info**, then **Run anyway**.
- **Admin rights** are optional. PCSweep runs fine as a normal user. Use **Restart as admin** in the sidebar to also clean protected system folders.
- **Requirements:** 64-bit Windows 10 or 11.

## Publishing a new version (for the maintainer)

Commit and push to `main`. GitHub Actions runs the Windows regression tests, renders UI previews, builds the app, and publishes a release with `PCSweep.exe`, a portable ZIP, SHA-256 checksums and `update.json` for the in-app updater. No tag, manual workflow run or extra secret is needed. Pushes to `codex/**` validate without publishing.

The first release of a Cargo version uses its usual tag, such as `v1.1.0`. Further pushes with that version get a unique build suffix, such as `v1.1.0+build.42`, so existing downloads stay intact. Bump `version` in `Cargo.toml` whenever you want a new named version. `RELEASE_NOTES.md` supplies the release description. Pull requests only validate and package the app; they do not publish releases. Build downloads are also available in each run's **Artifacts** section.

## What it does

- **Privacy and ads:** review supported account preferences for advertising ID, Windows tips, welcome suggestions and lock-screen tips. Direct On/Off changes are confirmed and have persistent Undo. Additional account, search, clipboard and supported diagnostic/Recall policies are described under New in 1.6.0. Unknown or managed settings show their reason; native consent pages are optional advanced links. Includes reviews for personalized offers, optional diagnostics, permissions, Recall and clipboard sync; clearing the current clipboard is a separate confirmed action.
- **Ads and tracking:** open the official uBlock Origin page for Firefox or uBlock Origin Lite page for Chrome/Edge. The browser asks for installation and permissions; PCSweep does not force extensions or claim they are active. Optional classic DNS filtering uses AdGuard Public DNS or the IP addresses of an existing local resolver such as AdGuard Home/Pi-hole. Review the adapter and both address families, then try a 90-second trial with resolver checks, Keep and Restore controls. A separate recovery process restores the saved automatic/manual DNS configuration if the trial is not kept, even if the app closes. Changing DNS requires administrator rights; browser setup does not.
- **Connection health:** review physical adapters, negotiated link rates, driver details, available Wi-Fi signal/rates and packet-counter changes. Inspect driver-reported options and open Windows network tools. Small, optional gateway/DNS/HTTPS checks help distinguish local connectivity problems; they do not measure internet bandwidth. No blanket adapter tuning is applied.
- **Battery and power:** review battery charge, runtime estimates, reported capacity and cycle count where available. Supported Windows builds offer separate plugged-in and battery power preferences, saved before applying and verified afterward, with persistent Undo. Older builds offer recognized power plans already advertised by Windows. Create local battery/SleepStudy reports or inspect sleep blockers, last wake and wake timers. Windows handles Energy Saver and manufacturer-specific charging limits.
- **Security check:** Defender provider status, signature freshness, recent detections, signature updates, quick/full scans and potentially unwanted app blocking with Undo. Scans use Defender's existing threat actions. Offline scanning opens Windows Security with a restart explanation.
- **Reviewed debloating:** desktop and current-user Store apps in one list. Frameworks, shared dependencies and Windows/security components are protected. Selected Store removals recheck the reviewed identities. Reinstall links are provided where available; deleted app data is not restored by reinstalling.
- **Installed-app updates:** preview and select updates through Microsoft's official WinGet PowerShell module. If needed, explicit setup installs the Microsoft-signed module for your account. PCSweep does not silently install support packages or change PowerShell execution policy.
- **Background activity:** review scheduled tasks, publishers and triggers. Supported third-party tasks can be enabled/disabled with persistent Undo. Windows/security tasks are protected. Browser background, Startup boost and extension shortcuts open the browser's own settings.
- **PCSweep updates:** manually check the official GitHub release, or opt into daily checking while PCSweep is running, including notification-area mode. Downloads are checked by size and SHA-256. Normal user sessions in writable portable locations support update-and-restart with a backup and startup acknowledgement; administrator sessions and protected folders get a verified download instead.
- **Overview:** a health score based on things that genuinely affect speed (free space, junk, memory pressure, startup load, uptime), with one-click fixes.
- **Junk cleaner:** about 25 categories covering user and Windows temp, Store app temp, thumbnail cache, crash dumps and error reports, Delivery Optimization, Windows Update downloads, old logs, shader caches, driver installer leftovers, browser caches (Chrome, Edge, Firefox, Brave, Opera, Vivaldi), Discord, Teams, Slack, VS Code, Steam (including shader caches in every library), other Chromium/Electron app caches, and developer package caches. **Review files** opens a full-page list for any category: search by name or folder, sort by size, date or folder, untick anything to keep, and use bulk Keep/Include actions that clearly say whether they apply to all files or only the filtered ones. Your selections are remembered, and a file that matches two categories is only counted once.
- **Space analyzer:** a parallel scan drawn as a clickable map you can drill into. Tabs for the largest files, old and unused files (filter by age and size), and a breakdown by file type.
- **Duplicates:** byte-for-byte duplicate detection. One copy of each file is always kept.
- **Installed apps:** sorted by size or install date, with each app's own uninstaller.
- **Startup apps:** on/off switches that work exactly like Task Manager's.
- **Memory and apps:** a compact summary of available RAM, committed memory and CPU (with more detail on demand), private-memory growth and grouped app usage in a list that uses the whole window. Tick several apps and use **Close selected** to ask them all to close normally; PCSweep then checks which apps actually exited and reports closed, still-open and unreachable apps separately, never force-closing. Private memory is read directly from Windows and shown as unavailable when Windows won't report it. You can also ask one app to close gracefully, use reversible efficiency mode for eligible background processes, or explicitly trim an app's working set under Advanced. Core/critical processes, reused PIDs and foreground processes are guarded. Trimming is temporary and may make an app reload pages; it does not fix a leak.
- **Tools:** restore point, component store cleanup (WinSxS), turn hibernation off/on, System File Checker, DISM repair, drive error check, TRIM/optimize, power plans and DNS flush, with live output.
- **Activity log:** searchable history of what PCSweep changed, including earlier sessions, with a button to copy matching entries. Saved to `%LOCALAPPDATA%\PCSweep\activity.log`.
- **Cleanup history:** a searchable record of every file PCSweep deletes or moves to the Recycle Bin, saved before anything is touched. Each entry shows the original path, size, category, when it happened and the result: deleted, moved to the Recycle Bin, removed empty folder, not deleted (with the reason) or uncertain if PCSweep was interrupted. Filter by result or the latest cleanup and export CSV or JSON. Stored in `%LOCALAPPDATA%\PCSweep\journal` with bounded size.

## Safety rules (built into the code, not just the UI)

- Nothing is deleted or moved unless it was first recorded in the cleanup journal; if the journal can't be written, the cleanup stops before changing anything.
- Changes are reviewed first. Cleanup, app/package actions, privacy changes and update installation are serialized so they cannot interrupt each other.
- Privacy Undo restores the saved value and refuses to overwrite a preference changed outside PCSweep. Undo for settings does not restore deleted files or app data.
- DNS changes are limited to the reviewed physical adapter and preserve the original IPv4/IPv6 automatic/manual sources. Managed/encrypted DNS, active VPN routes, domain-managed connections and remote sessions are guarded. The recovery record is protected for administrator writes; an outside DNS change is preserved for review. DNS filtering cannot remove every ad: browser secure DNS, VPNs and same-domain advertising can bypass it.
- Power Undo restores only the preferences PCSweep changed and preserves outside changes. Windows policy, hardware and manufacturer signals can affect the effective power mode. Performance can increase heat and energy use; efficiency can reduce throughput.
- Privacy controls reduce supported Windows suggestions and personalization. They do not block all internet advertising or promise zero diagnostic traffic. Defender handles malware findings; an unknown application is not automatically labeled spyware.
- **Downloads is never touched.** Nothing in your Downloads folder can be deleted or moved by any feature, including Downloads folders you have moved to another drive. You can still see those files in the analyzer, marked with a lock.
- The junk cleaner can't delete anything inside Documents, Desktop, Pictures, Videos, Music or OneDrive, even if a cache path somehow pointed there.
- Nothing is deleted without a confirmation dialog that shows what will happen.
- Your own files (from the analyzer and duplicate finder) go to the **Recycle Bin**, so they can be restored. Only junk caches are deleted permanently. You're warned when items are on a USB or network drive, which has no Recycle Bin.
- System and program folders, the page file and the hibernation file can't be removed from the analyzer.
- Scans never follow junctions or symbolic links, so nothing outside the chosen folder is counted or cleaned.
- Temp files changed in the last 24 hours are skipped, because they are likely in use. Files that are locked are skipped automatically.
- Reviewed junk files are checked again before deletion; files with a changed size or timestamp are skipped. Driver installer leftovers are optional and must be at least a week old.
- Duplicates are compared byte for byte, then checked again before recycling while the retained copy is held open to prevent changes.
- Browser cleaning removes caches only. Passwords, history, cookies and open tabs are kept.
- Startup changes only switch an app on or off; nothing is deleted.
- Core Windows processes can't be ended from the Memory page.

## Honest notes on "PC speed tricks"

- **Clearing Prefetch** makes apps launch slower for a while and frees almost nothing. PCSweep includes it, but it's off by default with a warning.
- **"RAM cleaners"** don't speed anything up. Windows uses spare memory as a cache, and forcing it empty just makes apps reload. What really helps is closing heavy apps and stopping them from launching at startup, and PCSweep shows you which ones those are.
- **Efficiency mode** targets CPU/power usage. It is optional and may reduce background throughput; it is not a RAM-clearing feature. Its previous process policy can be restored during the same PCSweep session.
- **Free disk space** matters: Windows slows down and updates fail when the system drive is nearly full. That's where PCSweep spends most of its effort.

## Building locally (optional)

On Windows with Rust and the Visual Studio C++ build tools: `cargo build --release --locked`. The app is at `target\release\PCSweep.exe`. Run regression tests with `cargo test --locked`.

Tests use fake state and disposable files/registry keys for mutations. Windows smoke tests read Defender, Store-app, Task Scheduler, adapter and power status; they do not uninstall real apps, change real DNS/power/security settings, run malware scans or install modules. Rendering tests create previews under `target\ui-previews`, including the new pages and long confirmation dialogs at supported window sizes.

## Local data and network use

Logs, settings, task/security undo records and staged updates are stored under `%LOCALAPPDATA%\PCSweep`. Power reports are saved in its `reports` folder and can include device and usage details; PCSweep does not upload them. Privacy Undo lives in `HKEY_CURRENT_USER\Software\PCSweep\PrivacyUndo`, power Undo in `HKEY_CURRENT_USER\Software\PCSweep\PowerUndo`, and the administrator-protected DNS recovery record in `HKEY_LOCAL_MACHINE\SOFTWARE\PCSweep\DnsUndo`.

No background service is installed. A DNS trial starts a temporary recovery process; supported Windows AC/DC power preferences continue to work without PCSweep running. App update checks are manual by default and send no device ID, inventory or activity history. GitHub still receives normal connection information such as the IP address. WinGet setup/updates and Defender updates contact their configured providers only when requested. Optional connection checks contact the gateway and Microsoft endpoints; DNS trial checks also query an ad-domain probe through the selected resolver. A chosen public DNS provider receives normal DNS requests under its own privacy policy. Browser extension links open the relevant official store.

Updater hashes protect download integrity using the official GitHub release as the trust source. Releases are currently unsigned; independent publisher signature verification is not claimed.

## New in 1.6.0

- **Direct Windows Update control:** supported Pro+ pause/resume policy, automatic/notify/manual behavior and driver choices, plus an explicit advanced reversible Windows Update service control for Home and Pro. The app shows actual service state and observed pause status. Windows may repair a service block; it is not presented as permanent enforcement. Quiet installation-age reminders can be dismissed, snoozed or disabled.
- **Direct preferences:** On/Off/Undo for transparency, Game Mode, background recording, personalized offers, clipboard history/sync, supported search and Start preferences, and optional diagnostics on supported editions. Recall blocking is a separately confirmed action that deletes existing snapshots; Undo restores policy, not snapshot contents. Required consent stays with Windows.
- **DNS fix:** native adapter state/type classification, active-route recommendation, precise eligibility reasons, direct AdGuard/custom trial controls, disabled-protocol preservation, multi-resolver checks and robust per-family recovery. DNS setup is first on the page. Browser extensions still ask for installation permission in the browser.
- **Remembered scans:** bounded analyzer caches by root, instant in-memory drive switching and background loading of older maps. Cleaner/duplicate snapshots restore too. Saved scans are labelled with their time and require refresh before deletion; Refresh this folder can update a focused analyzer view without scanning the whole drive.
- **Analyzer review:** search, sort and select visible files/folders with checkboxes, selection size/count and a reviewed bulk move to the Recycle Bin. Protected locations and aggregate rows cannot be selected.
- **Native app icons:** desktop icon resources and Store Shell artwork, extracted on bounded workers with native fallbacks.
- **Convenience:** last-page/window restoration and optional close-to-notification-area mode under App options. Show, Check for updates and Quit remain available from the icon. No startup entry or background service is installed; idle background work is reduced.

## New in 1.5.0

- **Responsive review:** full-page cleaner file review with search, sorting and retained selections; compact Memory page with multi-app checkboxes and normal-close results verified against the reviewed process instances.
- **Cleanup history:** local pre-operation intentions and per-item outcomes, searchable/exportable history, guarded storage and stop-on-journal-failure behavior. History has bounded retention; missing outcomes are uncertain.
- **Windows file changes:** a bounded, paged read of an existing NTFS journal and existing Security audit evidence. Windows File History is a separate optional backup feature. NTFS records do not identify the responsible app, old records expire, and resolved paths use current parent locations. Existing audit evidence requires prior auditing and does not by itself prove deletion.
- **Disk health:** cached storage status on Overview, physical-disk/volume mapping, reported health and available counters, separate OS TRIM notification policy, scheduled maintenance information and reviewed Windows-native Analyze/Optimize actions. Unknown counters and missing per-volume history stay unknown.
- **Windows controls:** reversible supported animation controls, edition/management-gated manual/notify Windows Update policy and driver-update exclusion. Configured readback and legacy Update API reports are shown separately from effective Windows 11 enforcement. Pause, active hours, transparency and indexing use the native Settings pages where direct controls are not verified. Local update/reliability history provides a What changed timeline without attributing causes automatically.
- **Internet speed test:** optional native M-Lab NDT7 download/upload measurements, data caps, cancellation, TCP timing context and optional session-local result history. M-Lab publishes test results including public IP addresses and retains them indefinitely. Tests require consent; see [PRIVACY.md](PRIVACY.md).
- **Consistent layouts:** reserved scrollbar gutters, one scroll region per confirmation, footer actions outside scrolling content, responsive lists and compact-window fitting. Native Windows tests include small, default and wide previews, high-DPI cases and interaction checks.

## License

Copyright 2026 Kaushal Bhingaradia.

Licensed under the [Apache License, Version 2.0](LICENSE). See `LICENSE` and `NOTICE`. Contributions are accepted under the same license.

The workflow only publishes after all tests, build and packaging checks pass. A failed run leaves existing releases available.
