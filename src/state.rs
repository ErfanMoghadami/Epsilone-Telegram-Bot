use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use reqwest::Client;
use teloxide::types::ChatId;

use crate::{
    config::Config,
    data::PersistentStore,
    models::{Beat, PreviewInfo},
};

#[derive(Clone, Debug)]
pub enum SearchRequest {
    Ai { query: String },
    Trending,
}

#[derive(Clone, Debug, Default)]
pub enum ConversationState {
    #[default]
    Home,

    WaitingForQuery {
        message_id: i32,
    },

    Searching {
        request_id: u64,
        message_id: i32,
    },

    ShowingResults {
        message_id: i32,
    },

    Error {
        request: SearchRequest,
        message_id: i32,
    },
}

#[derive(Clone)]
pub struct AppState {
    pub http: Client,
    pub site_url: String,

    persistent: PersistentStore,

    conversations: Arc<Mutex<HashMap<i64, ConversationState>>>,

    preview_cache: Arc<Mutex<HashMap<String, PreviewInfo>>>,

    active_previews: Arc<Mutex<HashSet<(i64, String)>>>,

    results: Arc<Mutex<HashMap<i64, (Vec<Beat>, usize)>>>,

    result_message_ids: Arc<Mutex<HashMap<i64, Vec<i32>>>>,

    result_message_is_photo: Arc<Mutex<HashMap<i64, bool>>>,

    preview_message_ids: Arc<Mutex<HashMap<i64, i32>>>,

    search_attempts: Arc<Mutex<HashMap<i64, VecDeque<Instant>>>>,

    preview_attempts: Arc<Mutex<HashMap<i64, VecDeque<Instant>>>>,

    next_request_id: Arc<AtomicU64>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let http = config
            .build_http_client()
            .expect("Failed to build HTTP client");

        let persistent = PersistentStore::from_env();

        let mut conversations = HashMap::new();
        let mut results = HashMap::new();
        let mut result_message_ids = HashMap::new();
        let mut result_message_is_photo = HashMap::new();
        let mut preview_message_ids = HashMap::new();

        let snapshots = persistent.carousels();

        let mut restored_beats = Vec::new();

        for (chat_id, snapshot) in &snapshots {
            if snapshot.beats.is_empty() {
                persistent.remove_carousel(*chat_id);
                continue;
            }

            let Some(message_id) = snapshot.message_id else {
                // An unfinished carousel must not be restored.
                persistent.remove_carousel(*chat_id);
                continue;
            };

            let index = snapshot.index.min(snapshot.beats.len() - 1);

            results.insert(*chat_id, (snapshot.beats.clone(), index));

            result_message_ids.insert(*chat_id, vec![message_id]);

            result_message_is_photo.insert(*chat_id, snapshot.is_photo);

            if let Some(preview_id) = snapshot.preview_message_id {
                preview_message_ids.insert(*chat_id, preview_id);
            }

            conversations.insert(*chat_id, ConversationState::ShowingResults { message_id });

            restored_beats.extend(snapshot.beats.clone());
        }

        let state = Self {
            http,
            site_url: config.site_url,

            persistent,

            conversations: Arc::new(Mutex::new(conversations)),

            preview_cache: Arc::new(Mutex::new(HashMap::new())),

            active_previews: Arc::new(Mutex::new(HashSet::new())),

            results: Arc::new(Mutex::new(results)),

            result_message_ids: Arc::new(Mutex::new(result_message_ids)),

            result_message_is_photo: Arc::new(Mutex::new(result_message_is_photo)),

            preview_message_ids: Arc::new(Mutex::new(preview_message_ids)),

            search_attempts: Arc::new(Mutex::new(HashMap::new())),

            preview_attempts: Arc::new(Mutex::new(HashMap::new())),

            next_request_id: Arc::new(AtomicU64::new(1)),
        };

        // Rebuild in-memory preview metadata for restored carousels.
        state.cache_beat_previews(&restored_beats);

