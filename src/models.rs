use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct ApiResponse {
    #[serde(default)]
    pub success: bool,

    #[serde(default)]
    pub results: Vec<Beat>,

    #[serde(default)]
    pub alternatives: Vec<Beat>,

    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Beat {
    pub id: String,
    pub title: Option<String>,
    pub producer_username: Option<String>,
    pub producer_display_name: Option<String>,
    pub cover_url: Option<String>,
    pub preview_url: Option<String>,
    pub bpm: Option<Value>,
    pub key: Option<String>,
    pub genre: Option<String>,
    pub starting_price: Option<Value>,
    pub currency: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PreviewInfo {
    pub url: String,
    pub title: String,
}
