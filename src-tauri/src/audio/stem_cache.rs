use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct StemCache {
    // Key: (track_id, stem_name)
    // Value: (samples, sample_rate)
    cache: Arc<Mutex<HashMap<(i64, String), (Arc<Vec<f32>>, u32)>>>,
}

impl Default for StemCache {
    fn default() -> Self {
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl StemCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, track_id: i64, stem_name: &str) -> Option<(Arc<Vec<f32>>, u32)> {
        let cache = self.cache.lock().unwrap();
        cache.get(&(track_id, stem_name.to_string())).cloned()
    }

    pub fn insert(
        &self,
        track_id: i64,
        stem_name: String,
        samples: Arc<Vec<f32>>,
        sample_rate: u32,
    ) {
        let mut cache = self.cache.lock().unwrap();
        cache.insert((track_id, stem_name), (samples, sample_rate));
    }

    pub fn remove_track(&self, track_id: i64) {
        let mut cache = self.cache.lock().unwrap();
        cache.retain(|(tid, _), _| *tid != track_id);
    }
}
