//! Bounded, noninteractive Windows command execution for background tasks.
use base64::Engine;
use std::io::Read;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Quote a single PowerShell literal. Never interpolate values as executable code.
pub fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Resolve Windows tools without searching the current folder or PATH.
pub fn system_directory() -> Result<std::path::PathBuf, String> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
    }
    let mut buffer = vec![0u16; 32768];
    let n = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if n == 0 || n >= buffer.len() {
        return Err("Windows system folder is unavailable.".into());
    }
    use std::os::windows::ffi::OsStringExt;
    Ok(std::path::PathBuf::from(std::ffi::OsString::from_wide(
        &buffer[..n],
    )))
}

pub fn windows_directory() -> Result<std::path::PathBuf, String> {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetSystemWindowsDirectoryW(buffer: *mut u16, size: u32) -> u32;
    }
    use std::os::windows::ffi::OsStringExt;
    let mut buffer = vec![0u16; 32768];
    let n =
        unsafe { GetSystemWindowsDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if n == 0 || n >= buffer.len() {
        return Err("Windows folder is unavailable.".into());
    }
    Ok(std::ffi::OsString::from_wide(&buffer[..n]).into())
}

/// Use the Windows trust store for outbound HTTPS. ureq's native TLS support
/// requires explicitly installing this connector on each agent.
pub fn https_agent() -> Result<ureq::AgentBuilder, String> {
    let tls = ureq::native_tls::TlsConnector::builder()
        .min_protocol_version(Some(ureq::native_tls::Protocol::Tlsv12))
        .build()
        .map_err(|_| "Windows could not initialize a secure HTTPS connection.".to_string())?;
    Ok(ureq::AgentBuilder::new()
        .https_only(true)
        .tls_connector(std::sync::Arc::new(tls))
        .redirects(0))
}

fn capture<R: Read + Send + 'static>(mut pipe: R) -> mpsc::Receiver<Result<Vec<u8>, String>> {
    let (send, recv) = mpsc::channel();
    std::thread::spawn(move || {
        const LIMIT: usize = 16 * 1024 * 1024;
        let mut result = Vec::new();
        let mut block = [0; 8192];
        let mut exceeded = false;
        loop {
            match pipe.read(&mut block) {
                Ok(0) => break,
                Ok(n) => {
                    if result.len() + n <= LIMIT {
                        result.extend_from_slice(&block[..n]);
                    } else {
                        // Keep draining so a child cannot block on a full pipe.
                        exceeded = true;
                    }
                }
                Err(error) => {
                    let _ = send.send(Err(error.to_string()));
                    return;
                }
            }
        }
        let _ = send.send(if exceeded {
            Err("The Windows command returned too much output.".into())
        } else {
            Ok(result)
        });
    });
    recv
}

/// Run an internal script on a worker thread. Output is UTF-8, execution has a
/// deadline, and command failure is never mistaken for an empty successful list.
pub fn powershell(script: &str, timeout: Duration) -> Result<String, String> {
    let script = format!(
        "$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; \
         [Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); \
         $OutputEncoding=[Console]::OutputEncoding; try {{ & {{ {script} }} }} \
         catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit 1 }}"
    );
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    use std::os::windows::process::CommandExt;
    let executable = system_directory()?.join(r"WindowsPowerShell\v1.0\powershell.exe");
    let mut command = std::process::Command::new(executable);
    command.creation_flags(crate::util::CREATE_NO_WINDOW);
    let mut child = command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &encoded,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Could not start Windows PowerShell: {error}"))?;
    let out = capture(child.stdout.take().ok_or("No command output pipe")?);
    let err = capture(child.stderr.take().ok_or("No command error pipe")?);
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(40))
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(
                    "The Windows command timed out. Refresh to check its current state.".into(),
                );
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Could not wait for the Windows command: {error}"));
            }
        }
    };
    let stdout = out
        .recv_timeout(Duration::from_secs(3))
        .map_err(|_| "Windows command output did not finish")??;
    let stderr = err
        .recv_timeout(Duration::from_secs(3))
        .map_err(|_| "Windows command error output did not finish")??;
    let decode = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .trim_start_matches('\u{feff}')
            .trim()
            .to_owned()
    };
    if !status.success() {
        let error = decode(&stderr);
        return Err(if error.is_empty() {
            format!("Windows command failed (exit code {:?}).", status.code())
        } else {
            error.chars().take(1600).collect()
        });
    }
    Ok(decode(&stdout))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literal_quoting_never_runs_embedded_commands() {
        let dangerous = "a'; Write-Output 'injected'; #";
        assert_eq!(
            powershell(
                &format!("Write-Output {}", ps_quote(dangerous)),
                Duration::from_secs(15)
            )
            .unwrap(),
            dangerous
        );
    }
    #[test]
    fn unicode_and_failed_commands_remain_distinct() {
        assert_eq!(
            powershell("Write-Output 'café 🧹'", Duration::from_secs(15)).unwrap(),
            "café 🧹"
        );
        assert!(
            powershell("throw 'expected failure'", Duration::from_secs(15))
                .unwrap_err()
                .contains("expected failure")
        );
    }
    #[test]
    fn command_deadlines_do_not_freeze_the_caller() {
        let start = Instant::now();
        assert!(powershell("Start-Sleep -Seconds 20", Duration::from_millis(150)).is_err());
        assert!(start.elapsed() < Duration::from_secs(10));
    }
}
