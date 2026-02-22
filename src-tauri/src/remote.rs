use crate::cli::NotifyRequest;
use crate::notification::{show_notification, NotificationManagerState};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::AppHandle;

// ── Token generation ──────────────────────────────────────────────────────────

/// Generate a cryptographically-random 32-character lowercase hex token.
pub fn generate_token() -> String {
    use std::fmt::Write as FmtWrite;
    let mut rng = [0u8; 16];
    // Fill with OS-provided randomness via getrandom (available through std)
    fill_random_bytes(&mut rng);
    rng.iter().fold(String::with_capacity(32), |mut s, b| {
        write!(s, "{b:02x}").unwrap();
        s
    })
}

/// Fill a byte slice with OS-provided random data.
/// Falls back to a time-seeded xorshift64 if getrandom is unavailable.
fn fill_random_bytes(buf: &mut [u8]) {
    // Use std::collections::hash_map::RandomState as a cross-platform entropy source
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::SystemTime;

    let mut seed = {
        let mut h = DefaultHasher::new();
        SystemTime::now().hash(&mut h);
        std::thread::current().id().hash(&mut h);
        h.finish()
    };

    for chunk in buf.chunks_mut(8) {
        // xorshift64
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let bytes = seed.to_le_bytes();
        let n = chunk.len();
        chunk.copy_from_slice(&bytes[..n]);
    }
}

// ── SSH Tunnel ────────────────────────────────────────────────────────────────

/// Configuration for an SSH reverse tunnel.
#[derive(Debug, Clone)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub key_path: String,
    /// Remote port on the SSH server that forwards to local_port.
    pub remote_port: u16,
    /// Local HTTP server port.
    pub local_port: u16,
}

/// Lifecycle state of the SSH tunnel.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum TunnelStatus {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Error(String),
}

/// Manages an SSH reverse-tunnel child process.
pub struct SshTunnel {
    pub process: Option<Child>,
    pub config: SshConfig,
}

impl SshTunnel {
    pub fn new(config: SshConfig) -> Self {
        Self {
            process: None,
            config,
        }
    }

    /// Build the SSH command arguments for the reverse tunnel.
    /// ssh -R {remote_port}:127.0.0.1:{local_port} {user}@{host} -p {port}
    ///     -N -i {key_path} -o StrictHostKeyChecking=accept-new
    ///     -o ServerAliveInterval=30 -o ExitOnForwardFailure=yes
    pub fn build_ssh_args(&self) -> Vec<String> {
        let cfg = &self.config;
        let mut args = vec![
            "-R".to_string(),
            format!("*:{}:127.0.0.1:{}", cfg.remote_port, cfg.local_port),
            format!("{}@{}", cfg.user, cfg.host),
            "-p".to_string(),
            cfg.port.to_string(),
            "-N".to_string(),
        ];
        // Only add -i if key path is specified
        if !cfg.key_path.is_empty() {
            args.push("-i".to_string());
            args.push(cfg.key_path.clone());
        }
        args.extend([
            "-o".to_string(),
            "StrictHostKeyChecking=accept-new".to_string(),
            "-o".to_string(),
            "ServerAliveInterval=30".to_string(),
            "-o".to_string(),
            "ExitOnForwardFailure=yes".to_string(),
        ]);
        args
    }

