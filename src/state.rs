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

    conversations: Arc<Mutex<HashMap<i64, ConversationState>>>,
    preview_cache: Arc<Mutex<HashMap<String, PreviewInfo>>>,
    active_previews: Arc<Mutex<HashSet<(i64, String)>>>,

    telegram_file_ids: Arc<Mutex<HashMap<String, String>>>,
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

        Self {
            http,
            site_url: config.site_url,
            conversations: Arc::new(Mutex::new(HashMap::new())),
            preview_cache: Arc::new(Mutex::new(HashMap::new())),
            active_previews: Arc::new(Mutex::new(HashSet::new())),
            telegram_file_ids: Arc::new(Mutex::new(HashMap::new())),
            results: Arc::new(Mutex::new(HashMap::new())),
            result_message_ids: Arc::new(Mutex::new(HashMap::new())),
            result_message_is_photo: Arc::new(Mutex::new(HashMap::new())),
            preview_message_ids: Arc::new(Mutex::new(HashMap::new())),
            search_attempts: Arc::new(Mutex::new(HashMap::new())),
            preview_attempts: Arc::new(Mutex::new(HashMap::new())),
            next_request_id: Arc::new(AtomicU64::new(1)),
        }
    }

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
        let cache_key = format!("{}|{}", beat_id, preview_url);

        self.telegram_file_ids.lock().ok()?.get(&cache_key).cloned()
    }

    pub fn set_telegram_file_id(&self, beat_id: &str, preview_url: &str, file_id: String) {
        let cache_key = format!("{}|{}", beat_id, preview_url);

        if let Ok(mut cache) = self.telegram_file_ids.lock() {
            if cache.len() >= 1000 && !cache.contains_key(&cache_key) {
                if let Some(key) = cache.keys().next().cloned() {
                    cache.remove(&key);
                }
            }

            cache.insert(cache_key, file_id);
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

    // ----------------------------------------
    // PAGINATION
    // ----------------------------------------

    // ----------------------------------------
    // BEAT CAROUSEL
    // ----------------------------------------

    pub fn set_results(&self, chat_id: ChatId, beats: Vec<Beat>) {
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

    /// Move through search results as a circular carousel.
    pub fn move_result(&self, chat_id: ChatId, direction: i32) -> Option<(Beat, usize, usize)> {
        let mut results = self.results.lock().ok()?;
        let (beats, index) = results.get_mut(&chat_id.0)?;

        let total = beats.len();

        if total < 2 {
            return None;
        }

        *index = if direction < 0 {
            if *index == 0 { total - 1 } else { *index - 1 }
        } else {
            (*index + 1) % total
        };

        Some((beats.get(*index)?.clone(), *index, total))
    }

    pub fn take_result_message_ids(&self, chat_id: ChatId) -> Vec<i32> {
        self.result_message_ids
            .lock()
            .ok()
            .and_then(|mut all| all.remove(&chat_id.0))
            .unwrap_or_default()
    }

    pub fn set_result_message_ids(&self, chat_id: ChatId, ids: Vec<i32>) {
        if let Ok(mut all) = self.result_message_ids.lock() {
            all.insert(chat_id.0, ids);
        }
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
    }

    // ----------------------------------------
    // ACTIVE PREVIEW MESSAGE
    // ----------------------------------------

    pub fn set_preview_message_id(&self, chat_id: ChatId, message_id: i32) {
        if let Ok(mut all) = self.preview_message_ids.lock() {
            all.insert(chat_id.0, message_id);
        }
    }

    pub fn take_preview_message_id(&self, chat_id: ChatId) -> Option<i32> {
        self.preview_message_ids
            .lock()
            .ok()
            .and_then(|mut all| all.remove(&chat_id.0))
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
}
