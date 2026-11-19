# PCSweep privacy and optional speed testing

PCSweep stores preferences, activity logs, cleanup history, Undo records and optional diagnostic reports locally. It does not send these records or device inventory to us. Exported history and reports can contain sensitive paths; export/open actions are user initiated.

## M-Lab NDT7 speed test

Speed tests run only after explicit consent and confirmation. PCSweep contacts the official M-Lab locator and selected M-Lab measurement servers. It sends synthetic test payloads and an app/version User-Agent. It does not upload personal files, cleanup history, process lists, hardware identifiers or diagnostic reports.

M-Lab receives connection information including the public IP address, measurement traffic, results and test time. M-Lab publishes measurement data, including the IP address, for research and retains it indefinitely. A VPN changes the measured route/public IP. PCSweep cannot withdraw measurements already sent. Do not run a test if you do not accept this processing.

- Provider privacy: https://www.measurementlab.net/privacy/
- Provider acceptable use: https://www.measurementlab.net/aup/
- NDT documentation: https://www.measurementlab.net/tests/ndt/

Data caps describe application test payload; protocol overhead and an in-flight frame also use data. Cancel stops further work as soon as outstanding connection operations return. Results optionally stay in this open app session and are not uploaded by PCSweep to any additional service. A local per-account UTC-day counter limits attempts across app restarts; provider limits also cover other clients using the same IP.

## Other network features

Official GitHub update checks/downloads, consented WinGet/Defender operations, optional connection probes and browser links contact their respective providers. DNS trials send DNS queries to the chosen resolver; that provider's privacy terms apply. These features do not make browsing anonymous. No background monitoring service is installed.

## Windows change records

Windows file-change and audit views read existing local records on request. They do not enable new global auditing. Some paths/program evidence is sensitive; the results remain local. Journal retention and audit coverage are limited, so absent entries do not prove that nothing changed.

## Saved scans and background mode

Analyzer, cleaner and duplicate snapshots are stored locally in the scan-cache folder beside the activity log. They can contain filenames and folder paths. Snapshots have bounded retention, are not uploaded, and are shown with their saved time. Restored cleaner/duplicate results require a refresh before deletion. Space analyzer selections can instead validate the selected files on disk, then recheck them before moving; they do not require remapping the full drive. The last page, window size and close-to-notification-area preference are saved locally.

Notification-area mode is optional. It keeps this process running; it does not install a service or Windows startup entry. Optional daily GitHub update checks can continue while the window is hidden. Installation remains explicit. Update-reminder dismissal and the last recorded Windows installation age are local preferences; no missing-update catalog lookup runs automatically.

Native application artwork is read from local app resources or the Windows Shell on workers. PCSweep does not launch an uninstaller or contact an icon-download service to obtain it.

## Uninstaller leftovers and readability

PCSweep remembers the last desktop product submitted for uninstall, including its name, publisher and location, locally. Reviewed registry cleanup retains the latest 16 typed-value backup generations in the guarded local scan-cache folder. Backups can contain sensitive application settings; they are not uploaded. Folder removals are recorded in local cleanup history and use the Recycle Bin on supported fixed local drives. The theme, interface size, bold text and optional UAC startup preferences are stored per account. Quick search queries and the recently closed apps list stay in memory for the open session. Reopen uses the verified executable under the desktop account and launches it with fresh arguments.
