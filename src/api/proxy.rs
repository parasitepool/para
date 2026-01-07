use {
    super::*,
    axum::extract::State,
    std::sync::atomic::{AtomicBool, Ordering},
};

pub struct ProxyStatus {
    upstream_url: String,
    upstream_username: String,
    downstream_address: String,
    downstream_port: u16,
    connected: AtomicBool,
}

impl ProxyStatus {
    pub fn new(
        upstream_url: String,
        upstream_username: String,
        downstream_address: String,
        downstream_port: u16,
    ) -> Self {
        Self {
            upstream_url,
            upstream_username,
            downstream_address,
            downstream_port,
            connected: AtomicBool::new(false),
        }
    }

    pub fn set_connected(&self, connected: bool) {
        self.connected.store(connected, Ordering::SeqCst);
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub upstream: UpstreamStatus,
    pub downstream: DownstreamStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamStatus {
    pub url: String,
    pub connected: bool,
    pub username: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownstreamStatus {
    pub address: String,
    pub port: u16,
}

pub fn router(status: Arc<ProxyStatus>) -> Router {
    Router::new()
        .route("/api/status", get(get_status))
        .with_state(status)
}

async fn get_status(State(status): State<Arc<ProxyStatus>>) -> Json<StatusResponse> {
    Json(StatusResponse {
        upstream: UpstreamStatus {
            url: status.upstream_url.clone(),
            connected: status.is_connected(),
            username: status.upstream_username.clone(),
        },
        downstream: DownstreamStatus {
            address: status.downstream_address.clone(),
            port: status.downstream_port,
        },
    })
}