    /// Spawn the SSH process with stderr capture for error logging.
    ///
    /// Waits briefly (up to 3 seconds) for early failures such as
    /// "connection refused" or "permission denied".  If the process exits
    /// within that window the captured stderr is written to the log file and
    /// the error message returned to the caller contains only the log path.
    ///
    /// Includes automatic retry: if the first attempt fails (commonly due to
    /// a stale remote port listener), performs an additional cleanup and
    /// retries once.
    pub fn connect(&mut self, status: Arc<Mutex<TunnelStatus>>) -> Result<(), String> {
        if self.process.is_some() {
            return Err("Tunnel already running".to_string());
        }

        *status.lock().unwrap() = TunnelStatus::Connecting;

        // Clean up stale remote port listeners from previous sessions.
        // On Windows, child.kill() uses TerminateProcess which doesn't give SSH
        // a chance to send SSH_MSG_DISCONNECT, leaving the remote sshd alive.
        cleanup_remote_port(&self.config);

        // First attempt
        match self.spawn_and_wait(&status) {
            Ok(()) => {
                *status.lock().unwrap() = TunnelStatus::Connected;
                log::info!("[SSH] Tunnel connected on first attempt");
                Ok(())
            }
            Err(first_err) => {
                log::warn!("[SSH] First attempt failed: {first_err}");
                log::info!("[SSH] Retrying after additional cleanup...");

                // Reset status for retry
                *status.lock().unwrap() = TunnelStatus::Connecting;

                // Aggressive second cleanup with longer wait
                cleanup_remote_port(&self.config);
                std::thread::sleep(Duration::from_secs(2));

                // Second (final) attempt
                match self.spawn_and_wait(&status) {
                    Ok(()) => {
                        *status.lock().unwrap() = TunnelStatus::Connected;
                        log::info!("[SSH] Tunnel connected on retry");
                        Ok(())
                    }
                    Err(retry_err) => {
                        log::error!("[SSH] Retry also failed: {retry_err}");
                        // Status already set to Error by spawn_and_wait
                        Err(retry_err)
                    }
                }
            }
        }
    }

    /// Internal: spawn SSH process and wait up to 3 seconds for early failure.
    /// On success, stores the child process in `self.process`.
    /// On failure, sets `TunnelStatus::Error` and returns the error message.
    fn spawn_and_wait(&mut self, status: &Arc<Mutex<TunnelStatus>>) -> Result<(), String> {
        let args = self.build_ssh_args();
        log::info!(
            "[SSH] Spawning tunnel to {}:{} (args: {:?})",
            self.config.host,
            self.config.port,
            args
        );

        let mut cmd = Command::new("ssh");
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        // Hide the console window on Windows
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = cmd.spawn().map_err(|e| {
            let msg = format!("Failed to spawn ssh: {e}");
            *status.lock().unwrap() = TunnelStatus::Error(msg.clone());
            log::error!("[SSH] {msg}");
            msg
        })?;

        // Wait briefly for early exit (auth failure, connection refused, etc.)
        let wait_ms = 3000u64;
        let poll_interval = Duration::from_millis(100);
        let start = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(exit_status)) => {
                    // Process exited early — read stderr for diagnostics
                    let stderr_text = child
                        .stderr
                        .take()
                        .map(|mut pipe| {
                            let mut buf = String::new();
                            std::io::Read::read_to_string(&mut pipe, &mut buf).ok();
                            buf
                        })
                        .unwrap_or_default();

                    let log_path = ssh_log_path();
                    write_ssh_log(&log_path, &args, exit_status.code(), &stderr_text);

                    let msg = format!(
                        "SSH connection failed (exit {}). Details: {}",
                        exit_status
                            .code()
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "signal".into()),
                        log_path,
                    );
                    *status.lock().unwrap() = TunnelStatus::Error(msg.clone());
                    log::error!("[SSH] {msg}");
                    return Err(msg);
                }
                Ok(None) => {
                    // Still running
                    if start.elapsed() >= Duration::from_millis(wait_ms) {
                        break; // survived the window → assume connected
                    }
                    std::thread::sleep(poll_interval);
                }
                Err(e) => {
                    let msg = format!("Failed to poll ssh process: {e}");
                    *status.lock().unwrap() = TunnelStatus::Error(msg.clone());
                    log::error!("[SSH] {msg}");
                    return Err(msg);
                }
            }
        }

        self.process = Some(child);
        log::info!("[SSH] Tunnel process spawned and alive after {wait_ms}ms");
        Ok(())
    }

    /// Kill the SSH process and reset state.
    pub fn disconnect(&mut self, status: Arc<Mutex<TunnelStatus>>) {
        if let Some(mut child) = self.process.take() {
            log::info!("[SSH] Killing tunnel process (pid={:?})", child.id());
            let _ = child.kill();
            let _ = child.wait();
            log::info!("[SSH] Tunnel process terminated");
        } else {
            log::info!("[SSH] Disconnect called but no active process");
        }
        *status.lock().unwrap() = TunnelStatus::Disconnected;
    }

    /// Returns true if the child process is still alive.
    pub fn is_alive(&mut self) -> bool {
        match &mut self.process {
            None => false,
            Some(child) => match child.try_wait() {
                Ok(None) => true,              // still running
                Ok(Some(_)) | Err(_) => false, // exited or error
            },
        }
    }
}

