# PCSweep

A fast, careful cleanup and PC health tool for Windows 11, written in Rust.

PCSweep shows you exactly where your disk space goes, finds junk that is safe to remove, and only deletes after you've reviewed it and confirmed.

## Getting the app (no Rust needed on your PC)

GitHub builds the app for you every time you push code.

1. Create a new, **empty** repository on GitHub (don't add a README, license or .gitignore). Call it `pcsweep`.
2. Unzip this project, open a terminal in the `pcsweep` folder and run:

   ```
   git remote add origin https://github.com/YOUR-USERNAME/pcsweep.git
   git push -u origin main
   ```

3. On GitHub, open the **Actions** tab. Wait for "Build PCSweep" to finish (about 5 to 10 minutes the first time, faster afterwards).
4. Open the finished run and download **PCSweep-windows-x64** under Artifacts. Inside is `PCSweep.exe`. It's a single file with no installer.

**Optional: a proper download page.** Tag a version and GitHub publishes a Release with the .exe attached:

```
git tag v1.0.0
git push origin v1.0.0
```

**First launch:** Windows SmartScreen may warn about an unrecognized app, because the .exe isn't code-signed. Click **More info**, then **Run anyway**.

**Admin rights:** PCSweep runs fine as a normal user. Use **Restart as admin** in the sidebar to also clean system folders like Windows Temp and update caches.

## What it does

- **Overview:** a health score based on things that genuinely affect speed (free space, junk, memory pressure, startup load, uptime), with one-click fixes.
- **Junk cleaner:** about 20 categories covering user and Windows temp, thumbnail cache, crash dumps and error reports, Delivery Optimization, Windows Update downloads, old logs, shader caches, browser caches (Chrome, Edge, Firefox, Brave, Opera, Vivaldi), Discord, Teams, Slack, VS Code, Steam, and developer package caches. Expand any category to review every file and uncheck what you want to keep.
- **Space analyzer:** a parallel scan of a whole drive in seconds, drawn as a clickable map you can drill into. Tabs for the largest files, old and unused files (filter by age and size), and a breakdown by file type.
- **Duplicates:** byte-for-byte duplicate detection. One copy of each file is always kept.
- **Installed apps:** sorted by size or install date, with each app's own uninstaller.
- **Startup apps:** on/off switches that work exactly like Task Manager's.
- **Memory and apps:** what is using your RAM and processor, grouped by app.
- **Tools:** restore point, component store cleanup (WinSxS), turn hibernation off/on, System File Checker, DISM repair, drive error check, TRIM/optimize, power plans and DNS flush, with live output.
- **Activity log:** everything PCSweep changed, also saved to `%LOCALAPPDATA%\PCSweep\activity.log`.

## Safety rules (built into the code, not just the UI)

- **Downloads is never touched.** Nothing in your Downloads folder can be deleted or moved by any feature, including Downloads folders you have moved to another drive. You can still see those files in the analyzer, marked with a lock.
- The junk cleaner can't delete anything inside Documents, Desktop, Pictures, Videos, Music or OneDrive, even if a cache path somehow pointed there.
- Nothing is deleted without a confirmation dialog that shows what will happen.
- Your own files (from the analyzer and duplicate finder) go to the **Recycle Bin**, so they can be restored. Only junk caches are deleted permanently.
- System and program folders, the page file and the hibernation file can't be removed from the analyzer.
- Scans never follow junctions or symbolic links, so nothing outside the chosen folder is counted or cleaned.
- Temp files changed in the last 24 hours are skipped, because they are likely in use. Files that are locked are skipped automatically.
- Browser cleaning removes caches only. Passwords, history, cookies and open tabs are kept.
- Startup changes only switch an app on or off; nothing is deleted.
- Core Windows processes can't be ended from the Memory page.

## Honest notes on "PC speed tricks"

- **Clearing Prefetch** makes apps launch slower for a while and frees almost nothing. PCSweep includes it, but it's off by default with a warning.
- **"RAM cleaners"** don't speed anything up. Windows uses spare memory as a cache, and forcing it empty just makes apps reload. What really helps is closing heavy apps and stopping them from launching at startup, and PCSweep shows you which ones those are.
- **Free disk space** matters: Windows slows down and updates fail when the system drive is nearly full. That's where PCSweep spends most of its effort.

## Building locally (optional)

If you ever install Rust: `cargo build --release`, and the app is at `target\release\PCSweep.exe`.

## If the GitHub build fails

Open the failed run in the Actions tab, copy the red error lines from the **Build release** step, and share them so they can be fixed.
