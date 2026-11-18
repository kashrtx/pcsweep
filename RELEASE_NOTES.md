# PCSweep 1.8.0

Memory and Background activity now share a live Windows process list. It includes processes with zero or unavailable memory readings, and groups apps by executable path and Windows session.

- **Find what is running.** Browse apps with open windows, windowless processes, or individual PIDs. Sort by RAM, CPU, private commit, growth, name, or PID. Search by app, exact PID, executable path, or window title.
- **Choose how to close.** Close normally gives apps a chance to prompt you to save. End task works for background processes and apps that stop responding. Both support selecting multiple processes and reviewing the exact PIDs. Windows critical processes and access restrictions are checked for each process.
- **See the app behind the process.** Native executable icons appear in the list. Process details show individual PIDs, memory readings, and open windows. Switch to a window or open the executable’s location.
- **Keep working while processes change.** Sampling, icon loading, and waiting for processes to exit run in the background. Results show which processes exited and why others could not be changed. Scheduled tasks and browser controls have their own tabs in Background activity.