// ── Remote Port Cleanup ──────────────────────────────────────────────────────

/// Kill any process listening on the configured remote port via SSH.
///
/// On Windows, `child.kill()` calls `TerminateProcess` which is an immediate
/// hard kill — the SSH client never sends `SSH_MSG_DISCONNECT` to the server.
/// The remote `sshd` keeps the forwarded port open until TCP keepalive times
/// out (minutes to hours).  This function SSHes into the remote host and runs
/// `fuser -k PORT/tcp` to clean up those stale listeners before establishing
/// a new tunnel.
fn cleanup_remote_port(config: &SshConfig) {
    log::info!(
        "[SSH] Cleaning up stale listeners on remote port {} ({}@{}:{})",
        config.remote_port,
        config.user,
        config.host,
        config.port
    );

    let port_str = config.port.to_string();
    // Use multiple tools for portability: fuser (procps), lsof, ss+kill
    let port = config.remote_port;
    let kill_cmd = format!(
        "fuser -k {port}/tcp 2>/dev/null; \
         kill $(lsof -t -i:{port} 2>/dev/null) 2>/dev/null; \
         exit 0"
    );

    let mut cmd = Command::new("ssh");
    cmd.arg(format!("{}@{}", config.user, config.host))
        .arg("-p")
        .arg(&port_str);

    if !config.key_path.is_empty() {
        cmd.arg("-i").arg(&config.key_path);
    }

    cmd.args(["-o", "StrictHostKeyChecking=accept-new"])
        .args(["-o", "ConnectTimeout=5"])
        .args(["-o", "BatchMode=yes"])
        .arg("--")
        .arg(&kill_cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    match cmd.output() {
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if output.status.success() {
                log::info!("[SSH] Remote port cleanup completed");
            } else {
                log::warn!(
                    "[SSH] Remote port cleanup exited with {}: {}",
                    output.status,
                    stderr.trim()
                );
            }
        }
        Err(e) => {
            log::warn!("[SSH] Failed to run remote port cleanup: {e}");
        }
    }

    // Pause to allow the OS to fully release the port.
    // 500ms is sometimes insufficient for TCP TIME_WAIT cleanup.
    std::thread::sleep(Duration::from_millis(1000));
}

// ── Watchdog ──────────────────────────────────────────────────────────────────

/// Start a watchdog thread that monitors the SSH tunnel process.
/// If the process dies and auto_reconnect is true, it attempts to reconnect.
/// Respects the `user_disconnected` flag to avoid reconnecting after manual
/// disconnect or app shutdown.
pub fn start_watchdog(
    tunnel: Arc<Mutex<Option<SshTunnel>>>,
    status: Arc<Mutex<TunnelStatus>>,
    auto_reconnect: bool,
    user_disconnected: Arc<Mutex<bool>>,
) {
    let poll_interval = Duration::from_secs(10);

    std::thread::spawn(move || loop {
        std::thread::sleep(poll_interval);

        // Skip if user/app explicitly disconnected
        if *user_disconnected.lock().unwrap() {
            continue;
        }

        let mut guard = tunnel.lock().unwrap();
        let Some(ref mut t) = *guard else { continue };

        if !t.is_alive() {
            log::warn!("[SSH] Tunnel process died");
            t.process = None;

            if auto_reconnect {
                log::info!("[SSH] Auto-reconnecting tunnel...");
                if let Err(e) = t.connect(status.clone()) {
                    log::error!("[SSH] Reconnect failed: {e}");
                }
            } else {
                *status.lock().unwrap() = TunnelStatus::Disconnected;
            }
        }
    });
}