        state
    }

    // ----------------------------------------
    // CONVERSATION STATE
    // ----------------------------------------

    pub fn get_state(&self, chat_id: ChatId) -> ConversationState {
        self.conversations
            .lock()
            .expect("Conversation lock poisoned")
            .get(&chat_id.0)
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_state(&self, chat_id: ChatId, new_state: ConversationState) {
        self.conversations
            .lock()
            .expect("Conversation lock poisoned")
            .insert(chat_id.0, new_state);
    }

    pub fn current_message_id(&self, chat_id: ChatId) -> Option<i32> {
        match self.get_state(chat_id) {
            ConversationState::Home => None,

            ConversationState::WaitingForQuery { message_id }
            | ConversationState::ShowingResults { message_id }
            | ConversationState::Error { message_id, .. }
            | ConversationState::Searching { message_id, .. } => Some(message_id),
        }
    }

    pub fn enter_waiting_for_query(&self, chat_id: ChatId, message_id: i32) -> bool {
        let mut conversations = self
            .conversations
            .lock()
            .expect("Conversation lock poisoned");

        if matches!(
            conversations.get(&chat_id.0),
            Some(ConversationState::Searching { .. })
        ) {
            return false;
        }

        conversations.insert(chat_id.0, ConversationState::WaitingForQuery { message_id });

        true
    }

    pub fn begin_ai_request(&self, chat_id: ChatId) -> Option<(u64, i32)> {
        let mut conversations = self
            .conversations
            .lock()
            .expect("Conversation lock poisoned");

        let message_id = match conversations.get(&chat_id.0).cloned().unwrap_or_default() {
            ConversationState::WaitingForQuery { message_id } => message_id,
            _ => return None,
        };

        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);

        conversations.insert(
            chat_id.0,
            ConversationState::Searching {
                request_id,
                message_id,
            },
        );

        Some((request_id, message_id))
    }

    pub fn begin_trending_request(&self, chat_id: ChatId, message_id: i32) -> Option<u64> {
        let mut conversations = self
            .conversations
            .lock()
            .expect("Conversation lock poisoned");

        if matches!(
            conversations.get(&chat_id.0),
            Some(ConversationState::Searching { .. })
        ) {
            return None;
        }

        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);

        conversations.insert(
            chat_id.0,
            ConversationState::Searching {
                request_id,
                message_id,
            },
        );

        Some(request_id)
    }

    pub fn begin_retry_request(
        &self,
        chat_id: ChatId,
        callback_message_id: i32,
    ) -> Option<(u64, SearchRequest, i32)> {
        let mut conversations = self
            .conversations
            .lock()
            .expect("Conversation lock poisoned");

        let (request, message_id) = match conversations.get(&chat_id.0).cloned().unwrap_or_default()
        {
            ConversationState::Error {
                request,
                message_id,
            } => (request, message_id),

            _ => return None,
        };

        if message_id != callback_message_id {
            return None;
        }

        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);

        conversations.insert(
            chat_id.0,
            ConversationState::Searching {
                request_id,
                message_id,
            },
        );

        Some((request_id, request, message_id))
    }

    pub fn is_current_request(&self, chat_id: ChatId, request_id: u64) -> bool {
        matches!(
            self.get_state(chat_id),
            ConversationState::Searching {
                request_id: active_id,
                ..
            } if active_id == request_id
        )
    }

    pub fn finish_request(
        &self,
        chat_id: ChatId,
        request_id: u64,
        next_state: ConversationState,
    ) -> bool {
        let mut conversations = self
            .conversations
            .lock()
            .expect("Conversation lock poisoned");

        let is_current = matches!(
            conversations.get(&chat_id.0),
            Some(ConversationState::Searching {
                request_id: active_id,
                ..
            }) if *active_id == request_id
        );

        if is_current {
            conversations.insert(chat_id.0, next_state);
        }

        is_current
    }

    // ----------------------------------------
    // PREVIEW URL CACHE
    // ----------------------------------------

    pub fn cache_beat_previews(&self, beats: &[Beat]) {
        let mut cache = self.preview_cache.lock().expect("Preview lock poisoned");

        for beat in beats {
            let Some(preview_url) = beat.preview_url.as_deref() else {
                continue;
            };

            let Ok(url) = reqwest::Url::parse(preview_url) else {
                continue;
            };

            if url.scheme() != "https"
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
            {
                continue;
            }

            if !cache.contains_key(&beat.id) && cache.len() >= 200 {
                if let Some(old_key) = cache.keys().next().cloned() {
                    cache.remove(&old_key);
                }
            }

            cache.insert(
                beat.id.clone(),
                PreviewInfo {
                    url: url.to_string(),
                    title: beat
                        .title
                        .clone()
                        .unwrap_or_else(|| "Untitled Beat".to_string()),
                },
            );
        }
    }

    pub fn has_cached_preview(&self, beat_id: &str) -> bool {
        self.preview_cache
            .lock()
            .map(|cache| cache.contains_key(beat_id))
            .unwrap_or(false)
    }

    pub fn get_preview(&self, beat_id: &str) -> Option<PreviewInfo> {
        self.preview_cache.lock().ok()?.get(beat_id).cloned()
    }

    // ----------------------------------------
    // TELEGRAM FILE ID CACHE
    // ----------------------------------------

    pub fn get_telegram_file_id(&self, beat_id: &str, preview_url: &str) -> Option<String> {
        self.persistent.get_telegram_file_id(beat_id, preview_url)
    }

    pub fn set_telegram_file_id(&self, beat_id: &str, preview_url: &str, file_id: String) {
        self.persistent
            .set_telegram_file_id(beat_id, preview_url, file_id);
    }

    // ----------------------------------------
    // FAVORITES
    // ----------------------------------------

    pub fn favorites(&self, chat_id: ChatId) -> Vec<Beat> {
        self.persistent.favorites(chat_id.0)
    }

    pub fn is_favorite(&self, chat_id: ChatId, beat_id: &str) -> bool {
        self.persistent.is_favorite(chat_id.0, beat_id)
    }

    pub fn toggle_favorite(&self, chat_id: ChatId, beat: Beat) -> bool {
        self.persistent.toggle_favorite(chat_id.0, beat)
    }

    pub fn is_favorites_view(&self, chat_id: ChatId) -> bool {
        self.persistent.is_favorites_view(chat_id.0)
    }

    /// Creates a recommendation prompt using the user's saved beats.
    /// It uses the existing /api/recommend endpoint.
    pub fn personalized_query(&self, chat_id: ChatId) -> Option<String> {
        let favorites = self.favorites(chat_id);

        if favorites.is_empty() {
            return None;
        }

        let mut genre_counts: HashMap<String, usize> = HashMap::new();

        for beat in &favorites {
            if let Some(genre) = beat.genre.as_deref() {
                let genre = genre.trim();

                if !genre.is_empty() {
                    *genre_counts.entry(genre.to_string()).or_default() += 1;
                }
            }
        }

        let mut genres: Vec<(String, usize)> = genre_counts.into_iter().collect();

        genres.sort_by(|a, b| b.1.cmp(&a.1));

        let preferred_genres = if genres.is_empty() {
            "Use the saved beat references to infer the user's preferences.".to_string()
        } else {
            genres
                .iter()
                .take(5)
                .map(|(genre, _)| genre.clone())
                .collect::<Vec<_>>()
                .join(", ")
        };

        let examples = favorites
            .iter()
            .take(5)
            .map(|beat| {
                let title = beat.title.as_deref().unwrap_or("Untitled");
                let producer = beat.producer_username.as_deref().unwrap_or("Unknown");

                format!("{} by {}", title, producer)
            })
            .collect::<Vec<_>>()
            .join("; ");

        Some(format!(
            "Recommend new beats based on my saved favorites. \
             My preferred genres include: {}. \
             Some beats I saved: {}. \
             Find new beats with similar musical characteristics, \
             but do not simply return the exact same saved beats.",
            preferred_genres, examples
        ))
    }

    // ----------------------------------------
    // PERSISTENT CAROUSEL
    // ----------------------------------------

    pub fn set_results(&self, chat_id: ChatId, beats: Vec<Beat>) {
        self.set_results_with_mode(chat_id, beats, false);
    }

    pub fn set_favorite_results(&self, chat_id: ChatId, beats: Vec<Beat>) {
        self.set_results_with_mode(chat_id, beats, true);
    }

    fn set_results_with_mode(&self, chat_id: ChatId, beats: Vec<Beat>, favorites_view: bool) {
        self.persistent
            .save_carousel(chat_id.0, beats.clone(), favorites_view);

        if let Ok(mut results) = self.results.lock() {
            results.insert(chat_id.0, (beats, 0));
        }
    }

    pub fn current_result(&self, chat_id: ChatId) -> Option<(Beat, usize, usize)> {
        let results = self.results.lock().ok()?;
        let (beats, index) = results.get(&chat_id.0)?;

        let total = beats.len();
        let beat = beats.get(*index)?.clone();

        Some((beat, *index, total))
    }

    pub fn move_result(&self, chat_id: ChatId, direction: i32) -> Option<(Beat, usize, usize)> {
        let mut results = self.results.lock().ok()?;
        let (beats, index) = results.get_mut(&chat_id.0)?;

        let total = beats.len();

        let next_index = carousel_index(*index, total, direction)?;

        *index = next_index;

        self.persistent.update_carousel_index(chat_id.0, *index);

        Some((beats.get(*index)?.clone(), *index, total))
    }

    pub fn take_result_message_ids(&self, chat_id: ChatId) -> Vec<i32> {
        let ids = self
            .result_message_ids
            .lock()
            .ok()
            .and_then(|mut all| all.remove(&chat_id.0))
            .unwrap_or_default();

        self.persistent.set_carousel_message_id(chat_id.0, None);

        ids
    }

    pub fn set_result_message_ids(&self, chat_id: ChatId, ids: Vec<i32>) {
        let message_id = ids.first().copied();

        if let Ok(mut all) = self.result_message_ids.lock() {
            all.insert(chat_id.0, ids);
        }

        self.persistent
            .set_carousel_message_id(chat_id.0, message_id);
    }

    pub fn is_current_result_message(&self, chat_id: ChatId, message_id: i32) -> bool {
        self.result_message_ids
            .lock()
            .ok()
            .and_then(|all| all.get(&chat_id.0).cloned())
            .map(|ids| ids.contains(&message_id))
            .unwrap_or(false)
    }

    pub fn set_result_message_is_photo(&self, chat_id: ChatId, is_photo: bool) {
        if let Ok(mut all) = self.result_message_is_photo.lock() {
            all.insert(chat_id.0, is_photo);
        }

        self.persistent.set_carousel_photo(chat_id.0, is_photo);
    }

    pub fn result_message_is_photo(&self, chat_id: ChatId) -> bool {
        self.result_message_is_photo
            .lock()
            .ok()
            .and_then(|all| all.get(&chat_id.0).copied())
            .unwrap_or(false)
    }

    pub fn clear_results(&self, chat_id: ChatId) {
        if let Ok(mut results) = self.results.lock() {
            results.remove(&chat_id.0);
        }

        self.persistent.remove_carousel(chat_id.0);
    }

    // ----------------------------------------
    // ACTIVE PREVIEW MESSAGE
    // ----------------------------------------

    pub fn set_preview_message_id(&self, chat_id: ChatId, message_id: i32) {
        if let Ok(mut all) = self.preview_message_ids.lock() {
            all.insert(chat_id.0, message_id);
        }

        self.persistent
            .set_preview_message_id(chat_id.0, Some(message_id));
    }

    pub fn take_preview_message_id(&self, chat_id: ChatId) -> Option<i32> {
        let message_id = self
            .preview_message_ids
            .lock()
            .ok()
            .and_then(|mut all| all.remove(&chat_id.0));

        self.persistent.set_preview_message_id(chat_id.0, None);

        message_id
    }

    // ----------------------------------------
    // ACTIVE PREVIEW LOCK
    // ----------------------------------------

    pub fn begin_preview(&self, chat_id: ChatId, beat_id: &str) -> bool {
        self.active_previews
            .lock()
            .expect("Preview lock poisoned")
            .insert((chat_id.0, beat_id.to_string()))
    }

    pub fn finish_preview(&self, chat_id: ChatId, beat_id: &str) {
        if let Ok(mut active) = self.active_previews.lock() {
            active.remove(&(chat_id.0, beat_id.to_string()));
        }
    }

    // ----------------------------------------
    // RATE LIMITING
    // ----------------------------------------

    pub fn check_search_rate_limit(&self, chat_id: ChatId) -> Result<(), u64> {
        Self::check_rate_limit(&self.search_attempts, chat_id.0, 5, Duration::from_secs(60))
    }

    pub fn check_preview_rate_limit(&self, chat_id: ChatId) -> Result<(), u64> {
        Self::check_rate_limit(
            &self.preview_attempts,
            chat_id.0,
            10,
            Duration::from_secs(60),
        )
    }

    fn check_rate_limit(
        store: &Arc<Mutex<HashMap<i64, VecDeque<Instant>>>>,
        key: i64,
        max_requests: usize,
        window: Duration,
    ) -> Result<(), u64> {
        let now = Instant::now();

        let mut all = store.lock().expect("Rate-limit lock poisoned");

        let attempts = all.entry(key).or_default();

        while attempts
            .front()
            .is_some_and(|time| now.duration_since(*time) >= window)
        {
            attempts.pop_front();
        }

        if attempts.len() >= max_requests {
            let retry_after = attempts
                .front()
                .map(|oldest| {
                    window
                        .saturating_sub(now.duration_since(*oldest))
                        .as_secs()
                        .max(1)
                })
                .unwrap_or(1);

            return Err(retry_after);
        }

        attempts.push_back(now);

        Ok(())
    }
}

// ----------------------------------------
// CAROUSEL INDEX LOGIC
// ----------------------------------------

fn carousel_index(current: usize, total: usize, direction: i32) -> Option<usize> {
    if total < 2 || current >= total {
        return None;
    }

    Some(if direction < 0 {
        if current == 0 { total - 1 } else { current - 1 }
    } else {
        (current + 1) % total
    })
}

#[cfg(test)]
mod tests {
    use super::carousel_index;

    #[test]
    fn next_wraps_from_last_to_first() {
        assert_eq!(carousel_index(4, 5, 1), Some(0));
    }

    #[test]
    fn previous_wraps_from_first_to_last() {
        assert_eq!(carousel_index(0, 5, -1), Some(4));
    }

    #[test]
    fn normal_next_and_previous_work() {
        assert_eq!(carousel_index(1, 5, 1), Some(2));
        assert_eq!(carousel_index(2, 5, -1), Some(1));
    }

    #[test]
    fn one_item_has_no_navigation() {
        assert_eq!(carousel_index(0, 1, 1), None);
    }

    #[test]
    fn invalid_index_is_rejected() {
        assert_eq!(carousel_index(5, 5, 1), None);
    }
}
