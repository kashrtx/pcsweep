PCSweep is a free cleanup and PC health tool for Windows 11 and 10. It shows exactly where your disk space goes, finds junk that is safe to remove, and never deletes anything until you've reviewed it and confirmed.

## Download and run

1. Download **PCSweep.exe** from the assets at the bottom of this page. It's a single file with no installer and nothing else to set up.
2. Double-click it. Windows SmartScreen may say it "protected your PC" because the app isn't code-signed yet. Click **More info**, then **Run anyway**.
3. Optional: click **Restart as admin** in the sidebar to also clean protected Windows folders such as Windows Temp, update caches and crash dumps.

To remove PCSweep, delete the .exe. Its log and settings live in `%LOCALAPPDATA%\PCSweep` if you want to delete those too.

**Requirements:** 64-bit Windows 10 or 11, and a graphics driver with OpenGL support, which almost every PC has.

## What's new in 1.1.0

- **Safer cleanup:** files changed since review are skipped; protection is checked again before deletion, including junctions and redirected paths. Driver installer leftovers are now optional and must be at least a week old.
- **Verified duplicates:** final byte comparisons, another check before recycling, and a locked retained copy help keep the last copy safe. Overlapping scan folders no longer create false duplicates, and reclaimable totals update after removal.
- **Searchable activity history:** filter previous activity and copy matching entries for troubleshooting.
- **More reliable app controls:** uninstallers handle paths with spaces correctly; process confirmations check that a PID still belongs to the same process. The memory page shows all matching apps.
- **Smoother background work:** task failures appear as errors, cancelled scans are identified, and system-tool output handles split Unicode characters.
- **Automatic releases:** each push to `main` runs Windows tests and publishes verified EXE, ZIP and checksum assets. No manual tagging or workflow dispatch is needed.
- **Fixed:** category names in the Junk cleaner showed one letter per line on some screens.
- **Fixed:** badges and buttons on Startup apps and Installed apps were misaligned when an item had no file location or no uninstaller.
- **Fixed:** folders inside the Startup folder (such as Autoruns' `AutorunsDisabled`) were listed as startup apps.
- **New cleanup categories:** driver installer leftovers (AMD and NVIDIA), Microsoft Store app temp files, Steam shader cache across all your Steam libraries, and caches from other Chromium and Electron apps.
- **More developer caches:** uv, Gradle, Cargo and Electron.
- **No double counting:** a file that matches two categories is counted and cleaned only once.
- **Your choices are remembered:** Junk cleaner selections are saved between runs. New one-click "Recommended", "Everything found" and "None" selections.
- **Overview:** "Junk found" now shows recommended categories separately from optional ones.
- **Safer Recycle Bin moves:** you get a warning when files are on a USB or network drive, where Windows has no Recycle Bin and deletes permanently.
- **Activity log:** now includes history from earlier sessions.
- **App icon and version info** are now built into the .exe.

## Features

- **Overview:** a health score based on things that genuinely affect speed (free space, junk, memory pressure, startup apps and uptime), with one-click fixes.
- **Junk cleaner:** about 25 categories. Expand any category to see every file and untick what you want to keep.
  - Windows: temp files, Store app temp, thumbnails, crash dumps, Delivery Optimization, update downloads, old logs, shader caches and driver installer leftovers.
  - Apps: browser caches (Chrome, Edge, Firefox, Brave, Opera, Vivaldi), Discord, Teams, Slack, VS Code and Steam.
  - Developer package caches.
- **Space analyzer:** maps folders and drives as a clickable picture you can drill into. Tabs for the largest files, old and unused files, and space by file type.
- **Duplicate finder:** byte-for-byte comparison. One copy of every file is always kept.
- **Installed apps:** sorted by size or install date. Uninstall runs each app's own uninstaller.
- **Startup apps:** turn apps on or off at sign-in, using the same switch as Task Manager.
- **Memory and apps:** what is using your RAM and processor right now, grouped by app.
- **Tools:**
  - Create a restore point.
  - Clean up the component store (WinSxS).
  - Turn hibernation off or on.
  - Repair system files (SFC) and the Windows image (DISM).
  - Check the drive for errors and optimize/TRIM it.
  - Switch power plans and flush DNS.
  - Every tool shows its output live.
- **Activity log:** a record of everything PCSweep changed.

## Built to be safe

- **Your Downloads folder is never deleted from**, wherever it is, even if you moved it to another drive.
- The Junk cleaner can't touch Documents, Desktop, Pictures, Videos, Music or OneDrive.
- Your own files, from the analyzer and duplicate finder, go to the **Recycle Bin**, so you can restore them. Only junk caches are deleted permanently, and only after you confirm.
- System folders, program folders, the page file and the hibernation file can't be removed.
- Scans never follow junctions or shortcut folders.
- Files that are in use, or changed in the last 24 hours in temp folders, are skipped.
- Browser cleaning removes caches only. Passwords, history, cookies and tabs stay.

## Verify your download (optional)

`SHA256SUMS.txt` lists checksums. In PowerShell, run `Get-FileHash .\PCSweep.exe` and compare the result with the line for `PCSweep.exe`.

## Source and license

The source code is open in this repository under the MIT license. PCSweep is built automatically by GitHub Actions from the exact code you see here.