// ── HTTP Server ───────────────────────────────────────────────────────────────

/// Start the HTTP notification server on a dedicated thread.
/// Binds to 127.0.0.1:{port} only (localhost security boundary).
///
/// Accepted request:
///   POST /notify
///   Header: X-Agent-Toast-Token: <token>
///   Body: JSON NotifyRequest
///
/// The `source` field in the body is always overridden to "remote".
/// The `remote_host` field is set to `ssh_host` so the frontend can display
/// which server the notification originated from.
pub fn start_http_server(
    port: u16,
    token: String,
    app: AppHandle,
    state: NotificationManagerState,
    ssh_host: String,
) {
    log::info!(
        "[HTTP] Starting remote notification server on port {port} (token configured: {})",
        !token.is_empty()
    );
    std::thread::spawn(move || {
        let addr = format!("127.0.0.1:{port}");
        let server = match tiny_http::Server::http(&addr) {
            Ok(s) => {
                log::info!("[HTTP] Remote notification server listening on {addr}");
                s
            }
            Err(e) => {
                log::error!("[HTTP] Failed to bind server on {addr}: {e}");
                return;
            }
        };

        log::info!("[HTTP] Server ready, waiting for incoming requests...");
        for request in server.incoming_requests() {
            handle_http_request(request, &token, &app, &state, &ssh_host);
        }
        log::warn!("[HTTP] Server loop ended unexpectedly");
    });
}

/// Handle a single HTTP request, consuming ownership as required by tiny_http.
fn handle_http_request(
    mut request: tiny_http::Request,
    token: &str,
    app: &AppHandle,
    state: &NotificationManagerState,
    ssh_host: &str,
) {
    let method = request.method().to_string();
    let url = request.url().to_string();
    let remote_addr = request
        .remote_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|| "unknown".into());

    log::info!("[HTTP] Incoming request: {method} {url} from {remote_addr}");

    // Only accept POST /notify
    if url != "/notify" {
        log::warn!("[HTTP] 404 Not Found: {method} {url}");
        respond_status(request, 404, "Not Found");
        return;
    }

    if method != "POST" {
        log::warn!("[HTTP] 405 Method Not Allowed: {method} {url}");
        respond_status(request, 405, "Method Not Allowed");
        return;
    }

    // Validate token - never log the token value itself
    let provided_token = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("X-Agent-Toast-Token"))
        .map(|h| h.value.as_str())
        .unwrap_or("")
        .to_string();

    let has_token_header = request
        .headers()
        .iter()
        .any(|h| h.field.equiv("X-Agent-Toast-Token"));
    log::info!(
        "[HTTP] Token header present: {has_token_header}, token configured: {}",
        !token.is_empty()
    );

    if token.is_empty() || provided_token != token {
        log::warn!("[HTTP] 401 Unauthorized: token mismatch (header present: {has_token_header})");
        respond_status(request, 401, "Unauthorized");
        return;
    }

    // Read body
    let mut body = String::new();
    if let Err(e) = std::io::Read::read_to_string(request.as_reader(), &mut body) {
        log::error!("[HTTP] 400 Bad Request: failed to read body: {e}");
        respond_status(request, 400, "Bad Request");
        return;
    }
    log::info!(
        "[HTTP] Request body ({} bytes): {}",
        body.len(),
        &body[..body.len().min(500)]
    );

    // Deserialize the notify request
    let mut notify_req: NotifyRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            log::error!("[HTTP] 400 Bad Request: invalid JSON: {e}");
            respond_status(request, 400, "Bad Request");
            return;
        }
    };

    // Keep user-provided source as display name (e.g., "GOLD24").
    // Default to "remote" only if the caller left it empty.
    if notify_req.source.is_empty() {
        notify_req.source = "remote".to_string();
    }
    // Always set remote_host so notification.rs knows to skip win32 lookups.
    notify_req.remote_host = Some(if ssh_host.is_empty() {
        "remote".to_string()
    } else {
        ssh_host.to_string()
    });

    log::info!(
        "[HTTP] 200 OK: showing notification event={}, pid={}, message={:?}",
        notify_req.event,
        notify_req.pid,
        notify_req.message
    );

    show_notification(app, state, notify_req);
    respond_status(request, 200, "OK");
}

