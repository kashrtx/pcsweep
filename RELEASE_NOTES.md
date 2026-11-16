# PCSweep 1.6.0

This release adds direct controls, fixes DNS eligibility, shows native app icons, and keeps expensive disk scans available between sessions.

## Control Windows updates directly

- Supported unmanaged Pro, Enterprise and Education editions can apply a feature/quality pause for up to 35 days, restore the previous pause, and choose automatic, notify-before-download or manual update behavior inside PCSweep.
- Pause configuration includes the policy gates and dates. Windows' observed pause status is separate: saved policy is not presented as proof of immediate enforcement.
- Home and Pro users have an explicit advanced Windows Update service control, with confirmed start mode/state and protected persistent Undo. Windows may repair or re-enable the service later; PCSweep reports that change. Managed computers are protected.
- Quiet inline reminders show days since PCSweep's change and the age of the last identified successful Windows OS quality/security installation. Dismiss, snooze for seven days, or turn reminders off. Installation age does not identify missing updates. Last known records are labelled cached when current history is unavailable.

## Direct privacy and performance preferences

- Advertising ID, tips, welcome suggestions and supported lock-screen/Start preferences have direct On/Off choices and precise Undo.
- Additional controls cover personalized offers, clipboard history/sync, supported search preferences, Start recommendations/recent files, optional diagnostics, transparency, Game Mode and background recording.
- Original values, absence and outside changes are preserved. Unknown mappings, organization policy, hardware support and account consent are explained rather than silently redirecting.
- Supported Recall blocking is a separate dangerous action: Windows deletes existing Recall snapshots when it takes effect. The confirmation explains that Undo cannot recover their contents. PCSweep does not enable Recall or bypass Windows Hello.
- Native permission/consent and detailed indexing pages remain optional advanced links, separate from controls PCSweep applies itself.

## Working DNS setup

- Native interface type, hardware flags and operational/media state replace fragile text/property checks. Eligibility explains exact reasons and recommends an eligible active connection.
- DNS is first on Ads and tracking, with direct Apply AdGuard DNS/custom DNS buttons and inline administrator restart.
- The reviewed 90-second trial preserves automatic/manual sources per protocol, respects disabled protocols, checks resolver connectivity and retains independent recovery. Keep persists the change; otherwise it restores automatically. Windows resolver cache is cleared after completed changes/restoration.
- Browser extension installation still requires browser permission confirmation. PCSweep opens the official listing and does not claim an extension is active merely because a browser exists.

## Reuse scans and review files comfortably

- Analyzer maps are remembered by root, including navigation/view state, with bounded memory/disk retention. Switching back reopens a saved map; uncached roots scan, and Rescan is explicit.
- Cleaner and duplicate results restore in the background. Saved results show their scan time and refresh guidance, rather than pretending to be live inventory.
- Restored results require refresh before destructive actions. Refresh this folder can update a targeted analyzer view without remapping a whole drive. Same-session file actions retain protection checks.
- Analyzer search, sorting, checkboxes, Select shown/Clear and selected-count/size summaries improve bulk Recycle Bin review. Protected paths and aggregate rows stay excluded.
- Installed apps use native desktop resource icons and Store AppsFolder artwork, with bounded background extraction/caching, native fallback and proper icon/GDI/COM cleanup.
- PCSweep remembers its last page and window size.

## Background availability and PCSweep updates

- App options offers optional close-to-notification-area behavior. The icon has Show, Check for updates and Quit. Normal closing remains the default; a missing icon cannot strand a hidden app.
- Optional daily PCSweep update checks also work in the background. Installation remains explicit and verified; updater/elevation restarts bypass background hiding.
- PCSweep does not install a background service or add itself to Windows startup. Idle background rendering and process polling are reduced.

## Validation and download

The workflow checks committed-source formatting, runs native Windows regression/UI tests and publisher tests, then builds the x64 executable. Service integration tests use a disposable test service that is never started; the runner's real Windows Update/DNS/security preferences are not changed. Previews cover compact/default/wide layouts, DPI, direct-control review/cancel, saved-map selection and modal geometry. Hardware-specific reachability and effective policy behavior still depend on the actual Windows machine.

Download PCSweep.exe or the portable ZIP. Apache License 2.0; copyright 2026 Kaushal Bhingaradia (kashrtx). LICENSE, NOTICE, privacy details and SHA-256 checksums are included. Releases remain unsigned. A passing main push publishes automatically, without a manual tag.
