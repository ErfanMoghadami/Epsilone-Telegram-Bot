use std::{error::Error, fmt, io};

use crate::{error::AppError, models::ApiResponse, state::AppState};

const TRENDING_FETCH_LIMIT: usize = 48;

#[derive(Debug)]
pub struct ApiHttpError {
    pub status: u16,
    pub message: String,
    pub retry_after: Option<u64>,
}

impl fmt::Display for ApiHttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.status == 429 {
            if let Some(seconds) = self.retry_after {
                return write!(f, "Rate limited. Retry after {} seconds.", seconds);
            }

            return write!(f, "Rate limited by Epsilone API. Please retry shortly.");
        }

        write!(f, "{} (HTTP {})", self.message, self.status)
    }
}

impl Error for ApiHttpError {}

pub async fn fetch_trending(state: &AppState) -> Result<ApiResponse, AppError> {
    let endpoint = format!(
        "{}/api/trending?mode=trending&limit={}",
        state.site_url, TRENDING_FETCH_LIMIT,
    );

    let response = state.http.get(endpoint).send().await?;

    parse_api_response(response).await
}

pub async fn fetch_recommendations(state: &AppState, query: &str) -> Result<ApiResponse, AppError> {
    let endpoint = format!("{}/api/recommend", state.site_url);

    let response = state
        .http
        .post(endpoint)
        .json(&serde_json::json!({
            "query": query
        }))
        .send()
        .await?;

    parse_api_response(response).await
}

async fn parse_api_response(response: reqwest::Response) -> Result<ApiResponse, AppError> {
    let status = response.status();

    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());

    let payload = response.json::<ApiResponse>().await?;

    if !status.is_success() || !payload.success {
        let message = payload
            .error
            .unwrap_or_else(|| format!("Epsilone API returned HTTP {}", status));

        if status.as_u16() == 429 {
            return Err(Box::new(ApiHttpError {
                status: 429,
                message,
                retry_after,
            }));
        }

        return Err(Box::new(io::Error::new(io::ErrorKind::Other, message)));
    }

    Ok(payload)
}

pub fn rate_limit_message(error: &AppError) -> Option<String> {
    let api_error = error.downcast_ref::<ApiHttpError>()?;

    if api_error.status != 429 {
        return None;
    }

    Some(match api_error.retry_after {
        Some(seconds) => format!(
            "⏳ تعداد درخواست‌ها زیاد شده. لطفاً {} ثانیه دیگه دوباره امتحان کن.",
            seconds
        ),
        None => "⏳ تعداد درخواست‌ها زیاد شده. لطفاً کمی بعد دوباره امتحان کن.".to_string(),
    })
}
