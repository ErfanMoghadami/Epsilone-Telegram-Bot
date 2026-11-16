mod config;
mod data;
mod error;
mod handlers;
mod keyboards;
mod models;
mod services;
mod state;
mod ui;

use teloxide::{prelude::*, requests::Request, types::BotCommand};

use crate::{
    config::Config,
    handlers::{callback_handler, message_handler},
    state::AppState,
};

#[tokio::main]
async fn main() {
    // Load environment variables.
    dotenvy::dotenv().ok();

    pretty_env_logger::init();

    println!("Starting Epsilone Telegram Bot...");

    let bot = Bot::from_env();

    // ----------------------------------------
    // REGISTER TELEGRAM MENU COMMANDS
    // ----------------------------------------

    let commands = vec![
        BotCommand::new("start", "شروع ربات"),
        BotCommand::new("menu", "نمایش منوی اصلی"),
        BotCommand::new("cancel", "لغو عملیات جاری"),
        BotCommand::new("link", "اتصال حساب Producer به تلگرام"),
        BotCommand::new("unlink", "قطع اتصال Producer از تلگرام"),
    ];

    match bot.set_my_commands(commands).send().await {
        Ok(_) => {
            println!("Telegram menu commands registered successfully.");
        }

        Err(error) => {
            eprintln!("Failed to register Telegram menu commands: {}", error);
        }
    }

    // ----------------------------------------
    // APPLICATION CONFIGURATION
    // ----------------------------------------

    let config = Config::from_env();

    let state = AppState::new(config);

    // ----------------------------------------
    // PRODUCER NOTIFICATION HTTP API
    // ----------------------------------------

    let notification_secret = std::env::var("TELEGRAM_INTERNAL_API_SECRET")
        .ok()
        .filter(|value| !value.trim().is_empty());

    if let Some(secret) = notification_secret {
        let bind = std::env::var("TELEGRAM_INTERNAL_BIND")
            .unwrap_or_else(|_| "127.0.0.1:3031".to_string());

        let api_bot = bot.clone();
        let api_state = state.clone();

        let _notification_api = tokio::spawn(async move {
            if let Err(error) =
                crate::services::producer_notifications::run(api_bot, api_state, bind, secret).await
            {
                log::error!("Producer notification API stopped: {}", error);
            }
        });
    } else {
        log::warn!(
            "Producer notification API disabled: \
             TELEGRAM_INTERNAL_API_SECRET is missing."
        );
    }

    // ----------------------------------------
    // DISPATCHER
    // ----------------------------------------

    let handler = dptree::entry()
        .branch(Update::filter_message().endpoint(message_handler))
        .branch(Update::filter_callback_query().endpoint(callback_handler));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![state])
        .build()
        .dispatch()
        .await;
}
