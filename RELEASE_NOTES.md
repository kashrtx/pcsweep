# PCSweep 1.3.0

This update adds three pages: **Ads and tracking**, **Connection health**, and **Battery and power**. Each explains what Windows reports and lets you review supported changes before applying them.

## Download

Download **PCSweep.exe** for the portable app, or **PCSweep-1.3.0-windows-x64.zip** with the README, license and release notes. Requires 64-bit Windows 10 or 11 and an OpenGL-capable graphics driver. Releases are currently unsigned, so Windows may show an unrecognized-app warning. Administrator rights are optional; DNS changes and some Windows diagnostics require elevation.

## Ads and tracking

- Open the official **uBlock Origin** page for Firefox, or **uBlock Origin Lite** for Chrome and Edge. Installation and permissions stay with the browser; PCSweep does not force extensions or report browser detection as proof of active blocking.
- Review optional classic DNS filtering with **AdGuard Public DNS**, or enter the IP addresses of an existing local resolver such as AdGuard Home/Pi-hole. PCSweep does not install a DNS service or change the router.
- Review the physical adapter, current settings and proposed IPv4/IPv6 servers before starting a **90-second trial**. An administrator-protected recovery record saves the original automatic/manual configuration before any change.
- Check resolver results, then choose **Keep** or **Restore now**. A separate temporary recovery process restores the original configuration if the trial is not kept, including when PCSweep closes. Kept changes retain Undo.
- Protect managed/encrypted DNS, VPN routes, remote sessions and domain-managed networks. Restore checks adapter identity and preserves DNS changes made outside PCSweep for review.

DNS filtering reduces requests to blocked domains; it cannot block every ad or guarantee anonymous browsing. Secure DNS in a browser, VPN routing and ads served from the same domain as content can bypass it. The chosen DNS provider receives DNS requests under its own privacy policy.

## Connection health

- Review physical adapters, negotiated send/receive rates, addresses, gateways and driver information. Available Wi-Fi details show signal quality and current link rates.
- Compare packet errors and discards across refreshes instead of treating lifetime counters as a current fault. Missing counters or driver details are reported as unavailable.
- Run small, optional gateway, DNS and HTTPS checks. A blocked ping is not automatically treated as a broken connection, and local link rates are not advertised as internet speed.
- Inspect driver-reported options and open Windows network tools. Adapter capabilities vary: PCSweep does not blindly disable IPv6, offloads, power management or other driver features.

## Battery and power

- See Windows-reported power source, charge, saver state and runtime estimate, plus reported capacity and cycle count where the device exposes them. Missing or unknown data stays unknown.
- On supported Windows builds, choose separate **plugged-in** and **on-battery** power preferences. Windows switches between those preferences when the power source changes; PCSweep need not remain open.
- On older builds, choose recognized power plans already advertised by Windows. Hidden/OEM plans are not created or forced, and legacy performance changes require external power.
- Save original preferences before changes, verify the configured result, and retain **Undo** across restarts. Preferences changed outside PCSweep are preserved for review. Windows policy and manufacturer signals can still affect the effective mode.
- Create local **battery health** and **SleepStudy** reports, or inspect sleep blockers, the last wake event and wake timers. SleepStudy needs a compatible Modern Standby device; some diagnostics need administrator rights. These reports do not change sleep modes or timers.
- Open Windows Energy Saver and battery-care guidance. Charging limits and Smart Charging depend on the manufacturer; no universal firmware charge cap is promised.

Performance preferences can increase heat, fan noise and battery use; efficiency can reduce throughput. Reported battery capacity depends on device calibration and is not a guaranteed runtime.

## Reliability and validation

- Keep new work off the UI thread, serialize confirmed changes with existing system operations, and retain the cleaner's Downloads and personal-folder protections.
- Recheck reviewed identities and values, use persistent recovery records and readback, and show actionable partial-failure states.
- Keep long confirmation content scrollable while its action buttons remain reachable. New pages use the existing cards, spacing and navigation.
- The Windows workflow runs regression tests, read-only native API checks, PowerShell parser checks, and UI rendering at 1040×680 and 1320×860 before building and packaging the release. Mutation tests use fake state or disposable resources; they do not change real DNS, power or security settings.

Existing cleaning, disk analysis, duplicate detection, reviewed app removal, Defender checks, privacy controls, memory tools and verified in-app updates remain available. Update checks are manual by default, with optional daily checks while PCSweep is open.

## Automatic release and local data

Pushing a commit to `main` publishes only after Windows tests, the optimized build and packaging checks pass. The release includes the EXE, portable ZIP, **SHA256SUMS.txt** and **update.json**. Hash verification trusts the official GitHub release; independent code-signing verification is not claimed.

Logs, settings and staged updates live in `%LOCALAPPDATA%\PCSweep`; local power reports are in its `reports` folder and can include device/usage details. Power Undo lives in `HKEY_CURRENT_USER\Software\PCSweep\PowerUndo`, privacy Undo in `HKEY_CURRENT_USER\Software\PCSweep\PrivacyUndo`, and DNS recovery in `HKEY_LOCAL_MACHINE\SOFTWARE\PCSweep\DnsUndo`. No background service is installed and PCSweep does not upload diagnostic reports. Source is available under the MIT license.
