use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NotifyPayload {
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub fields: Vec<(String, String)>,
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub trace_id: Option<String>,
}
