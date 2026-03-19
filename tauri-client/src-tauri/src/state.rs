use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::net::central::CentralClient;
use crate::net::community::CommunityClient;

pub struct AppState {
    pub central: Option<CentralClient>,
    pub communities: HashMap<String, CommunityClient>,
    pub username: Option<String>,
    pub token: Option<String>,
    pub credentials: Option<(String, String)>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            central: None,
            communities: HashMap::new(),
            username: None,
            token: None,
            credentials: None,
        }
    }
}

pub type SharedState = Arc<Mutex<AppState>>;
