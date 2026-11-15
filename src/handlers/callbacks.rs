use teloxide::{prelude::Requester, requests::Request, types::CallbackQuery};

use crate::{
    error::HandlerResult,
    keyboards::{loading_keyboard, query_keyboard},
    services::preview::send_voice_preview,
    state::{AppState, ConversationState, SearchRequest},
    ui::{edit_status_message, show_welcome_from_callback},
};

use super::search::{execute_request, show_adjacent_result};

pub async fn callback_handler(
    bot: teloxide::Bot,
    query: CallbackQuery,
    state: AppState,
) -> HandlerResult {
    let action = query.data.clone().unwrap_or_default();

    let Some(message) = query.message.clone() else {
        bot.answer_callback_query(query.id).send().await?;
        return Ok(());
    };

    let chat_id = message.chat.id;
    let message_id = message.id;

    // Answer immediately to stop Telegram's callback spinner.
    bot.answer_callback_query(query.id).send().await?;

    match action.as_str() {
        // ------------------------------------
        // BEAT CAROUSEL
        // ------------------------------------
        "carousel:prev" => {
            show_adjacent_result(&bot, chat_id, &state, message_id, -1).await?;
        }

        "carousel:next" => {
            show_adjacent_result(&bot, chat_id, &state, message_id, 1).await?;
        }

        "carousel:play" => {
            play_current_beat(&bot, chat_id, &state, message_id).await?;
        }

        // ------------------------------------
        // AI SEARCH
        // ------------------------------------
        "ai_search" | "search_again" => {
            remove_preview_message(&bot, chat_id, &state).await;

            if !state.enter_waiting_for_query(chat_id, message_id) {
                bot.send_message(chat_id, "⏳ جست‌وجوی قبلی هنوز در حال انجامه.")
                    .send()
                    .await?;

                return Ok(());
            }

            state.clear_results(chat_id);

            edit_status_message(
                &bot,
                chat_id,
                message_id,
                "🔎 دنبال چه نوع بیتی هستی؟\n\nسبک، Mood، سازها یا حس‌وحالی که می‌خوای رو برام توضیح بده.\n\nمثال:\nA dark melodic trap beat with emotional piano and hard 808s",
                query_keyboard(),
            )
            .await?;
        }

        // ------------------------------------
        // TRENDING
        // ------------------------------------
        "trending" => {
            remove_preview_message(&bot, chat_id, &state).await;

            if let Err(seconds) = state.check_search_rate_limit(chat_id) {
                bot.send_message(
                    chat_id,
                    format!(
                        "⏳ تعداد جست‌وجوهات زیاده. لطفاً {} ثانیه دیگه دوباره امتحان کن.",
                        seconds
                    ),
                )
                .send()
                .await?;

                return Ok(());
            }

            let Some(request_id) = state.begin_trending_request(chat_id, message_id) else {
                bot.send_message(chat_id, "⏳ ابتدا اجازه بده جست‌وجوی فعلی تموم بشه.")
                    .send()
                    .await?;

                return Ok(());
            };

            state.clear_results(chat_id);

            if let Err(error) = edit_status_message(
                &bot,
                chat_id,
                message_id,
                "🔥 دارم جدیدترین بیت‌های ترند Epsilone رو دریافت می‌کنم...\n\nلطفاً چند لحظه صبر کن.",
                loading_keyboard(),
            )
            .await
            {
                state.finish_request(
                    chat_id,
                    request_id,
                    ConversationState::Error {
                        request: SearchRequest::Trending,
                        message_id,
                    },
                );

                return Err(error);
            }

            execute_request(
                &bot,
                chat_id,
                &state,
                SearchRequest::Trending,
                request_id,
                message_id,
            )
            .await?;
        }

        // ------------------------------------
        // RETRY
        // ------------------------------------
        "retry_request" => {
            remove_preview_message(&bot, chat_id, &state).await;

            if let Err(seconds) = state.check_search_rate_limit(chat_id) {
                bot.send_message(
                    chat_id,
                    format!(
                        "⏳ تعداد جست‌وجوهات زیاده. لطفاً {} ثانیه دیگه دوباره امتحان کن.",
                        seconds
                    ),
                )
                .send()
                .await?;

                return Ok(());
            }

            let Some((request_id, request, status_message_id)) =
                state.begin_retry_request(chat_id, message_id)
            else {
                bot.send_message(
                    chat_id,
                    "این درخواست دیگه قابل تکرار نیست. یک جست‌وجوی جدید شروع کن.",
                )
                .send()
                .await?;

                return Ok(());
            };

            if let Err(error) = edit_status_message(
                &bot,
                chat_id,
                status_message_id,
                "🔄 دوباره دارم درخواستت رو بررسی می‌کنم...",
                loading_keyboard(),
            )
            .await
            {
                state.finish_request(
                    chat_id,
                    request_id,
                    ConversationState::Error {
                        request: request.clone(),
                        message_id: status_message_id,
                    },
                );

                return Err(error);
            }

            execute_request(
                &bot,
                chat_id,
                &state,
                request,
                request_id,
                status_message_id,
            )
            .await?;
        }

        // ------------------------------------
        // MAIN MENU
        // ------------------------------------
        "main_menu" | "cancel_search" => {
            remove_preview_message(&bot, chat_id, &state).await;
            state.clear_results(chat_id);

            for old_id in state.take_result_message_ids(chat_id) {
                if old_id != message_id {
                    let _ = bot.delete_message(chat_id, old_id).send().await;
                }
            }

            state.set_state(chat_id, ConversationState::Home);

            show_welcome_from_callback(&bot, &message, &state.site_url).await?;
        }

        _ => {}
    }

    Ok(())
}

