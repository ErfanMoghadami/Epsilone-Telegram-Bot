use reqwest::Url;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};

pub fn main_keyboard(site_url: &str) -> InlineKeyboardMarkup {
    let website_url = Url::parse(site_url).expect("Invalid website URL");

    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback(
            "🔎 AI Beat Search",
            "ai_search",
        )],
        vec![InlineKeyboardButton::callback(
            "🔥 Trending Beats",
            "trending",
        )],
        vec![InlineKeyboardButton::callback(
            "⭐ Favorite Beats",
            "favorites",
        )],
        vec![InlineKeyboardButton::callback(
            "✨ Personalized for You",
            "personalized",
        )],
        vec![InlineKeyboardButton::url(
            "🌐 Visit Epsilone ↗",
            website_url,
        )],
    ])
}

pub fn query_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback(
            "❌ Cancel Search",
            "cancel_search",
        )],
        vec![InlineKeyboardButton::callback("🏠 Main Menu", "main_menu")],
    ])
}

pub fn loading_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        "🏠 Main Menu",
        "main_menu",
    )]])
}

pub fn results_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback(
            "🔎 Search Again",
            "search_again",
        )],
        vec![InlineKeyboardButton::callback("🏠 Main Menu", "main_menu")],
    ])
}

pub fn retry_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![InlineKeyboardButton::callback("🔄 Retry", "retry_request")],
        vec![InlineKeyboardButton::callback("🏠 Main Menu", "main_menu")],
    ])
}

pub fn carousel_keyboard(
    beat_url: &str,
    has_preview: bool,
    can_navigate: bool,
    is_favorite: bool,
) -> InlineKeyboardMarkup {
    let url = Url::parse(beat_url).expect("Invalid beat URL");

    let mut rows = Vec::new();
    let mut navigation = Vec::new();

    if can_navigate {
        navigation.push(InlineKeyboardButton::callback(
            "⬅️ Previous",
            "carousel:prev",
        ));
    }

    if has_preview {
        navigation.push(InlineKeyboardButton::callback("▶️ Play", "carousel:play"));
    }

    if can_navigate {
        navigation.push(InlineKeyboardButton::callback("Next ➡️", "carousel:next"));
    }

    if !navigation.is_empty() {
        rows.push(navigation);
    }

    let favorite_label = if is_favorite {
        "💔 Remove Favorite"
    } else {
        "❤️ Add to Favorites"
    };

    rows.push(vec![InlineKeyboardButton::callback(
        favorite_label,
        "favorite:toggle",
    )]);

    rows.push(vec![InlineKeyboardButton::url(
        "🛒 View / Buy on Epsilone ↗",
        url,
    )]);

    rows.push(vec![
        InlineKeyboardButton::callback("🔎 Search Again", "search_again"),
        InlineKeyboardButton::callback("🏠 Main Menu", "main_menu"),
    ]);

    InlineKeyboardMarkup::new(rows)
}
