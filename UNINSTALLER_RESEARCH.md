# Uninstaller design research — PCSweep 1.7

Primary source: [Bulk Crap Uninstaller](https://github.com/BCUninstaller/Bulk-Crap-Uninstaller), inspected at commit `30da609384c98ba6e35c6530129541ea4ff3970a`. BCU is Apache-2.0; its source headers credit Marcin Szeniak. PCSweep's implementation is independent Rust code, informed by its evidence and ownership design, rather than a port of its scanners.

Relevant upstream code:

- [ConfidenceCollection](https://github.com/BCUninstaller/Bulk-Crap-Uninstaller/blob/30da609384c98ba6e35c6530129541ea4ff3970a/source/UninstallTools/Junk/Confidence/ConfidenceCollection.cs): combines evidence, then maps scores to confidence levels.
- [ConfidenceRecords](https://github.com/BCUninstaller/Bulk-Crap-Uninstaller/blob/30da609384c98ba6e35c6530129541ea4ff3970a/source/UninstallTools/Junk/Confidence/ConfidenceRecords.cs): lowers confidence for shared directories, similar names, installed products and publishers; raises it for explicit product connections.
- [CommonDriveJunkScanner](https://github.com/BCUninstaller/Bulk-Crap-Uninstaller/blob/30da609384c98ba6e35c6530129541ea4ff3970a/source/UninstallTools/Junk/Finders/Drive/CommonDriveJunkScanner.cs): searches known locations with bounded depth and checks other install locations.
- [SoftwareRegKeyScanner](https://github.com/BCUninstaller/Bulk-Crap-Uninstaller/blob/30da609384c98ba6e35c6530129541ea4ff3970a/source/UninstallTools/Junk/Finders/Registry/SoftwareRegKeyScanner.cs): treats exact names and explicit paths as evidence, with exclusions for shared system registry roots.

## Shipping behavior

PCSweep runs the registered uninstaller on a worker, records the prior product identity/location, and lets the user check leftovers after removal. A refreshed desktop inventory must show that the product has disappeared. An uninstaller exiting does not prove removal: launchers can hand work to another process, and MSI may require a restart.

The review checks the inventoried product install folder, exact product folders in Local/Roaming AppData and ProgramData, and exact product or publisher/product Software registry keys in native and WOW6432Node locations. It shows the path, confidence and evidence. Nothing is preselected. An exact reported install folder without remaining executable code can have high confidence; app data, inferred locations, executable-bearing folders and registry keys require review.

Other installed apps' locations, product-name collisions, publisher-owned registry keys, shared Windows/runtime locations, personal libraries, Downloads, links, cloud placeholders and unsupported drives are excluded. The registry walker opens link objects rather than following symbolic links. Folder inspection is bounded to 100,000 entries/10 seconds and registry trees to 1 MiB/eight levels per key. This first version deliberately avoids fuzzy name matching, COM cleanup, arbitrary services/tasks, and forced uninstall.

Ownership and filesystem manifests are checked again before removal. Selected folders go to the Recycle Bin, with durable cleanup history. Registry trees retain exact raw value types and bytes in an atomic guarded local backup before deletion; the latest 16 cleanup generations are retained. Restore preserves differing values that appeared later and leaves a conflicted backup available for review. Registry actions also appear in Activity. These checks reduce mistakes; confidence cannot prove that no unregistered or portable app uses a folder.

## Responsiveness and validation

Scan JSON now uses 256 KiB I/O buffers; tests count reads/writes for a snapshot larger than 10 MB to catch accidental byte/token I/O. Disk traversal publishes its result before cache serialization. Restore can be skipped and detaches after 30 seconds without blocking the UI. Saved-map file selection validates current files without remapping a drive and rechecks them after confirmation.

Native Windows tests use disposable files, registry keys and HWNDs. They cover registry type round trips, preservation of later values, single-instance exclusion, hidden-window tray wake, changed-file protection and scan completion while saving is blocked. UI fixtures cover all 20 pages, including enlarged/bold variants. Actual installed third-party uninstallers and every Windows configuration cannot be certified by these tests.
