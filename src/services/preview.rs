use std::io;

use reqwest::Url;
use teloxide::{
    payloads::SendVoiceSetters,
    prelude::Requester,
    requests::Request,
    types::{ChatId, InputFile},
};

use crate::{error::AppError, state::AppState};

const MAX_PREVIEW_SIZE: u64 = 10 * 1024 * 1024;

/// Sends the selected beat preview and returns its Telegram message ID.
pub async fn send_voice_preview(
    bot: &teloxide::Bot,
    chat_id: ChatId,
    state: &AppState,
    beat_id: &str,
) -> Result<i32, AppError> {
    let preview = state.get_preview(beat_id).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "Preview URL is not available for this beat",
        )
    })?;

    // First try Telegram's cached file_id.
    if let Some(file_id) = state.get_telegram_file_id(beat_id, &preview.url) {
        match bot
            .send_voice(chat_id, InputFile::file_id(file_id))
            .caption(format!("🎧 {} — Beat Preview", preview.title))
            .send()
            .await
        {
            Ok(message) => return Ok(message.id),

            Err(error) => {
                log::warn!(
                    "Cached Telegram file_id failed for beat {}: {}",
                    beat_id,
                    error
                );
            }
        }
    }

    // Cache miss (or an invalid cached file_id): download from R2.
    let url = Url::parse(&preview.url)?;

    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(Box::new(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid preview URL",
        )));
    }

    let response = state.http.get(url).send().await?.error_for_status()?;

    if response
        .content_length()
        .is_some_and(|size| size > MAX_PREVIEW_SIZE)
    {
        return Err(Box::new(io::Error::new(
            io::ErrorKind::InvalidData,
            "Preview file is too large",
        )));
    }

    let audio = response.bytes().await?;

    if audio.is_empty() || audio.len() as u64 > MAX_PREVIEW_SIZE {
        return Err(Box::new(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid preview file size",
        )));
    }

    // Upload to Telegram and cache the returned file_id.
    let sent = bot
        .send_voice(
            chat_id,
            InputFile::memory(audio.to_vec()).file_name("epsilone-preview.mp3"),
        )
        .caption(format!("🎧 {} — Beat Preview", preview.title))
        .send()
        .await?;

    if let Some(voice) = sent.voice() {
        state.set_telegram_file_id(beat_id, &preview.url, voice.file_id.clone());
    }

    Ok(sent.id)
}
