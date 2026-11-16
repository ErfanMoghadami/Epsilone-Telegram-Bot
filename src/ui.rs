use teloxide::{
    payloads::{EditMessageCaptionSetters, EditMessageTextSetters, SendMessageSetters},
    prelude::Requester,
    requests::Request,
    types::{ChatId, InlineKeyboardMarkup, Message, ParseMode},
};

use crate::{error::HandlerResult, keyboards::main_keyboard};

pub const WELCOME_TEXT: &str = r#"
<b>🎧 به Epsilone خوش اومدی!</b>

اینجا قراره بیت بعدی خودتو پیدا کنی.

با کمک هوش مصنوعی، بیت‌هایی متناسب با سبک و حس‌وحال موردعلاقه‌ات پیدا کن یا جدیدترین بیت‌های ترند رو کشف کن.

<i>صدای بعدی تو، همین‌جاست.</i> 🖤
"#;

pub async fn edit_status_message(
    bot: &teloxide::Bot,
    chat_id: ChatId,
    message_id: i32,
    text: &str,
    keyboard: InlineKeyboardMarkup,
) -> HandlerResult {
    // Try editing as text first.
    match bot
        .edit_message_text(chat_id, message_id, text)
        .reply_markup(keyboard.clone())
        .send()
        .await
    {
        Ok(_) => Ok(()),

        Err(text_error) => {
            log::debug!(
                "Could not edit message {} as text; trying caption edit: {}",
                message_id,
                text_error
            );

            // If it is a photo message, update its caption instead.
            bot.edit_message_caption(chat_id, message_id)
                .caption(text)
                .reply_markup(keyboard)
                .send()
                .await?;

            Ok(())
        }
    }
}

pub async fn show_welcome(
    bot: &teloxide::Bot,
    chat_id: ChatId,
    message_id: Option<i32>,
    site_url: &str,
) -> HandlerResult {
    if let Some(message_id) = message_id {
        match bot
            .edit_message_text(chat_id, message_id, WELCOME_TEXT)
            .parse_mode(ParseMode::Html)
            .reply_markup(main_keyboard(site_url))
            .send()
            .await
        {
            Ok(_) => return Ok(()),
            Err(error) => {
                log::warn!("Could not edit menu message; sending a new one: {}", error);
            }
        }
    }

    bot.send_message(chat_id, WELCOME_TEXT)
        .parse_mode(ParseMode::Html)
        .reply_markup(main_keyboard(site_url))
        .send()
        .await?;

    Ok(())
}

pub async fn show_welcome_from_callback(
    bot: &teloxide::Bot,
    message: &Message,
    site_url: &str,
) -> HandlerResult {
    let chat_id = message.chat.id;
    let message_id = message.id;
    let keyboard = main_keyboard(site_url);

    // Photo/media messages cannot be converted into text messages.
    // Delete the old beat card and send a clean menu instead.
    if message.caption().is_some() {
        if let Err(error) = bot.delete_message(chat_id, message_id).send().await {
            log::warn!(
                "Could not delete previous beat card {}: {}",
                message_id,
                error
            );
        }

        bot.send_message(chat_id, WELCOME_TEXT)
            .parse_mode(ParseMode::Html)
            .reply_markup(keyboard)
            .send()
            .await?;

        return Ok(());
    }

    // Text messages can safely be edited in place.
    if message.text().is_some() {
        match bot
            .edit_message_text(chat_id, message_id, WELCOME_TEXT)
            .parse_mode(ParseMode::Html)
            .reply_markup(keyboard.clone())
            .send()
            .await
        {
            Ok(_) => return Ok(()),

            Err(error) => {
                log::warn!("Could not edit text message to main menu: {}", error);
            }
        }
    }

    // Fallback for unsupported message types.
    bot.send_message(chat_id, WELCOME_TEXT)
        .parse_mode(ParseMode::Html)
        .reply_markup(keyboard)
        .send()
        .await?;

    Ok(())
}
