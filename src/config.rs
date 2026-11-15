use std::env;

use reqwest::{Client, Proxy};

use crate::error::AppError;

#[derive(Clone, Debug)]
pub struct Config {
    pub site_url: String,
    pub http_proxy: Option<String>,
}

impl Config {
    pub fn from_env() -> Self {
        let site_url = env::var("EPSILONE_SITE_URL")
            .unwrap_or_else(|_| "https://www.epsilone.ir".to_string())
            .trim_end_matches('/')
            .to_string();

        let http_proxy = env::var("EPSILONE_HTTP_PROXY")
            .ok()
            .filter(|value| !value.trim().is_empty());

        Self {
            site_url,
            http_proxy,
        }
    }

    pub fn build_http_client(&self) -> Result<Client, AppError> {
        let mut builder = Client::builder().timeout(std::time::Duration::from_secs(90));

        if let Some(proxy_url) = &self.http_proxy {
            builder = builder.proxy(Proxy::all(proxy_url)?);
        }

        Ok(builder.build()?)
    }
}
