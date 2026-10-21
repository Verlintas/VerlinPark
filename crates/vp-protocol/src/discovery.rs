use serde::{Deserialize, Serialize};

/// Contents of `~/Library/Application Support/VerlinPark/run/daemon.json`.
///
/// The file is written with mode 0600. The token is required for both the
/// WebSocket stream and the HTTP JSON-RPC endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Discovery {
    pub pid: u32,
    pub port: u16,
    pub token: String,
    pub protocol: u32,
    pub version: String,
    pub started_at: String,
}

impl Discovery {
    pub fn websocket_url(&self) -> String {
        format!("ws://127.0.0.1:{}/ws", self.port)
    }

    pub fn rpc_url(&self) -> String {
        format!("http://127.0.0.1:{}/rpc", self.port)
    }
}
