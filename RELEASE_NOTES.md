# PCSweep 1.5.0

This release improves responsiveness, file review, memory accuracy and multi-app closing; adds cleanup history, Windows change evidence, disk health, Windows controls, and an optional native internet speed test.

## Review and responsiveness

- Cleaner file review fills the page, with search, sorting, clear filtered selection actions and remembered choices.
- Memory has compact summaries and multi-selection. Normal close requests preserve save prompts; results distinguish verified exits, still-running apps and failed requests. Reviewed process identity is rechecked.
- Private committed memory comes from the exact Windows process instance. Unavailable readings remain unavailable, preventing virtual-address-space values being misreported as negative-terabyte growth.
- Drive and process polling runs in background workers, including failure/retry paths. Scans use bounded worker pools and nonblocking progress reads. Cancellation acknowledges stopping, and large results are released away from the window thread.
- Shared scrollbars reserve an edge gutter; confirmation dialogs have one content scroll and persistent Cancel/Confirm actions. Short arming delays protect destructive confirmations. Compact viewports fit the interface without changing normal-size layouts.

## Cleanup history and Windows evidence

- File removals, recycling, empty folders and Recycle Bin batches record intentions before work and outcomes afterward. Failed journal persistence stops further removals; interrupted outcomes remain uncertain.
- Guarded journal paths, exclusive writers, bounded streaming reads and fail-closed rotation protect local history. Search/filter and CSV/JSON export are available through Activity log → Cleanup history.
- Windows file changes reads existing NTFS records in bounded batches and existing file-access audit events. It does not enable auditing, create a journal, recover file contents, or promise a complete history. NTFS does not record the responsible program; audit access evidence is labeled separately. Parent paths are best-effort current paths.
- Windows File History is an optional backup feature, distinct from change journals and PCSweep's own history. Permanent cache deletion cannot be undone by a restore point.

## Disk health and maintenance

- Overview links to a detailed storage page: mapped volumes/disks, reported health and available temperature, wear/error counters, free space, and native schedule information.
- Unsupported health data stays unknown. Free space is not physical-health proof. OS TRIM notification policy is separate from device support and successful retrim.
- Review Analyze or Windows-managed Optimize for a supported, revalidated volume. Critical warnings, unsupported/ambiguous mappings and offline/read-only volumes lead to native/vendor review. No universal SSD defrag or guessed overdue badge.

## Windows controls and change timeline

- Supported animation preferences have persistent exact Undo.
- Unmanaged supported editions can review manual/notify update policy and driver-update exclusion with protected machine Undo. Existing unowned policy is preserved.
- Current policy values and legacy Windows Update API reports are read back, with explicit limits: they do not prove immediate modern Windows enforcement. Native pause/active-hours/settings links remain available on unsupported editions.
- The local What changed timeline reads recorded Windows Update and reliability events; events are not proof of causation and missing sources are explained.

## Optional internet speed test

- Native M-Lab NDT7 download/upload testing uses synthetic traffic, quick/longer data caps, cancellation and live progress. Idle timing uses TCP connects; loaded timing uses server TCP estimates. Packet loss is not measured.
- M-Lab publishes results including the public IP address and retains measurement data indefinitely. Every run requires consent. PCSweep sends its version identifier but no inventory, history or user files. See PRIVACY.md and the provider privacy/usage links.
- History is optional and limited to this app session. PCSweep limits this account to 20 runs per UTC day across restarts and waits between runs; other clients sharing the IP also count toward provider limits. No speed tests run automatically.

## Licensing, validation and download

Apache License 2.0; copyright 2026 Kaushal Bhingaradia (kashrtx). LICENSE and NOTICE are included with downloads. Earlier releases retain their original license.

The release workflow checks formatting, runs Windows application and publisher tests, renders reviewable UI previews, then builds and packages the optimized x64 executable. Mutation tests use fake state or disposable resources; they do not change the runner's real update/security preferences or optimize its system disk. Home/Pro physical-machine enforcement and every hardware/controller combination have not been certified by Windows Server CI.

Download PCSweep.exe or the portable ZIP. Requires 64-bit Windows 10/11 and an OpenGL-capable graphics driver. Releases are unsigned. Only a passing main push publishes the release, with SHA256SUMS.txt and the update manifest; no manual tagging is required.