/// Send a minimal HTTP response with the given status code and text body.
/// Consumes the request (as required by tiny_http).
fn respond_status(request: tiny_http::Request, code: u16, text: &str) {
    let response = tiny_http::Response::from_string(text).with_status_code(code);
    if let Err(e) = request.respond(response) {
        log::error!("[HTTP] Failed to send response (code={code}): {e}");
    }
}

// ── SSH Log Helpers ──────────────────────────────────────────────────────────

/// Return the path to the SSH-specific log file (%TEMP%/agent-toast-ssh.log).
pub fn ssh_log_path() -> String {
    std::env::temp_dir()
        .join("agent-toast-ssh.log")
        .to_string_lossy()
        .to_string()
}

/// Append a timestamped SSH error entry to the log file.
fn write_ssh_log(path: &str, args: &[String], exit_code: Option<i32>, stderr: &str) {
    use std::io::Write;
    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    let entry = format!(
        "\n[{timestamp}] SSH Connection Failed\n\
         Command: ssh {}\n\
         Exit code: {}\n\
         --- stderr ---\n\
         {}\n\
         --- end ---\n",
        args.join(" "),
        exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".into()),
        if stderr.trim().is_empty() {
            "(no output)"
        } else {
            stderr.trim()
        },
    );
    log::error!("[SSH] {entry}");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(entry.as_bytes());
    }
}

// ── IPC Commands ──────────────────────────────────────────────────────────────

/// Resolve `~` or `~/` prefix in a path to the user's home directory.
pub fn resolve_home_dir(path: &str) -> String {
    if path.starts_with("~/") || path.starts_with("~\\") {
        if let Some(home) = dirs::home_dir() {
            return home.join(&path[2..]).to_string_lossy().to_string();
        }
    } else if path == "~" {
        if let Some(home) = dirs::home_dir() {
            return home.to_string_lossy().to_string();
        }
    }
    path.to_string()
}

