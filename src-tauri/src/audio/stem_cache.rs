use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct StemCache {
    // Key: (track_id, stem_name)
    // Value: (samples, sample_rate)
    cache: Arc<Mutex<HashMap<(String, String), (Arc<Vec<f32>>, u32)>>>,
    /// Per-key loading locks to prevent thundering herd when multiple tasks
    /// try to load the same stem concurrently.
    loading: Arc<Mutex<HashMap<(String, String), Arc<tokio::sync::Mutex<()>>>>>,
}

impl Default for StemCache {
    fn default() -> Self {
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
            loading: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl StemCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, track_id: &str, stem_name: &str) -> Option<(Arc<Vec<f32>>, u32)> {
        let cache = self.cache.lock().unwrap();
        cache
            .get(&(track_id.to_string(), stem_name.to_string()))
            .cloned()
    }

    pub fn insert(
        &self,
        track_id: &str,
        stem_name: String,
        samples: Arc<Vec<f32>>,
        sample_rate: u32,
    ) {
        let mut cache = self.cache.lock().unwrap();
        cache.insert((track_id.to_string(), stem_name), (samples, sample_rate));
    }

    /// Get a stem from cache, or load it using the provided closure.
    /// Only one task will execute the loader for a given (track_id, stem_name);
    /// concurrent callers wait and then read from cache.
    pub async fn get_or_load<F>(
        &self,
        track_id: &str,
        stem_name: &str,
        loader: F,
    ) -> Result<(Arc<Vec<f32>>, u32), String>
    where
        F: FnOnce() -> Result<(Arc<Vec<f32>>, u32), String>,
    {
        // Fast path: already cached
        if let Some(cached) = self.get(track_id, stem_name) {
            return Ok(cached);
        }

        // Get or create the loading lock for this key
        let lock = {
            let mut loading = self.loading.lock().unwrap();
            loading
                .entry((track_id.to_string(), stem_name.to_string()))
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };

        // Only one task proceeds past this point per key
        let _guard = lock.lock().await;

        // Check cache again — another task may have loaded it while we waited
        if let Some(cached) = self.get(track_id, stem_name) {
            return Ok(cached);
        }

        // We're the loader — execute the closure.
        // Always clean up the loading lock, even on error.
        let key = (track_id.to_string(), stem_name.to_string());
        let result = loader();
        {
            let mut loading = self.loading.lock().unwrap();
            loading.remove(&key);
        }
        let (samples, sample_rate) = result?;
        self.insert(
            track_id,
            stem_name.to_string(),
            samples.clone(),
            sample_rate,
        );

        Ok((samples, sample_rate))
    }

    pub fn remove_track(&self, track_id: &str) {
        let mut cache = self.cache.lock().unwrap();
        cache.retain(|(tid, _), _| tid != track_id);
    }
}
