use teloxide::{
    payloads::SendMessageSetters, prelude::Requester, requests::Request, types::Message,
};

use crate::{
    error::HandlerResult,
    keyboards::{main_keyboard, results_keyboard, retry_keyboard},
    state::{AppState, ConversationState, SearchRequest},
    ui::{edit_status_message, show_welcome},
};

use super::search::execute_request;

const MAX_QUERY_LENGTH: usize = 500;

pub async fn message_handler(bot: teloxide::Bot, msg: Message, state: AppState) -> HandlerResult {
    let chat_id = msg.chat.id;

    let Some(raw_text) = msg.text() else {
        if matches!(
            state.get_state(chat_id),
            ConversationState::WaitingForQuery { .. }
        ) {
            bot.send_message(chat_id, "لطفاً توضیحاتت رو به صورت متن بفرست.")
                .send()
                .await?;
        }

        return Ok(());
    };

    let text = raw_text.trim();
    let command = text.split_whitespace().next().unwrap_or("");

    // ----------------------------------------
    // COMMANDS
    // ----------------------------------------

    // ----------------------------------------
    // PRODUCER TELEGRAM LINK
    // ----------------------------------------

    if command == "/link" || command.starts_with("/link@") {
        // Linking is allowed only in private chats.
        if chat_id.0 <= 0 {
            bot.send_message(
                chat_id,
                "برای اتصال حساب Producer، لطفاً وارد چت خصوصی ربات شو.",
            )
            .send()
            .await?;

            return Ok(());
        }

        if state
            .notifications
            .producer_id_for_chat(chat_id.0)
            .is_some()
        {
            bot.send_message(
                chat_id,
                "✅ حساب Producer تو قبلاً به تلگرام متصل شده.\n\nبرای قطع اتصال از /unlink استفاده کن.",
            )
            .send()
            .await?;

            return Ok(());
        }

        let code = state.notifications.issue_link_code(chat_id.0);

        bot.send_message(
            chat_id,
            format!(
                "🔗 کد اتصال تلگرام:\n\n\
                <code>{}</code>\n\n\
                این کد ۱۰ دقیقه اعتبار داره و فقط یک‌بار قابل استفاده است.\n\n\
                وارد پنل Producer در Epsilone شو و کد رو در بخش اتصال Telegram وارد کن.",
                code
            ),
        )
        .parse_mode(teloxide::types::ParseMode::Html)
        .send()
        .await?;

        return Ok(());
    }

    // ----------------------------------------
    // PRODUCER TELEGRAM UNLINK
    // ----------------------------------------

    if command == "/unlink" || command.starts_with("/unlink@") {
        if chat_id.0 <= 0 {
            return Ok(());
        }

        match state.notifications.unlink_chat(chat_id.0) {
            Some(_) => {
                bot.send_message(chat_id, "✅ اتصال حساب Producer از تلگرام قطع شد.")
                    .send()
                    .await?;
            }

            None => {
                bot.send_message(chat_id, "این حساب تلگرام به هیچ Producer متصل نیست.")
                    .send()
                    .await?;
            }
        }

        return Ok(());
    }

    if command == "/start"
        || command.starts_with("/start@")
        || command == "/menu"
        || command.starts_with("/menu@")
        || command == "/cancel"
        || command.starts_with("/cancel@")
    {
        let old_message_id = state.current_message_id(chat_id);
        if let Some(preview_message_id) = state.take_preview_message_id(chat_id) {
            let _ = bot.delete_message(chat_id, preview_message_id).send().await;
        }
        for old_id in state.take_result_message_ids(chat_id) {
            let _ = bot.delete_message(chat_id, old_id).send().await;
        }
        // /start deletes the carousel message, so the next UI message starts as text.
        state.set_result_message_is_photo(chat_id, false);
        state.clear_results(chat_id);

        state.set_state(chat_id, ConversationState::Home);

        show_welcome(&bot, chat_id, old_message_id, &state.site_url).await?;

        return Ok(());
    }

    // ----------------------------------------
    // CONVERSATION STATES
    // ----------------------------------------

    match state.get_state(chat_id) {
        ConversationState::Home => {
            bot.send_message(
                chat_id,
                "برای شروع یکی از گزینه‌های منوی اصلی رو انتخاب کن. 👇",
            )
            .reply_markup(main_keyboard(&state.site_url))
            .send()
            .await?;
        }

        ConversationState::WaitingForQuery { .. } => {
            if text.is_empty() {
                bot.send_message(chat_id, "لطفاً توضیحاتت رو ارسال کن.")
                    .send()
                    .await?;

                return Ok(());
            }

            if text.encode_utf16().count() > MAX_QUERY_LENGTH {
                bot.send_message(
                    chat_id,
                    "توضیحاتت نباید بیشتر از ۵۰۰ کاراکتر باشه. دوباره امتحان کن.",
                )
                .send()
                .await?;

                return Ok(());
            }

            // Per-chat search rate limit.
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

            state.clear_results(chat_id);

            let Some((request_id, status_message_id)) = state.begin_ai_request(chat_id) else {
                return Ok(());
            };

            let request = SearchRequest::Ai {
                query: text.to_string(),
            };

            if let Err(error) = edit_status_message(
                &bot,
                chat_id,
                status_message_id,
                "🔎 دارم دنبال بیت‌های مناسب سلیقه‌ات می‌گردم...\n\nممکنه چند لحظه طول بکشه.",
                crate::keyboards::loading_keyboard(),
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

        ConversationState::Searching { .. } => {
            // Ignore extra messages while a request is running.
        }

        ConversationState::ShowingResults { .. } => {
            bot.send_message(
                chat_id,
                "نتایج جست‌وجوی قبلی نمایش داده شده. برای جست‌وجوی جدید از دکمه زیر استفاده کن.",
            )
            .reply_markup(results_keyboard())
            .send()
            .await?;
        }

        ConversationState::Error { .. } => {
            bot.send_message(
                chat_id,
                "درخواست قبلی با مشکل مواجه شد. از دکمه Retry یا منوی اصلی استفاده کن.",
            )
            .reply_markup(retry_keyboard())
            .send()
            .await?;
        }
    }

    Ok(())
}