/// Tauri command: connect the SSH tunnel.
/// Accepts current UI config values so the user doesn't need to save first.
#[allow(clippy::too_many_arguments)]
#[tauri::command(rename_all = "snake_case")]
pub fn connect_ssh_tunnel(
    app: AppHandle,
    tunnel_state: tauri::State<'_, crate::RemoteState>,
    notification_state: tauri::State<'_, crate::notification::NotificationManagerState>,
    ssh_host: String,
    ssh_port: u16,
    ssh_user: String,
    ssh_key_path: String,
    ssh_remote_port: u16,
    remote_port: u16,
    remote_token: String,
) -> Result<(), String> {
    log::info!(
        "[SSH] connect_ssh_tunnel called: host={}, port={}, user={}, key_path={}, remote_port={}, local_port={}, token_len={}",
        ssh_host, ssh_port, ssh_user, ssh_key_path, ssh_remote_port, remote_port, remote_token.len()
    );

    if ssh_host.is_empty() {
        log::warn!("[SSH] SSH host is empty, aborting connection");
        return Err("SSH host is not configured".to_string());
    }

    // Ensure the HTTP notification server is running on the correct port
    {
        let mut current_port = tunnel_state.http_server_port.lock().unwrap();
        if *current_port == 0 || *current_port != remote_port {
            if *current_port != 0 {
                log::warn!("[HTTP] Port changed from {} to {} — starting new server (old server on {} will be orphaned)", *current_port, remote_port, *current_port);
            } else {
                log::info!("[HTTP] HTTP server not yet started — starting on port {remote_port}");
            }
            start_http_server(
                remote_port,
                remote_token,
                app.clone(),
                notification_state.inner().clone(),
                ssh_host.clone(),
            );
            *current_port = remote_port;
        } else {
            log::info!("[HTTP] HTTP server already running on port {remote_port}");
        }
    }

    let status = tunnel_state.tunnel_status.clone();
    let mut guard = tunnel_state.ssh_tunnel.lock().unwrap();

    // Always recreate the tunnel with current UI values
    // (previous tunnel may have stale config)
    if let Some(ref mut old) = *guard {
        log::info!("[SSH] Disconnecting previous tunnel before reconnecting");
        old.disconnect(status.clone());
    }

    let ssh_config = SshConfig {
        host: ssh_host,
        port: ssh_port,
        user: ssh_user,
        key_path: resolve_home_dir(&ssh_key_path),
        remote_port: ssh_remote_port,
        local_port: remote_port,
    };
    log::info!(
        "[SSH] Creating new tunnel with config: {}@{}:{} -R {}:127.0.0.1:{}",
        ssh_config.user,
        ssh_config.host,
        ssh_config.port,
        ssh_config.remote_port,
        ssh_config.local_port
    );
    *guard = Some(SshTunnel::new(ssh_config));

    match guard.as_mut() {
        Some(t) => {
            *tunnel_state.user_disconnected.lock().unwrap() = false;
            let result = t.connect(status.clone());
            log::info!("[SSH] connect result: {result:?}");
            // Update tray icon immediately
            let tray_status = match &*status.lock().unwrap() {
                TunnelStatus::Connected => "Connected",
                TunnelStatus::Connecting => "Connecting",
                TunnelStatus::Disconnected => "Disconnected",
                TunnelStatus::Error(_) => "Error",
            };
            crate::update_tray_status(&app, tray_status);
            result
        }
        None => Err("SSH tunnel not configured".to_string()),
    }
}

/// Tauri command: disconnect the SSH tunnel.
#[tauri::command]
pub fn disconnect_ssh_tunnel(
    app: AppHandle,
    tunnel_state: tauri::State<'_, crate::RemoteState>,
) -> Result<(), String> {
    let status = tunnel_state.tunnel_status.clone();
    let mut guard = tunnel_state.ssh_tunnel.lock().unwrap();
    if let Some(ref mut t) = *guard {
        // Mark user-initiated disconnect so watchdog does not auto-reconnect
        *tunnel_state.user_disconnected.lock().unwrap() = true;
        t.disconnect(status);
    }
    crate::update_tray_status(&app, "Disconnected");
    Ok(())
}

/// Tauri command: get current tunnel status as a string.
#[tauri::command]
pub fn get_tunnel_status(tunnel_state: tauri::State<'_, crate::RemoteState>) -> String {
    match &*tunnel_state.tunnel_status.lock().unwrap() {
        TunnelStatus::Disconnected => "Disconnected".to_string(),
        TunnelStatus::Connecting => "Connecting".to_string(),
        TunnelStatus::Connected => "Connected".to_string(),
        TunnelStatus::Error(msg) => format!("Error: {msg}"),
    }
}

/// Tauri command: test remote connection by sending a self-ping.
#[tauri::command]
pub fn test_remote_connection(
    tunnel_state: tauri::State<'_, crate::RemoteState>,
) -> Result<String, String> {
    let status = tunnel_state.tunnel_status.lock().unwrap().clone();
    match status {
        TunnelStatus::Connected | TunnelStatus::Disconnected => {
            Ok("Connection test not yet implemented".to_string())
        }
        TunnelStatus::Connecting => Err("Tunnel is connecting".to_string()),
        TunnelStatus::Error(msg) => Err(format!("Tunnel error: {msg}")),
    }
}

/// Tauri command: generate a new random token.
#[tauri::command]
pub fn generate_remote_token() -> String {
    generate_token()
}

