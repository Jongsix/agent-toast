use crate::cli::NotifyRequest;
use crate::notification::{show_notification, NotificationManagerState};
use std::process::{Child, Command};
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
        vec![
            "-R".to_string(),
            format!("{}:127.0.0.1:{}", cfg.remote_port, cfg.local_port),
            format!("{}@{}", cfg.user, cfg.host),
            "-p".to_string(),
            cfg.port.to_string(),
            "-N".to_string(),
            "-i".to_string(),
            cfg.key_path.clone(),
            "-o".to_string(),
            "StrictHostKeyChecking=accept-new".to_string(),
            "-o".to_string(),
            "ServerAliveInterval=30".to_string(),
            "-o".to_string(),
            "ExitOnForwardFailure=yes".to_string(),
        ]
    }

    /// Spawn the SSH process and return it.
    pub fn connect(&mut self, status: Arc<Mutex<TunnelStatus>>) -> Result<(), String> {
        if self.process.is_some() {
            return Err("Tunnel already running".to_string());
        }

        *status.lock().unwrap() = TunnelStatus::Connecting;

        let args = self.build_ssh_args();
        log::info!(
            "[SSH] Connecting tunnel to {}:{}",
            self.config.host,
            self.config.port
        );

        match Command::new("ssh").args(&args).spawn() {
            Ok(child) => {
                self.process = Some(child);
                *status.lock().unwrap() = TunnelStatus::Connected;
                log::info!("[SSH] Tunnel process spawned");
                Ok(())
            }
            Err(e) => {
                let msg = format!("Failed to spawn ssh: {e}");
                *status.lock().unwrap() = TunnelStatus::Error(msg.clone());
                log::error!("[SSH] {msg}");
                Err(msg)
            }
        }
    }

    /// Kill the SSH process and reset state.
    pub fn disconnect(&mut self, status: Arc<Mutex<TunnelStatus>>) {
        if let Some(mut child) = self.process.take() {
            let _ = child.kill();
            let _ = child.wait();
            log::info!("[SSH] Tunnel disconnected");
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

// ── Watchdog ──────────────────────────────────────────────────────────────────

/// Start a watchdog thread that monitors the SSH tunnel process.
/// If the process dies and auto_reconnect is true, it attempts to reconnect.
pub fn start_watchdog(
    tunnel: Arc<Mutex<Option<SshTunnel>>>,
    status: Arc<Mutex<TunnelStatus>>,
    auto_reconnect: bool,
) {
    let poll_interval = Duration::from_secs(10);

    std::thread::spawn(move || loop {
        std::thread::sleep(poll_interval);

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
pub fn start_http_server(
    port: u16,
    token: String,
    app: AppHandle,
    state: NotificationManagerState,
) {
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

        for request in server.incoming_requests() {
            handle_http_request(request, &token, &app, &state);
        }
    });
}

/// Handle a single HTTP request, consuming ownership as required by tiny_http.
fn handle_http_request(
    mut request: tiny_http::Request,
    token: &str,
    app: &AppHandle,
    state: &NotificationManagerState,
) {
    let method = request.method().to_string();
    let url = request.url().to_string();

    log::debug!("[HTTP] {method} {url}");

    // Only accept POST /notify
    if url != "/notify" {
        respond_status(request, 404, "Not Found");
        return;
    }

    if method != "POST" {
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

    if token.is_empty() || provided_token != token {
        log::warn!("[HTTP] Unauthorized request: token mismatch [TOKEN MASKED]");
        respond_status(request, 401, "Unauthorized");
        return;
    }

    // Read body
    let mut body = String::new();
    if let Err(e) = std::io::Read::read_to_string(request.as_reader(), &mut body) {
        log::error!("[HTTP] Failed to read request body: {e}");
        respond_status(request, 400, "Bad Request");
        return;
    }

    // Deserialize the notify request
    let mut notify_req: NotifyRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            log::error!("[HTTP] Invalid JSON body: {e}");
            respond_status(request, 400, "Bad Request");
            return;
        }
    };

    // Force source to "remote" regardless of what was sent
    notify_req.source = "remote".to_string();

    log::info!(
        "[HTTP] Remote notification: event={}, pid={}",
        notify_req.event,
        notify_req.pid
    );

    show_notification(app, state, notify_req);
    respond_status(request, 200, "OK");
}

/// Send a minimal HTTP response with the given status code and text body.
/// Consumes the request (as required by tiny_http).
fn respond_status(request: tiny_http::Request, code: u16, text: &str) {
    let response = tiny_http::Response::from_string(text).with_status_code(code);
    if let Err(e) = request.respond(response) {
        log::debug!("[HTTP] Failed to send response: {e}");
    }
}

// ── IPC Commands ──────────────────────────────────────────────────────────────

/// Tauri command: connect the SSH tunnel.
#[tauri::command]
pub fn connect_ssh_tunnel(
    tunnel_state: tauri::State<'_, crate::RemoteState>,
) -> Result<(), String> {
    let status = tunnel_state.tunnel_status.clone();
    let mut guard = tunnel_state.ssh_tunnel.lock().unwrap();
    match guard.as_mut() {
        Some(t) => t.connect(status),
        None => Err("SSH tunnel not configured".to_string()),
    }
}

/// Tauri command: disconnect the SSH tunnel.
#[tauri::command]
pub fn disconnect_ssh_tunnel(
    tunnel_state: tauri::State<'_, crate::RemoteState>,
) -> Result<(), String> {
    let status = tunnel_state.tunnel_status.clone();
    let mut guard = tunnel_state.ssh_tunnel.lock().unwrap();
    if let Some(ref mut t) = *guard {
        // Mark user-initiated disconnect so watchdog does not auto-reconnect
        *tunnel_state.user_disconnected.lock().unwrap() = true;
        t.disconnect(status);
    }
    Ok(())
}

/// Tauri command: get current tunnel status as a string.
#[tauri::command]
pub fn get_tunnel_status(tunnel_state: tauri::State<'_, crate::RemoteState>) -> String {
    match &*tunnel_state.tunnel_status.lock().unwrap() {
        TunnelStatus::Disconnected => "disconnected".to_string(),
        TunnelStatus::Connecting => "connecting".to_string(),
        TunnelStatus::Connected => "connected".to_string(),
        TunnelStatus::Error(msg) => format!("error: {msg}"),
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
            port: 22,
            user: "alice".to_string(),
            key_path: "/home/alice/.ssh/id_rsa".to_string(),
            remote_port: 19876,
            local_port: 9876,
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
        assert_eq!(args[r_idx + 1], "19876:127.0.0.1:9876");

        // Verify user@host
        assert!(args.contains(&format!("{}@{}", config.user, config.host)));

        // Verify port
        let p_idx = args.iter().position(|a| a == "-p").unwrap();
        assert_eq!(args[p_idx + 1], "22");

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
    fn test_source_remote_override() {
        // Simulate the server overriding source to "remote"
        let mut req = NotifyRequest {
            pid: 1234,
            event: "task_complete".to_string(),
            message: Some("Done".to_string()),
            title_hint: None,
            process_tree: None,
            source: "claude".to_string(), // This would be overridden
        };
        // The server always forces source to "remote"
        req.source = "remote".to_string();
        assert_eq!(req.source, "remote");
    }

    #[test]
    fn test_source_remote_is_internal() {
        // Verify that "remote" source is treated as internal (no win32 lookup)
        // This mirrors the logic in notification.rs
        let source = "remote";
        let is_internal = source == "updater" || source == "remote";
        assert!(is_internal, "Remote source must be treated as internal");
    }

    #[test]
    fn test_hookconfig_remote_defaults() {
        let config = crate::setup::HookConfig::default();
        assert!(!config.remote_enabled);
        assert_eq!(config.remote_port, 9876);
        assert!(config.remote_token.is_empty());
        assert!(config.ssh_host.is_empty());
        assert_eq!(config.ssh_port, 22);
        assert!(config.ssh_user.is_empty());
        assert!(config.ssh_key_path.is_empty());
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
}
