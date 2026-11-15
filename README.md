# PCSweep

A fast, careful cleanup, privacy and PC health tool for Windows 11, written in Rust.

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

- **Privacy and ads:** review supported account preferences for advertising ID, Windows tips, welcome suggestions and lock-screen tips. Each change is confirmed and has persistent Undo. Unknown or managed settings open the relevant Windows page. Includes reviews for personalized offers, optional diagnostics, permissions, Recall and clipboard sync; clearing the current clipboard is a separate confirmed action.
- **Security check:** Defender provider status, signature freshness, recent detections, signature updates, quick/full scans and potentially unwanted app blocking with Undo. Scans use Defender's existing threat actions. Offline scanning opens Windows Security with a restart explanation.
- **Reviewed debloating:** desktop and current-user Store apps in one list. Frameworks, shared dependencies and Windows/security components are protected. Selected Store removals recheck the reviewed identities. Reinstall links are provided where available; deleted app data is not restored by reinstalling.
- **Installed-app updates:** preview and select updates through Microsoft's official WinGet PowerShell module. If needed, explicit setup installs the Microsoft-signed module for your account. PCSweep does not silently install support packages or change PowerShell execution policy.
- **Background activity:** review scheduled tasks, publishers and triggers. Supported third-party tasks can be enabled/disabled with persistent Undo. Windows/security tasks are protected. Browser background, Startup boost and extension shortcuts open the browser's own settings.
- **PCSweep updates:** manually check the official GitHub release, or opt into daily checking while the app is open. Downloads are checked by size and SHA-256. Normal user sessions in writable portable locations support update-and-restart with a backup and startup acknowledgement; administrator sessions and protected folders get a verified download instead.
- **Overview:** a health score based on things that genuinely affect speed (free space, junk, memory pressure, startup load, uptime), with one-click fixes.
- **Junk cleaner:** about 25 categories covering user and Windows temp, Store app temp, thumbnail cache, crash dumps and error reports, Delivery Optimization, Windows Update downloads, old logs, shader caches, driver installer leftovers, browser caches (Chrome, Edge, Firefox, Brave, Opera, Vivaldi), Discord, Teams, Slack, VS Code, Steam (including shader caches in every library), other Chromium/Electron app caches, and developer package caches. Expand any category to review every file and uncheck what you want to keep. Your selections are remembered, and a file that matches two categories is only counted once.
- **Space analyzer:** a parallel scan drawn as a clickable map you can drill into. Tabs for the largest files, old and unused files (filter by age and size), and a breakdown by file type.
- **Duplicates:** byte-for-byte duplicate detection. One copy of each file is always kept.
- **Installed apps:** sorted by size or install date, with each app's own uninstaller.
- **Startup apps:** on/off switches that work exactly like Task Manager's.
- **Memory and apps:** available RAM, committed memory and limit, cache context, private-memory growth and grouped app usage. Ask selected apps to close gracefully, use reversible efficiency mode for eligible background processes, or explicitly trim an app's working set under Advanced. Core/critical processes, reused PIDs and foreground processes are guarded. Trimming is temporary and may make an app reload pages; it does not fix a leak.
- **Tools:** restore point, component store cleanup (WinSxS), turn hibernation off/on, System File Checker, DISM repair, drive error check, TRIM/optimize, power plans and DNS flush, with live output.
- **Activity log:** searchable history of what PCSweep changed, including earlier sessions, with a button to copy matching entries. Saved to `%LOCALAPPDATA%\PCSweep\activity.log`.

## Safety rules (built into the code, not just the UI)

- Changes are reviewed first. Cleanup, app/package actions, privacy changes and update installation are serialized so they cannot interrupt each other.
- Privacy Undo restores the saved value and refuses to overwrite a preference changed outside PCSweep. Undo for settings does not restore deleted files or app data.
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

Tests use fake state and disposable files/registry keys for mutations. Windows smoke tests read Defender, Store-app and Task Scheduler status; they do not uninstall real apps, change security preferences, run malware scans or install modules. Rendering tests create previews under `target\ui-previews`.

## Local data and network use

Logs, settings, task/security undo records and staged updates are stored under `%LOCALAPPDATA%\PCSweep`. The precise privacy undo journal is stored in `HKEY_CURRENT_USER\Software\PCSweep\PrivacyUndo`. No background service is installed. App update checks are manual by default and send no device ID, inventory or activity history. GitHub still receives normal connection information such as the IP address. WinGet setup/updates and Defender updates contact their configured providers only when requested.

Updater hashes protect download integrity using the official GitHub release as the trust source. Releases are currently unsigned; independent publisher signature verification is not claimed.

## License

MIT. See `LICENSE`.

The workflow only publishes after all tests, build and packaging checks pass. A failed run leaves existing releases available.