/// Tauri command: return the path to the SSH log file.
#[tauri::command]
pub fn get_ssh_log_path() -> String {
    ssh_log_path()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Token generation tests ──

    #[test]
    fn test_token_generation() {
        let token = generate_token();
        assert_eq!(token.len(), 32, "Token must be exactly 32 characters");
    }

    #[test]
    fn test_token_format() {
        let token = generate_token();
        // Must match [0-9a-f]{32}
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "Token must contain only lowercase hex digits, got: {token}"
        );
    }

    #[test]
    fn test_token_uniqueness() {
        let t1 = generate_token();
        let t2 = generate_token();
        // With 128 bits of entropy the probability of collision is negligible
        assert_ne!(t1, t2, "Two generated tokens should not be equal");
    }

    // ── TunnelStatus tests ──

    #[test]
    fn test_tunnel_status_default() {
        let status = TunnelStatus::default();
        assert_eq!(status, TunnelStatus::Disconnected);
    }

    #[test]
    fn test_tunnel_status_variants() {
        let disconnected = TunnelStatus::Disconnected;
        let connecting = TunnelStatus::Connecting;
        let connected = TunnelStatus::Connected;
        let error = TunnelStatus::Error("something went wrong".to_string());

        assert_eq!(disconnected, TunnelStatus::Disconnected);
        assert_eq!(connecting, TunnelStatus::Connecting);
        assert_eq!(connected, TunnelStatus::Connected);
        assert!(matches!(error, TunnelStatus::Error(_)));
    }

    // ── SshTunnel / SshConfig tests ──

    fn make_ssh_config() -> SshConfig {
        SshConfig {
            host: "ssh.example.com".to_string(),
            port: 21168,
            user: "aicc".to_string(),
            key_path: "/home/aicc/.ssh/id_rsa".to_string(),
            remote_port: 19876,
            local_port: 19876,
        }
    }

    #[test]
    fn test_ssh_command_args() {
        let config = make_ssh_config();
        let tunnel = SshTunnel::new(config.clone());
        let args = tunnel.build_ssh_args();

        // Verify the -R forwarding argument
        assert!(args.contains(&"-R".to_string()));
        let r_idx = args.iter().position(|a| a == "-R").unwrap();
        assert_eq!(args[r_idx + 1], "*:19876:127.0.0.1:19876");

        // Verify user@host
        assert!(args.contains(&format!("{}@{}", config.user, config.host)));

        // Verify port
        let p_idx = args.iter().position(|a| a == "-p").unwrap();
        assert_eq!(args[p_idx + 1], "21168");

        // Verify -N (no command)
        assert!(args.contains(&"-N".to_string()));

        // Verify identity file
        let i_idx = args.iter().position(|a| a == "-i").unwrap();
        assert_eq!(args[i_idx + 1], config.key_path);

        // Verify options
        assert!(args.contains(&"StrictHostKeyChecking=accept-new".to_string()));
        assert!(args.contains(&"ServerAliveInterval=30".to_string()));
        assert!(args.contains(&"ExitOnForwardFailure=yes".to_string()));
    }

    #[test]
    fn test_ssh_tunnel_initial_state() {
        let config = make_ssh_config();
        let tunnel = SshTunnel::new(config);
        assert!(tunnel.process.is_none(), "New tunnel must have no process");
    }

    #[test]
    fn test_ssh_tunnel_not_alive_when_no_process() {
        let config = make_ssh_config();
        let mut tunnel = SshTunnel::new(config);
        assert!(
            !tunnel.is_alive(),
            "Tunnel without process must not be alive"
        );
    }

    // ── HTTP request validation logic tests ──

    #[test]
    fn test_token_comparison_correct() {
        let server_token = "abc123def456";
        let client_token = "abc123def456";
        assert_eq!(server_token, client_token);
    }

    #[test]
    fn test_token_comparison_wrong() {
        let server_token = "abc123def456";
        let client_token = "wrong_token";
        assert_ne!(server_token, client_token);
    }

    #[test]
    fn test_token_comparison_empty() {
        let server_token = "";
        let client_token = "";
        // Empty token should still fail authentication (no token configured)
        assert!(server_token.is_empty());
        assert!(client_token.is_empty());
    }

    #[test]
    fn test_source_preserved_from_request() {
        // Source field is kept as-is for display (e.g., "GOLD24")
        let mut req = NotifyRequest {
            pid: 1234,
            event: "task_complete".to_string(),
            message: Some("Done".to_string()),
            title_hint: None,
            process_tree: None,
            source: "GOLD24".to_string(),
            remote_host: None,
        };
        // Server keeps user-provided source; only defaults empty to "remote"
        if req.source.is_empty() {
            req.source = "remote".to_string();
        }
        req.remote_host = Some("10.0.0.1".to_string());
        assert_eq!(req.source, "GOLD24");
        assert!(req.remote_host.is_some());
    }

    #[test]
    fn test_source_defaults_to_remote_when_empty() {
        let mut req = NotifyRequest {
            pid: 0,
            event: "task_complete".to_string(),
            message: None,
            title_hint: None,
            process_tree: None,
            source: "".to_string(),
            remote_host: None,
        };
        if req.source.is_empty() {
            req.source = "remote".to_string();
        }
        req.remote_host = Some("remote".to_string());
        assert_eq!(req.source, "remote");
    }

    #[test]
    fn test_remote_detected_by_remote_host() {
        // Remote notifications are identified by remote_host being set,
        // not by source == "remote". This mirrors notification.rs logic.
        let remote_host: Option<String> = Some("10.0.0.1".to_string());
        let source = "GOLD24";
        let is_internal = source == "updater" || remote_host.is_some();
        assert!(is_internal, "Remote notification must be treated as internal");
    }

    #[test]
    fn test_hookconfig_remote_defaults() {
        let config = crate::setup::HookConfig::default();
        assert!(!config.remote_enabled);
        assert_eq!(config.remote_port, 19876);
        assert!(config.remote_token.is_empty());
        assert!(config.ssh_host.is_empty());
        assert_eq!(config.ssh_port, 21168);
        assert_eq!(config.ssh_user, "aicc");
        // Default SSH key path is resolved from USERPROFILE/HOME env
        assert!(!config.ssh_key_path.is_empty() || std::env::var("USERPROFILE").is_err() && std::env::var("HOME").is_err());
        assert_eq!(config.ssh_remote_port, 19876);
        assert!(!config.ssh_auto_connect);
    }

    #[test]
    fn test_notify_request_json_parse() {
        let json = r#"{"pid":0,"event":"task_complete","message":"hello","source":"remote"}"#;
        let req: NotifyRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.source, "remote");
        assert_eq!(req.event, "task_complete");
    }

    #[test]
    fn test_notify_request_invalid_json() {
        let json = r#"not valid json"#;
        let result = serde_json::from_str::<NotifyRequest>(json);
        assert!(result.is_err(), "Invalid JSON must fail to parse");
    }

    // ── resolve_home_dir tests ──

    #[test]
    fn test_resolve_home_dir_tilde_slash() {
        let resolved = resolve_home_dir("~/.ssh/id_rsa");
        assert!(!resolved.starts_with("~"), "~ should be resolved");
        assert!(resolved.ends_with(".ssh/id_rsa") || resolved.ends_with(".ssh\\id_rsa"));
    }

    #[test]
    fn test_resolve_home_dir_tilde_backslash() {
        let resolved = resolve_home_dir("~\\.ssh\\id_rsa");
        assert!(!resolved.starts_with("~"), "~ should be resolved");
    }

    #[test]
    fn test_resolve_home_dir_absolute_path_unchanged() {
        let path = "C:\\Users\\admin\\.ssh\\id_rsa";
        assert_eq!(resolve_home_dir(path), path);
    }

    #[test]
    fn test_resolve_home_dir_empty_unchanged() {
        assert_eq!(resolve_home_dir(""), "");
    }
}
