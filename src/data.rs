use std::{
    collections::HashMap,
    env, fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};

use crate::models::Beat;

const STATE_VERSION: u32 = 1;
const MAX_FAVORITES_PER_CHAT: usize = 200;
const MAX_TELEGRAM_FILE_IDS: usize = 1000;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StoredCarousel {
    #[serde(default)]
    pub beats: Vec<Beat>,

    #[serde(default)]
    pub index: usize,

    #[serde(default)]
    pub message_id: Option<i32>,

    #[serde(default)]
    pub is_photo: bool,

    #[serde(default)]
    pub preview_message_id: Option<i32>,

    #[serde(default)]
    pub favorites_view: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistentData {
    #[serde(default)]
    version: u32,

    #[serde(default)]
    telegram_file_ids: HashMap<String, String>,

    #[serde(default)]
    favorites: HashMap<i64, Vec<Beat>>,

    #[serde(default)]
    carousels: HashMap<i64, StoredCarousel>,
}

#[derive(Clone)]
pub struct PersistentStore {
    inner: Arc<Mutex<PersistentData>>,
    path: Arc<PathBuf>,
}

impl PersistentStore {
    pub fn from_env() -> Self {
        let path = env::var("EPSILONE_STATE_FILE")
            .unwrap_or_else(|_| "data/epsilone-telegram-state.json".to_string());

        Self::open(PathBuf::from(path))
    }

    pub fn open(path: PathBuf) -> Self {
        let backup_path = path.with_extension("bak");

        // Recover from a previous interrupted file replacement.
        if !path.exists() && backup_path.exists() {
            let _ = fs::rename(&backup_path, &path);
        }

        let mut data = if path.exists() {
            match fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<PersistentData>(&bytes).ok())
            {
                Some(data) => data,
                None => {
                    log::error!("Persistent state file is invalid. Starting with an empty state.");

                    PersistentData::default()
                }
            }
        } else {
            PersistentData::default()
        };

        if data.version != STATE_VERSION {
            log::info!(
                "Initializing persistent state schema version {}",
                STATE_VERSION
            );
            data.version = STATE_VERSION;
        }

        Self {
            inner: Arc::new(Mutex::new(data)),
            path: Arc::new(path),
        }
    }

    fn save_locked(&self, data: &PersistentData) {
        let result = (|| -> std::io::Result<()> {
            if let Some(parent) = self.path.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent)?;
                }
            }

            let json = serde_json::to_vec_pretty(data)?;

            let temp_path = self.path.with_extension("tmp");
            let backup_path = self.path.with_extension("bak");

            fs::write(&temp_path, json)?;

            if self.path.exists() {
                if backup_path.exists() {
                    fs::remove_file(&backup_path)?;
                }

                fs::rename(&*self.path, &backup_path)?;

                if let Err(error) = fs::rename(&temp_path, &*self.path) {
                    let _ = fs::rename(&backup_path, &*self.path);
                    return Err(error);
                }

                let _ = fs::remove_file(backup_path);
            } else {
                fs::rename(&temp_path, &*self.path)?;
            }

            Ok(())
        })();

        if let Err(error) = result {
            log::error!("Failed to persist Telegram bot state: {}", error);
        }
    }

    fn update<R>(&self, operation: impl FnOnce(&mut PersistentData) -> R) -> R {
        let mut data = self.inner.lock().expect("Persistent store lock poisoned");

        let result = operation(&mut data);

        data.version = STATE_VERSION;

        self.save_locked(&data);

        result
    }

    // ----------------------------------------
    // TELEGRAM FILE ID CACHE
    // ----------------------------------------

    fn file_cache_key(beat_id: &str, preview_url: &str) -> String {
        format!("{}|{}", beat_id, preview_url)
    }

    pub fn get_telegram_file_id(&self, beat_id: &str, preview_url: &str) -> Option<String> {
        let key = Self::file_cache_key(beat_id, preview_url);

        self.inner.lock().ok()?.telegram_file_ids.get(&key).cloned()
    }

    pub fn set_telegram_file_id(&self, beat_id: &str, preview_url: &str, file_id: String) {
        let key = Self::file_cache_key(beat_id, preview_url);

        self.update(|data| {
            if data.telegram_file_ids.len() >= MAX_TELEGRAM_FILE_IDS
                && !data.telegram_file_ids.contains_key(&key)
            {
                if let Some(old_key) = data.telegram_file_ids.keys().next().cloned() {
                    data.telegram_file_ids.remove(&old_key);
                }
            }

            data.telegram_file_ids.insert(key, file_id);
        });
    }

    // ----------------------------------------
    // FAVORITES
    // ----------------------------------------

    pub fn favorites(&self, chat_id: i64) -> Vec<Beat> {
        self.inner
            .lock()
            .ok()
            .and_then(|data| data.favorites.get(&chat_id).cloned())
            .unwrap_or_default()
    }

    pub fn is_favorite(&self, chat_id: i64, beat_id: &str) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|data| {
                data.favorites
                    .get(&chat_id)
                    .map(|beats| beats.iter().any(|beat| beat.id == beat_id))
            })
            .unwrap_or(false)
    }

    /// Returns true when the beat is now saved.
    /// Returns false when it was removed.
    pub fn toggle_favorite(&self, chat_id: i64, beat: Beat) -> bool {
        self.update(|data| {
            let favorites = data.favorites.entry(chat_id).or_default();

            if let Some(position) = favorites.iter().position(|saved| saved.id == beat.id) {
                favorites.remove(position);
                false
            } else {
                if favorites.len() >= MAX_FAVORITES_PER_CHAT {
                    favorites.remove(0);
                }

                favorites.push(beat);
                true
            }
        })
    }

    // ----------------------------------------
    // PERSISTENT CAROUSEL
    // ----------------------------------------

    pub fn carousels(&self) -> HashMap<i64, StoredCarousel> {
        self.inner
            .lock()
            .map(|data| data.carousels.clone())
            .unwrap_or_default()
    }

    pub fn save_carousel(&self, chat_id: i64, beats: Vec<Beat>, favorites_view: bool) {
        self.update(|data| {
            data.carousels.insert(
                chat_id,
                StoredCarousel {
                    beats,
                    index: 0,
                    message_id: None,
                    is_photo: false,
                    preview_message_id: None,
                    favorites_view,
                },
            );
        });
    }

    pub fn update_carousel_index(&self, chat_id: i64, index: usize) {
        self.update(|data| {
            if let Some(carousel) = data.carousels.get_mut(&chat_id) {
                carousel.index = index;
            }
        });
    }

    pub fn set_carousel_message_id(&self, chat_id: i64, message_id: Option<i32>) {
        self.update(|data| {
            if let Some(carousel) = data.carousels.get_mut(&chat_id) {
                carousel.message_id = message_id;
            }
        });
    }

    pub fn set_carousel_photo(&self, chat_id: i64, is_photo: bool) {
        self.update(|data| {
            if let Some(carousel) = data.carousels.get_mut(&chat_id) {
                carousel.is_photo = is_photo;
            }
        });
    }

    pub fn set_preview_message_id(&self, chat_id: i64, message_id: Option<i32>) {
        self.update(|data| {
            if let Some(carousel) = data.carousels.get_mut(&chat_id) {
                carousel.preview_message_id = message_id;
            }
        });
    }

    pub fn is_favorites_view(&self, chat_id: i64) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|data| {
                data.carousels
                    .get(&chat_id)
                    .map(|carousel| carousel.favorites_view)
            })
            .unwrap_or(false)
    }

    pub fn remove_carousel(&self, chat_id: i64) {
        self.update(|data| {
            data.carousels.remove(&chat_id);
        });
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct PendingProducerLink {
    chat_id: i64,
    expires_at: u64,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct ProducerNotificationData {
    #[serde(default)]
    pending_links: HashMap<String, PendingProducerLink>,

    #[serde(default)]
    producer_chats: HashMap<String, i64>,

    #[serde(default)]
    processed_events: HashMap<String, u64>,
}

#[derive(Clone)]
pub struct ProducerNotificationStore {
    inner: Arc<Mutex<ProducerNotificationData>>,
    path: Arc<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkClaimError {
    InvalidProducerId,
    InvalidOrExpiredCode,
    ExpiredCode,
    ProducerAlreadyLinked,
    ChatAlreadyLinked,
}

impl ProducerNotificationStore {
    const LINK_TTL_SECONDS: u64 = 600;
    const EVENT_RETENTION_SECONDS: u64 = 90 * 24 * 60 * 60;
    const MAX_PROCESSED_EVENTS: usize = 20_000;

    pub fn from_env() -> Self {
        let path = env::var("TELEGRAM_NOTIFICATION_STATE_FILE")
            .unwrap_or_else(|_| "data/producer-notifications.json".to_string());

        Self::open(PathBuf::from(path))
    }

    pub fn open(path: PathBuf) -> Self {
        let mut data = if path.exists() {
            fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<ProducerNotificationData>(&bytes).ok())
                .unwrap_or_else(|| {
                    log::error!("Producer notification state is invalid. Starting empty.");

                    ProducerNotificationData::default()
                })
        } else {
            ProducerNotificationData::default()
        };

        let now = unix_timestamp();

        data.pending_links.retain(|_, link| link.expires_at > now);

        data.processed_events
            .retain(|_, timestamp| now.saturating_sub(*timestamp) <= Self::EVENT_RETENTION_SECONDS);

        Self {
            inner: Arc::new(Mutex::new(data)),
            path: Arc::new(path),
        }
    }

    fn persist_locked(&self, data: &ProducerNotificationData) {
        let result = (|| -> std::io::Result<()> {
            if let Some(parent) = self.path.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent)?;
                }
            }

            let json = serde_json::to_vec_pretty(data)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;

            let temp = self.path.with_extension("tmp");
            let backup = self.path.with_extension("bak");

            fs::write(&temp, json)?;

            if self.path.exists() {
                if backup.exists() {
                    fs::remove_file(&backup)?;
                }

                fs::rename(&*self.path, &backup)?;

                if let Err(error) = fs::rename(&temp, &*self.path) {
                    let _ = fs::rename(&backup, &*self.path);
                    return Err(error);
                }

                let _ = fs::remove_file(backup);
            } else {
                fs::rename(temp, &*self.path)?;
            }

            Ok(())
        })();

        if let Err(error) = result {
            log::error!("Failed to persist producer notifications: {}", error);
        }
    }

    fn update<R>(&self, operation: impl FnOnce(&mut ProducerNotificationData) -> R) -> R {
        let mut data = self
            .inner
            .lock()
            .expect("Producer notification lock poisoned");

        let result = operation(&mut data);

        self.persist_locked(&data);

        result
    }

    // ----------------------------------------
    // LINK CODE GENERATION
    // ----------------------------------------

    pub fn issue_link_code(&self, chat_id: i64) -> String {
        use rand::RngCore;

        const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

        let now = unix_timestamp();

        self.update(|data| {
            data.pending_links
                .retain(|_, pending| pending.chat_id != chat_id && pending.expires_at > now);

            let mut rng = rand::rngs::OsRng;

            let code = loop {
                let mut bytes = [0u8; 10];
                rng.fill_bytes(&mut bytes);

                let generated: String = bytes
                    .iter()
                    .map(|byte| ALPHABET[*byte as usize % ALPHABET.len()] as char)
                    .collect();

                let hash = hash_link_code(&generated);

                if !data.pending_links.contains_key(&hash) {
                    break generated;
                }
            };

            let hash = hash_link_code(&code);

            data.pending_links.insert(
                hash,
                PendingProducerLink {
                    chat_id,
                    expires_at: now + Self::LINK_TTL_SECONDS,
                },
            );

            code
        })
    }

    // ----------------------------------------
    // CLAIM PRODUCER LINK
    // ----------------------------------------

    pub fn claim_link_code(&self, producer_id: &str, code: &str) -> Result<(), LinkClaimError> {
        let producer_id = producer_id.trim();

        if producer_id.is_empty()
            || producer_id.chars().count() > 128
            || producer_id.chars().any(|c| c.is_control())
        {
            return Err(LinkClaimError::InvalidProducerId);
        }

        if code.len() != 10 {
            return Err(LinkClaimError::InvalidOrExpiredCode);
        }

        let code_hash = hash_link_code(code);
        let now = unix_timestamp();

        self.update(|data| {
            let Some(pending) = data.pending_links.get(&code_hash).cloned() else {
                return Err(LinkClaimError::InvalidOrExpiredCode);
            };

            if pending.expires_at <= now {
                data.pending_links.remove(&code_hash);
                return Err(LinkClaimError::ExpiredCode);
            }

            if data.producer_chats.contains_key(producer_id) {
                return Err(LinkClaimError::ProducerAlreadyLinked);
            }

            if data
                .producer_chats
                .values()
                .any(|chat_id| *chat_id == pending.chat_id)
            {
                return Err(LinkClaimError::ChatAlreadyLinked);
            }

            data.pending_links.remove(&code_hash);

            // Remove any older unused tokens for this chat.
            data.pending_links
                .retain(|_, link| link.chat_id != pending.chat_id);

            data.producer_chats
                .insert(producer_id.to_string(), pending.chat_id);

            Ok(())
        })
    }

    // ----------------------------------------
    // PRODUCER CONNECTION LOOKUP
    // ----------------------------------------

    pub fn producer_id_for_chat(&self, chat_id: i64) -> Option<String> {
        self.inner
            .lock()
            .ok()?
            .producer_chats
            .iter()
            .find_map(|(producer_id, stored_chat_id)| {
                (*stored_chat_id == chat_id).then(|| producer_id.clone())
            })
    }

    pub fn chat_id_for_producer(&self, producer_id: &str) -> Option<i64> {
        self.inner
            .lock()
            .ok()?
            .producer_chats
            .get(producer_id)
            .copied()
    }

    pub fn unlink_producer(&self, producer_id: &str) -> bool {
        self.update(|data| data.producer_chats.remove(producer_id).is_some())
    }

    pub fn unlink_chat(&self, chat_id: i64) -> Option<String> {
        self.update(|data| {
            let producer_id =
                data.producer_chats
                    .iter()
                    .find_map(|(producer_id, stored_chat_id)| {
                        (*stored_chat_id == chat_id).then(|| producer_id.clone())
                    })?;

            data.producer_chats.remove(&producer_id);

            Some(producer_id)
        })
    }

    // ----------------------------------------
    // EVENT IDEMPOTENCY
    // ----------------------------------------

    pub fn is_event_processed(&self, event_id: &str) -> bool {
        self.inner
            .lock()
            .map(|data| data.processed_events.contains_key(event_id))
            .unwrap_or(false)
    }

    pub fn mark_event_processed(&self, event_id: &str) {
        let now = unix_timestamp();

        self.update(|data| {
            data.processed_events.insert(event_id.to_string(), now);

            data.processed_events.retain(|_, timestamp| {
                now.saturating_sub(*timestamp) <= Self::EVENT_RETENTION_SECONDS
            });

            if data.processed_events.len() > Self::MAX_PROCESSED_EVENTS {
                let mut oldest: Vec<(String, u64)> = data
                    .processed_events
                    .iter()
                    .map(|(id, timestamp)| (id.clone(), *timestamp))
                    .collect();

                oldest.sort_by_key(|(_, timestamp)| *timestamp);

                let remove_count = data.processed_events.len() - Self::MAX_PROCESSED_EVENTS;

                for (id, _) in oldest.into_iter().take(remove_count) {
                    data.processed_events.remove(&id);
                }
            }
        });
    }
}

fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn hash_link_code(code: &str) -> String {
    use sha2::{Digest, Sha256};

    let normalized = code.trim().to_ascii_uppercase();
    let digest = Sha256::digest(normalized.as_bytes());

    digest.iter().map(|byte| format!("{:02x}", byte)).collect()
}

// ----------------------------------------
// AUTOMATED TESTS
// ----------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        std::env::temp_dir().join(format!("epsilone-telegram-test-{}.json", nonce))
    }

    fn test_beat(id: &str) -> Beat {
        Beat {
            id: id.to_string(),
            title: Some("Test Beat".to_string()),
            producer_username: Some("producer".to_string()),
            producer_display_name: Some("Producer".to_string()),
            cover_url: None,
            preview_url: None,
            bpm: None,
            key: None,
            genre: Some("Trap".to_string()),
            starting_price: None,
            currency: None,
        }
    }

    #[test]
    fn favorite_survives_restart() {
        let path = test_path();

        {
            let store = PersistentStore::open(path.clone());

            assert!(store.toggle_favorite(12345, test_beat("beat-1")));
            assert!(store.is_favorite(12345, "beat-1"));
        }

        {
            let store = PersistentStore::open(path.clone());

            assert!(store.is_favorite(12345, "beat-1"));
            assert_eq!(store.favorites(12345).len(), 1);
        }

        let _ = fs::remove_file(path);
    }

    #[test]
    fn favorite_can_be_removed() {
        let store = PersistentStore::open(test_path());

        assert!(store.toggle_favorite(12345, test_beat("beat-1")));
        assert!(!store.toggle_favorite(12345, test_beat("beat-1")));

        assert!(!store.is_favorite(12345, "beat-1"));
    }

    #[test]
    fn telegram_file_id_survives_restart() {
        let path = test_path();

        {
            let store = PersistentStore::open(path.clone());

            store.set_telegram_file_id(
                "beat-1",
                "https://example.com/preview.mp3",
                "telegram-file-123".to_string(),
            );
        }

        {
            let store = PersistentStore::open(path.clone());

            assert_eq!(
                store.get_telegram_file_id("beat-1", "https://example.com/preview.mp3"),
                Some("telegram-file-123".to_string())
            );
        }

        let _ = fs::remove_file(path);
    }

    #[test]
    fn carousel_survives_restart() {
        let path = test_path();

        {
            let store = PersistentStore::open(path.clone());

            store.save_carousel(12345, vec![test_beat("beat-1"), test_beat("beat-2")], true);

            store.set_carousel_message_id(12345, Some(777));
            store.update_carousel_index(12345, 1);
            store.set_carousel_photo(12345, true);
        }

        {
            let store = PersistentStore::open(path.clone());

            let carousel = store.carousels().remove(&12345).unwrap();

            assert_eq!(carousel.index, 1);
            assert_eq!(carousel.message_id, Some(777));
            assert!(carousel.is_photo);
            assert!(carousel.favorites_view);
            assert_eq!(carousel.beats.len(), 2);
        }

        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod producer_notification_store_tests {
    use super::*;

    fn test_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "epsilone-producer-notifications-{}.json",
            unix_timestamp() + rand::random::<u64>()
        ))
    }

    #[test]
    fn producer_link_code_is_one_time() {
        let path = test_path();
        let store = ProducerNotificationStore::open(path.clone());

        let code = store.issue_link_code(12345);

        assert!(store.claim_link_code("producer-1", &code).is_ok());

        assert_eq!(store.chat_id_for_producer("producer-1"), Some(12345));

        assert_eq!(
            store.claim_link_code("producer-2", &code),
            Err(LinkClaimError::InvalidOrExpiredCode)
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn one_chat_cannot_link_multiple_producers() {
        let store = ProducerNotificationStore::open(test_path());

        let first_code = store.issue_link_code(12345);

        assert!(store.claim_link_code("producer-1", &first_code).is_ok());

        let second_code = store.issue_link_code(12345);

        assert_eq!(
            store.claim_link_code("producer-2", &second_code),
            Err(LinkClaimError::ChatAlreadyLinked)
        );
    }

    #[test]
    fn producer_can_unlink() {
        let store = ProducerNotificationStore::open(test_path());

        let code = store.issue_link_code(12345);

        assert!(store.claim_link_code("producer-1", &code).is_ok());

        assert_eq!(store.unlink_chat(12345), Some("producer-1".to_string()));

        assert_eq!(store.chat_id_for_producer("producer-1"), None);
    }

    #[test]
    fn processed_event_survives_restart() {
        let path = test_path();

        {
            let store = ProducerNotificationStore::open(path.clone());

            store.mark_event_processed("producer-1:order-123");

            assert!(store.is_event_processed("producer-1:order-123"));
        }

        {
            let store = ProducerNotificationStore::open(path.clone());

            assert!(store.is_event_processed("producer-1:order-123"));
        }

        let _ = fs::remove_file(path);
    }
}
