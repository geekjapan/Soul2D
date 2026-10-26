//! The MCP server the editor starts from its settings (D `ngMcpLoadSettings` / `ngMcpApplySettings`, keys
//! `MCP.Enabled`, `MCP.Host`, `MCP.Port`, `MCP.authEnabled`).
//!
//! The listener is non-blocking: the editor calls [`McpServer::serve_pending`] once per frame and answers
//! the connections that are waiting, one request each. Off by default. D's OAuth-style flow, its approval
//! popup and its SSE stream are not ported; authentication is one static bearer token (the environment's
//! `SOUL2D_MCP_BEARER`, else a random one made when `MCP.authEnabled` is on).

use crate::mcp_http::{handle_connection_with, is_loopback_host, random_token, RpcReply};
use crate::settings::Settings;
use serde_json::Value;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

pub const ENV_BEARER: &str = "SOUL2D_MCP_BEARER";
pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 8088;
/// Connections answered per frame, and the time one stalled peer may hold the editor.
pub const MAX_CONNECTIONS_PER_FRAME: usize = 4;
pub const CONNECTION_BUDGET: Duration = Duration::from_millis(400);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpSettings {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub auth: bool,
}

impl Default for McpSettings {
    fn default() -> Self {
        Self { enabled: false, host: DEFAULT_HOST.to_owned(), port: DEFAULT_PORT, auth: false }
    }
}

impl McpSettings {
    /// D defaults: disabled, `127.0.0.1`, port 8088, no authentication. A stored port outside 0..=65535, or
    /// an empty host, falls back to the default so a bad file never starts a listener on a surprise address.
    pub fn load(store: &Settings) -> Self {
        let defaults = Self::default();
        let host = store.get_or::<String>("MCP.Host", defaults.host.clone()).unwrap_or(defaults.host.clone());
        let port = store.get_or::<i64>("MCP.Port", i64::from(DEFAULT_PORT)).unwrap_or(i64::from(DEFAULT_PORT));
        Self {
            enabled: store.get_or::<bool>("MCP.Enabled", false).unwrap_or(false),
            host: if host.trim().is_empty() { defaults.host } else { host.trim().to_owned() },
            port: u16::try_from(port).unwrap_or(DEFAULT_PORT),
            auth: store.get_or::<bool>("MCP.authEnabled", false).unwrap_or(false),
        }
    }

    pub fn store(&self, store: &mut Settings) -> Result<(), String> {
        store.set("MCP.Enabled", self.enabled)?;
        store.set("MCP.Host", self.host.clone())?;
        store.set("MCP.Port", i64::from(self.port))?;
        store.set("MCP.authEnabled", self.auth)
    }
}

/// How requests are authenticated, decided from the settings and the environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Auth {
    /// Loopback only, no token.
    Open,
    /// `SOUL2D_MCP_BEARER` (or the caller's) token; never shown.
    Configured(String),
    /// Made for this run because `MCP.authEnabled` is on and nothing was configured; shown to the user.
    Generated(String),
}

impl Auth {
    pub fn token(&self) -> Option<&str> {
        match self {
            Self::Open => None,
            Self::Configured(token) | Self::Generated(token) => Some(token),
        }
    }
}

/// Applies the `soul2d-core --mcp-port` rules: a non-loopback host needs a configured bearer. With
/// `MCP.authEnabled` and no configured bearer a random token is generated (only for loopback hosts).
pub fn decide_auth(settings: &McpSettings, configured: Option<String>) -> Result<Auth, String> {
    let configured = configured.filter(|token| !token.is_empty());
    if let Some(token) = configured {
        return Ok(Auth::Configured(token));
    }
    if !is_loopback_host(&settings.host) {
        return Err(format!("refusing to serve {} without {ENV_BEARER}; use a loopback host or set the token", settings.host));
    }
    if settings.auth {
        return Ok(Auth::Generated(random_token()?));
    }
    Ok(Auth::Open)
}

pub struct McpServer {
    listener: TcpListener,
    auth: Auth,
    url: String,
}

impl McpServer {
    /// Binds the listener (non-blocking). `configured` is the environment's bearer.
    pub fn start(settings: &McpSettings, configured: Option<String>) -> Result<Self, String> {
        let auth = decide_auth(settings, configured)?;
        let listener = TcpListener::bind((settings.host.as_str(), settings.port))
            .map_err(|error| format!("cannot listen on {}:{}: {error}", settings.host, settings.port))?;
        listener.set_nonblocking(true).map_err(|error| format!("cannot configure the MCP listener: {error}"))?;
        let addr = listener.local_addr().map_err(|error| format!("cannot read the MCP address: {error}"))?;
        Ok(Self { listener, auth, url: url_of(addr) })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn auth(&self) -> &Auth {
        &self.auth
    }

    /// Answers the connections that are waiting now (at most `max`); never blocks on an idle listener.
    /// Returns how many were answered.
    pub fn serve_pending(&self, max: usize, budget: Duration, handler: &mut dyn FnMut(&Value) -> RpcReply) -> usize {
        let mut served = 0;
        while served < max {
            let Ok((stream, _)) = self.listener.accept() else { break };
            // Some platforms hand out accepted sockets in the listener's non-blocking mode.
            if configure(&stream).is_err() {
                continue;
            }
            if let Err(error) = handle_connection_with(stream, self.auth.token(), budget, handler) {
                eprintln!("mcp-http: response failed: {error}");
            }
            served += 1;
        }
        served
    }
}

fn configure(stream: &TcpStream) -> std::io::Result<()> {
    stream.set_nonblocking(false)
}

fn url_of(addr: SocketAddr) -> String {
    match addr {
        SocketAddr::V4(v4) => format!("http://{}:{}/mcp", v4.ip(), v4.port()),
        SocketAddr::V6(v6) => format!("http://[{}]:{}/mcp", v6.ip(), v6.port()),
    }
}
