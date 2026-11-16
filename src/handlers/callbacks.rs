use teloxide::{prelude::Requester, requests::Request, types::CallbackQuery};

use crate::{
    error::HandlerResult,
    keyboards::{loading_keyboard, main_keyboard, query_keyboard},
    services::preview::send_voice_preview,
    state::{AppState, ConversationState, SearchRequest},
    ui::{edit_status_message, show_welcome_from_callback},
};

use super::search::{
    execute_request, render_carousel_beat, show_adjacent_result, show_beats_in_carousel,
};

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
        // FAVORITE TOGGLE
        // ------------------------------------
        "favorite:toggle" => {
            if !state.is_current_result_message(chat_id, message_id) {
                return Ok(());
            }

            let Some((beat, index, total)) = state.current_result(chat_id) else {
                return Ok(());
            };

            let was_favorites_view = state.is_favorites_view(chat_id);

            let is_now_favorite = state.toggle_favorite(chat_id, beat.clone());

            // Removing a beat from the favorites carousel.
            if was_favorites_view && !is_now_favorite {
                remove_preview_message(&bot, chat_id, &state).await;

                let favorites = state.favorites(chat_id);

                if favorites.is_empty() {
                    state.clear_results(chat_id);
                    state.take_result_message_ids(chat_id);
                    state.set_state(chat_id, ConversationState::Home);

                    edit_status_message(
                        &bot,
                        chat_id,
                        message_id,
                        "⭐ دیگه هیچ بیتی در Favoriteهات باقی نمونده.\n\nهر وقت بیتی رو دوست داشتی، می‌تونی دوباره ذخیره‌اش کنی.",
                        main_keyboard(&state.site_url),
                    )
                    .await?;
                } else {
                    show_beats_in_carousel(&bot, chat_id, &state, message_id, favorites, true)
                        .await?;
                }
            } else {
                // Update the current card and its favorite button.
                render_carousel_beat(&bot, chat_id, &state, message_id, &beat, index, total)
                    .await?;
            }
        }

        // ------------------------------------
        // FAVORITES MENU
        // ------------------------------------
        "favorites" => {
            remove_preview_message(&bot, chat_id, &state).await;

            let favorites = state.favorites(chat_id);

            if favorites.is_empty() {
                state.clear_results(chat_id);
                state.take_result_message_ids(chat_id);
                state.set_state(chat_id, ConversationState::Home);

                edit_status_message(
                    &bot,
                    chat_id,
                    message_id,
                    "⭐ هنوز هیچ بیتی رو Favorite نکردی.\n\nداخل کارت هر بیت روی ❤️ Add to Favorites بزن تا اینجا ذخیره بشه.",
                    main_keyboard(&state.site_url),
                )
                .await?;
            } else {
                show_beats_in_carousel(&bot, chat_id, &state, message_id, favorites, true).await?;
            }
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
        // PERSONALIZED DISCOVERY
        // ------------------------------------
        "personalized" => {
            remove_preview_message(&bot, chat_id, &state).await;

            // Generate a query from the user's saved favorites.
            let Some(personalized_query) = state.personalized_query(chat_id) else {
                bot.send_message(
                    chat_id,
                    "✨ هنوز Favorite کافی نداری!\n\n\
                    اول چند بیت رو با دکمه ❤️ Add to Favorites ذخیره کن \
                    تا بتونم بر اساس سلیقه موسیقی‌ات پیشنهاد بدم.",
                )
                .send()
                .await?;

                return Ok(());
            };

            // Apply the existing search rate limit.
            if let Err(seconds) = state.check_search_rate_limit(chat_id) {
                bot.send_message(
                    chat_id,
                    format!("⏳ لطفاً {} ثانیه دیگه دوباره امتحان کن.", seconds),
                )
                .send()
                .await?;

                return Ok(());
            }

            if !state.enter_waiting_for_query(chat_id, message_id) {
                bot.send_message(chat_id, "⏳ یک درخواست دیگه هنوز در حال اجراست.")
                    .send()
                    .await?;

                return Ok(());
            }

            state.clear_results(chat_id);

            let Some((request_id, status_message_id)) = state.begin_ai_request(chat_id) else {
                return Ok(());
            };

            let request = SearchRequest::Personalized {
                query: personalized_query,
            };

            if let Err(error) = edit_status_message(
                &bot,
                chat_id,
                status_message_id,
                "✨ دارم بر اساس Favoriteهات بیت‌های جدید پیدا می‌کنم...\n\n\
                سلیقه موسیقی‌ات رو بررسی می‌کنم و دنبال بیت‌های مشابه می‌گردم.",
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

        // ------------------------------------
        // MAIN MENU
        // ------------------------------------
        "main_menu" | "cancel_search" => {
            remove_preview_message(&bot, chat_id, &state).await;

            // Capture the old card IDs before clearing the carousel.
            let old_message_ids = state.take_result_message_ids(chat_id);

            state.clear_results(chat_id);

            // Reset media-message state.
            state.set_result_message_is_photo(chat_id, false);

            // Delete older result messages, but let
            // show_welcome_from_callback handle the current one.
            for old_id in old_message_ids {
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
