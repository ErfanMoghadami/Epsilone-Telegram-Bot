use serde_json::Value;

use std::collections::HashSet;
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
        SearchRequest::Ai { query } | SearchRequest::Personalized { query } => {
            fetch_recommendations(state, query).await
        }

        SearchRequest::Trending => fetch_trending(state).await,
    };

    match api_result {
        Ok(response) => {
            if !state.is_current_request(chat_id, request_id) {
                return Ok(());
            }

            let is_personalized = matches!(&request, SearchRequest::Personalized { .. });

            let mut exact_results = response.results;
            let mut alternatives = response.alternatives;

            if is_personalized {
                let favorite_ids: HashSet<String> = state
                    .favorites(chat_id)
                    .into_iter()
                    .map(|beat| beat.id)
                    .collect();

                exact_results = filter_personalized_beats(exact_results, &favorite_ids);

                alternatives = filter_personalized_beats(alternatives, &favorite_ids);
            }

            let beats = if !exact_results.is_empty() {
                exact_results
            } else {
                alternatives
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

                    let empty_text = match &request {
                        SearchRequest::Trending => {
                            "فعلاً بیت ترندی برای نمایش پیدا نشد. کمی بعد دوباره امتحان کن."
                        }

                        SearchRequest::Ai { .. } => {
                            "نتیجه‌ای برای نمایش پیدا نشد. می‌تونی جست‌وجوی دیگری انجام بدی."
                        }

                        SearchRequest::Personalized { .. } => {
                            "✨ فعلاً بیت جدید و مناسبی بر اساس Favoriteهات پیدا نشد.\n\nچند بیت دیگر Favorite کن و دوباره امتحان کن."
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

fn filter_personalized_beats(beats: Vec<Beat>, favorite_ids: &HashSet<String>) -> Vec<Beat> {
    let mut seen = HashSet::new();

    beats
        .into_iter()
        .filter(|beat| !favorite_ids.contains(&beat.id) && seen.insert(beat.id.clone()))
        .collect()
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
// SHOW BEATS IN THE EXISTING CAROUSEL
// ----------------------------------------

pub async fn show_beats_in_carousel(
    bot: &teloxide::Bot,
    chat_id: ChatId,
    state: &AppState,
    message_id: i32,
    beats: Vec<Beat>,
    favorites_view: bool,
) -> HandlerResult {
    if beats.is_empty() {
        return Ok(());
    }

    state.cache_beat_previews(&beats);

    if favorites_view {
        state.set_favorite_results(chat_id, beats);
    } else {
        state.set_results(chat_id, beats);
    }

    state.set_state(chat_id, ConversationState::ShowingResults { message_id });

    state.set_result_message_ids(chat_id, vec![message_id]);

    let Some((beat, index, total)) = state.current_result(chat_id) else {
        return Ok(());
    };

    render_carousel_beat(bot, chat_id, state, message_id, &beat, index, total).await
}

// ----------------------------------------
// RENDER / EDIT THE SINGLE CAROUSEL MESSAGE
// ----------------------------------------

pub async fn render_carousel_beat(
    bot: &teloxide::Bot,
    chat_id: ChatId,
    state: &AppState,
    message_id: i32,
    beat: &Beat,
    index: usize,
    total: usize,
) -> HandlerResult {
    let beat_url = format!("{}/beat/{}", state.site_url, beat.id);

    let keyboard = carousel_keyboard(
        &beat_url,
        state.has_cached_preview(&beat.id),
        total > 1,
        state.is_favorite(chat_id, &beat.id),
    );

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

#[cfg(test)]
mod personalized_tests {
    use super::*;

    fn beat(id: &str) -> Beat {
        Beat {
            id: id.to_string(),
            title: Some("Test Beat".to_string()),
            producer_username: None,
            producer_display_name: None,
            cover_url: None,
            preview_url: None,
            bpm: None,
            key: None,
            genre: None,
            starting_price: None,
            currency: None,
        }
    }

    #[test]
    fn removes_favorite_beats() {
        let favorites = HashSet::from(["beat-1".to_string()]);

        let result = filter_personalized_beats(vec![beat("beat-1"), beat("beat-2")], &favorites);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "beat-2");
    }

    #[test]
    fn removes_duplicate_beat_ids() {
        let result = filter_personalized_beats(
            vec![beat("beat-1"), beat("beat-1"), beat("beat-2")],
            &HashSet::new(),
        );

        assert_eq!(result.len(), 2);
    }

    #[test]
    fn keeps_all_new_beats() {
        let result =
            filter_personalized_beats(vec![beat("beat-1"), beat("beat-2")], &HashSet::new());

        assert_eq!(result.len(), 2);
    }
}
