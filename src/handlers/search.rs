use serde_json::Value;

use teloxide::{
    payloads::{EditMessageCaptionSetters, EditMessageMediaSetters, EditMessageTextSetters},
    prelude::Requester,
    requests::Request,
    types::{ChatId, InputFile, InputMedia, InputMediaPhoto},
};

use crate::{
    error::HandlerResult,
    keyboards::{carousel_keyboard, results_keyboard},
    models::Beat,
    services::epsilone::{fetch_recommendations, fetch_trending, rate_limit_message},
    state::{AppState, ConversationState, SearchRequest},
    ui::edit_status_message,
};

pub async fn execute_request(
    bot: &teloxide::Bot,
    chat_id: ChatId,
    state: &AppState,
    request: SearchRequest,
    request_id: u64,
    status_message_id: i32,
) -> HandlerResult {
    let api_result = match &request {
        SearchRequest::Ai { query } => fetch_recommendations(state, query).await,
        SearchRequest::Trending => fetch_trending(state).await,
    };

    match api_result {
        Ok(response) => {
            if !state.is_current_request(chat_id, request_id) {
                return Ok(());
            }

            let exact_matches = !response.results.is_empty();

            let beats = if exact_matches {
                response.results
            } else {
                response.alternatives
            };

            state.cache_beat_previews(&beats);
            state.set_results(chat_id, beats);

            let Some((first_beat, index, total)) = state.current_result(chat_id) else {
                if state.finish_request(
                    chat_id,
                    request_id,
                    ConversationState::ShowingResults {
                        message_id: status_message_id,
                    },
                ) {
                    state.set_result_message_ids(chat_id, vec![status_message_id]);

                    let empty_text = match request {
                        SearchRequest::Trending => {
                            "فعلاً بیت ترندی برای نمایش پیدا نشد. کمی بعد دوباره امتحان کن."
                        }
                        SearchRequest::Ai { .. } => {
                            "نتیجه‌ای برای نمایش پیدا نشد. می‌تونی جست‌وجوی دیگری انجام بدی."
                        }
                    };

                    edit_status_message(
                        bot,
                        chat_id,
                        status_message_id,
                        empty_text,
                        results_keyboard(),
                    )
                    .await?;
                }

                return Ok(());
            };

            if !state.is_current_request(chat_id, request_id) {
                return Ok(());
            }

            // Reuse the loading/status message as the first beat card.
            let changed = state.finish_request(
                chat_id,
                request_id,
                ConversationState::ShowingResults {
                    message_id: status_message_id,
                },
            );

            if !changed {
                return Ok(());
            }

            state.set_result_message_ids(chat_id, vec![status_message_id]);

            render_carousel_beat(
                bot,
                chat_id,
                state,
                status_message_id,
                &first_beat,
                index,
                total,
            )
            .await?;
        }

        Err(error) => {
            log::error!("Epsilone API error: {}", error);

            if state.finish_request(
                chat_id,
                request_id,
                ConversationState::Error {
                    request,
                    message_id: status_message_id,
                },
            ) {
                let message = rate_limit_message(&error).unwrap_or_else(|| {
                    "⚠️ ارتباط با سرویس Epsilone با مشکل مواجه شد.\n\nمی‌تونی دوباره تلاش کنی."
                        .to_string()
                });

                edit_status_message(
                    bot,
                    chat_id,
                    status_message_id,
                    &message,
                    crate::keyboards::retry_keyboard(),
                )
                .await?;
            }
        }
    }

    Ok(())
}

// ----------------------------------------
// BEAT CAROUSEL NAVIGATION
// ----------------------------------------

pub async fn show_adjacent_result(
    bot: &teloxide::Bot,
    chat_id: ChatId,
    state: &AppState,
    message_id: i32,
    direction: i32,
) -> HandlerResult {
    if !state.is_current_result_message(chat_id, message_id) {
        return Ok(());
    }

    let Some((beat, index, total)) = state.move_result(chat_id, direction) else {
        return Ok(());
    };

    // Remove the previous voice preview when changing the selected beat.
    if let Some(preview_message_id) = state.take_preview_message_id(chat_id) {
        let _ = bot.delete_message(chat_id, preview_message_id).send().await;
    }

    render_carousel_beat(bot, chat_id, state, message_id, &beat, index, total).await
}

