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
