# PCSweep 1.2.0

This release adds privacy reviews, Defender integration, reviewed debloating, background-task controls, deeper memory diagnostics and verified in-app updates to the cleaner and space analyzer.

## Download

Download **PCSweep.exe** for a single portable app, or **windows-x64.zip** with the README and license. Requires 64-bit Windows 10 or 11 and an OpenGL-capable graphics driver. Releases are currently unsigned, so Windows may show an unrecognized-app warning. Administrator rights are optional; protected system actions explain when elevation is needed.

## Privacy and fewer distractions

- Review recognized account preferences for advertising ID, Windows tips, welcome suggestions, lock-screen facts and supported Start suggestions.
- Confirm each change and restore its exact original state with persistent Undo. The journal is written before changes and refuses to overwrite outside changes.
- Managed, unrecognized and build-specific controls lead to Windows Settings instead of reporting a registry write as successful enforcement.
- Review personalized offers, optional diagnostics, app permissions, Recall snapshots and clipboard history/cloud sync in Windows. Windows Hello remains responsible for protected Recall changes.
- Clear the current clipboard after a separate confirmation. Clipboard history and cloud items remain available for review in Windows.

Advertising-ID controls limit personalization; they do not remove every advertisement or stop website cookies. PCSweep does not promise zero Windows diagnostic traffic.

## Defender security check

- Read actual provider mode, real-time protection, signature dates and recent detections.
- Request signature updates, quick scans or full scans with confirmation. Defender retains its existing quarantine/remediation settings.
- Enable potentially unwanted app blocking through the supported API, verify the result and retain the original setting for Undo. Organization policies and Windows refusals are respected.
- Match fresh scan timestamps before reporting completion. A successful command never means “no threats.” Missing/cancelled results have bounded monitoring and a stop-monitoring control.
- Open offline scan options with a restart explanation; PCSweep does not silently reboot the computer.

## Reviewed app removal and updates

- Browse desktop programs and current-user Store/MSIX apps with type filters, publisher and available running/startup context.
- Review selected Store removals. Exact package versions and identities are rechecked before removal; shared frameworks, dependencies and Windows/security components remain protected.
- See per-app results and Store reinstall links where available. Reinstalling does not restore deleted app data.
- Preview installed-app updates and select which to install using Microsoft's official WinGet module. Optional setup is explicit, installs for the current account, checks Microsoft signatures and leaves execution policy and repository trust unchanged.

## Background activity

- Review scheduled-task actions, publishers, triggers and states with search and a third-party filter.
- Enable or disable supported third-party tasks with persistent Undo and a fresh task-definition identity check.
- Protect Windows, Microsoft, security and unverified task actions; task registrations are never deleted.
- Open browser background/Startup boost and extension settings without editing browser profiles or cookies.

## Better memory tools

- Separate available RAM, resident usage, private committed memory, commit limit/peak and cache context.
- Track private-memory growth over a bounded window. Growth is a useful symptom, not proof of a leak.
- Ask selected apps to close gracefully so save prompts can appear. Force-end remains a separate reviewed action.
- Offer efficiency mode for eligible background processes and restore the previous policy during the current PCSweep session.
- Offer an explicit Advanced working-set trim. It is temporary, may cause pages to reload, and does not fix high committed memory or a leak. No scheduled global RAM purge is added.
- Check exact process identity, own PID, critical-process state and foreground eligibility before process operations.

## Verified PCSweep updates

- Check manually or opt into a daily check while PCSweep is open. No service, device ID, inventory or activity upload is required.
- Compare explicit build revisions as well as versions, so same-version +build releases are detected.
- Restrict downloads to official releases, validate metadata, size and SHA-256, and show progress/cancellation and clear offline/error states.
- Normal user sessions in writable portable folders support update-and-restart with a backup, process-identity checks and startup acknowledgement. Restore the previous EXE when replacement or startup fails.
- Administrator sessions and protected folders get a verified portable copy with replacement guidance instead of an elevated helper.
- Publish update.json with the EXE, ZIP and checksums. Hash verification trusts the official GitHub release; independent code-signing verification is not claimed.

## Reliability and validation

- Serialize confirmed system changes and update installation, retain existing Downloads/personal-folder protection, and keep long operations off the UI thread.
- Use bounded PowerShell output and execution deadlines, exact identities, readback verification and truthful partial-failure messages.
- Validate on native Windows with regression tests, read-only Defender/Store/Task Scheduler checks, PowerShell parser tests and UI rendering at 1040×680 and 1320×860.
- Tests mutate only fake state or disposable files/registry keys. They do not remove real apps, run malware scans, install modules or change real security/privacy settings.

Releases publish automatically only after Windows tests, the optimized build and packaging checks pass. SHA256SUMS.txt covers the EXE, portable ZIP and update manifest.

## Local data and license

Settings, logs, staged updates and task/security undo records live in `%LOCALAPPDATA%\PCSweep`. Privacy Undo lives in `HKEY_CURRENT_USER\Software\PCSweep\PrivacyUndo`. Source is available under the MIT license.