async fn play_current_beat(
    bot: &teloxide::Bot,
    chat_id: teloxide::types::ChatId,
    state: &AppState,
    message_id: i32,
) -> HandlerResult {
    if !state.is_current_result_message(chat_id, message_id) {
        return Ok(());
    }

    let Some((beat, _, _)) = state.current_result(chat_id) else {
        return Ok(());
    };

    if !state.has_cached_preview(&beat.id) {
        bot.send_message(chat_id, "⚠️ Preview این بیت در دسترس نیست.")
            .send()
            .await?;

        return Ok(());
    }

    if let Err(seconds) = state.check_preview_rate_limit(chat_id) {
        bot.send_message(
            chat_id,
            format!(
                "⏳ درخواست‌های Preview زیاده. لطفاً {} ثانیه دیگه دوباره امتحان کن.",
                seconds
            ),
        )
        .send()
        .await?;

        return Ok(());
    }

    if !state.begin_preview(chat_id, &beat.id) {
        bot.send_message(chat_id, "⏳ این Preview در حال آماده‌سازیه.")
            .send()
            .await?;

        return Ok(());
    }

    // Only one preview voice message is kept in the chat at a time.
    remove_preview_message(bot, chat_id, state).await;

    let result = send_voice_preview(bot, chat_id, state, &beat.id).await;

    state.finish_preview(chat_id, &beat.id);

    match result {
        Ok(preview_message_id) => {
            let still_selected = state
                .current_result(chat_id)
                .map(|(current, _, _)| current.id == beat.id)
                .unwrap_or(false);

            if still_selected && state.is_current_result_message(chat_id, message_id) {
                state.set_preview_message_id(chat_id, preview_message_id);
            } else {
                // The user navigated while the audio was being uploaded.
                let _ = bot.delete_message(chat_id, preview_message_id).send().await;
            }
        }

        Err(error) => {
            log::warn!("Voice preview error for beat {}: {}", beat.id, error);

            bot.send_message(
                chat_id,
                "⚠️ پخش Preview با مشکل مواجه شد. لطفاً دوباره امتحان کن.",
            )
            .send()
            .await?;
        }
    }

    Ok(())
}

async fn remove_preview_message(
    bot: &teloxide::Bot,
    chat_id: teloxide::types::ChatId,
    state: &AppState,
) {
    if let Some(message_id) = state.take_preview_message_id(chat_id) {
        if let Err(error) = bot.delete_message(chat_id, message_id).send().await {
            log::debug!(
                "Could not delete previous preview message {}: {}",
                message_id,
                error
            );
        }
    }
}