// ----------------------------------------
// RENDER / EDIT THE SINGLE CAROUSEL MESSAGE
// ----------------------------------------

async fn render_carousel_beat(
    bot: &teloxide::Bot,
    chat_id: ChatId,
    state: &AppState,
    message_id: i32,
    beat: &Beat,
    index: usize,
    total: usize,
) -> HandlerResult {
    let beat_url = format!("{}/beat/{}", state.site_url, beat.id);

    let keyboard = carousel_keyboard(&beat_url, state.has_cached_preview(&beat.id), total > 1);

    let caption = format!(
        "{}\n\n🎵 Beat {} of {}",
        format_beat(beat),
        index + 1,
        total
    );

    let current_message_is_photo = state.result_message_is_photo(chat_id);
    let cover_url = safe_cover_url(beat.cover_url.as_deref());

    let mut cover_update_failed = cover_url.is_none();

    if let Some(cover_url) = cover_url {
        let media = InputMedia::Photo(
            InputMediaPhoto::new(InputFile::url(cover_url)).caption(caption.clone()),
        );

        match bot
            .edit_message_media(chat_id, message_id, media)
            .reply_markup(keyboard.clone())
            .send()
            .await
        {
            Ok(_) => {
                state.set_result_message_is_photo(chat_id, true);
                return Ok(());
            }

            Err(error) => {
                cover_update_failed = true;

                log::warn!(
                    "Could not update cover for beat {}; falling back to text/caption: {}",
                    beat.id,
                    error
                );
            }
        }
    }

    let display_text = if cover_update_failed {
        let cover_note = if current_message_is_photo {
            "🖼 کاور جدید در دسترس نیست؛ تصویر قبلی حفظ شده است."
        } else {
            "🖼 کاور این بیت در دسترس نیست."
        };

        format!("{}\n\n{}", caption, cover_note)
    } else {
        caption
    };

    // A photo message cannot be converted back to a text-only message.
    // If the next beat has no usable cover, update its caption instead.
    if current_message_is_photo {
        match bot
            .edit_message_caption(chat_id, message_id)
            .caption(display_text.clone())
            .reply_markup(keyboard.clone())
            .send()
            .await
        {
            Ok(_) => return Ok(()),

            Err(error) => {
                log::warn!("Could not edit carousel photo caption: {}", error);
            }
        }
    }

    bot.edit_message_text(chat_id, message_id, display_text)
        .reply_markup(keyboard)
        .send()
        .await?;

    state.set_result_message_is_photo(chat_id, false);

    Ok(())
}

fn safe_cover_url(value: Option<&str>) -> Option<reqwest::Url> {
    let url = reqwest::Url::parse(value?).ok()?;

    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }

    Some(url)
}

// ----------------------------------------
// FORMAT BEAT
// ----------------------------------------

fn format_beat(beat: &Beat) -> String {
    let title = beat.title.as_deref().unwrap_or("Untitled Beat");

    let producer = beat
        .producer_display_name
        .as_deref()
        .or(beat.producer_username.as_deref())
        .unwrap_or("Unknown Producer");

    let mut lines = vec![
        format!("🎧 {}", title),
        format!("👤 Producer: {}", producer),
    ];

    if let Some(genre) = beat.genre.as_deref() {
        lines.push(format!("🎼 Genre: {}", genre));
    }

    if let Some(bpm) = value_to_string(&beat.bpm) {
        lines.push(format!("🥁 BPM: {}", bpm));
    }

    if let Some(key) = beat.key.as_deref() {
        lines.push(format!("🎹 Key: {}", key));
    }

    if let Some(price) = value_to_string(&beat.starting_price) {
        let currency = beat.currency.as_deref().unwrap_or("");

        if currency.is_empty() {
            lines.push(format!("💰 Starting Price: {}", price));
        } else {
            lines.push(format!("💰 Starting Price: {} {}", price, currency));
        }
    }

    lines.join("\n")
}

fn value_to_string(value: &Option<Value>) -> Option<String> {
    match value.as_ref()? {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}
