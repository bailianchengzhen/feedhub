//! `ApiImageIndex`：对外接口 `/image` 的图片清单备忘录（访问序 LRU，并发线程安全）。

use std::sync::Mutex;
use crate::appapi::{ContentImages, DetailImages};

struct Lru<K: PartialEq + Clone, V: Clone> {
    capacity: usize,
    entries: Vec<(K, V)>,
}

impl<K: PartialEq + Clone, V: Clone> Lru<K, V> {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: Vec::with_capacity(capacity),
        }
    }

    fn get(&mut self, key: &K) -> Option<V> {
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == key) {
            let entry = self.entries.remove(pos);
            self.entries.push((entry.0, entry.1.clone()));
            Some(entry.1)
        } else {
            None
        }
    }

    fn put(&mut self, key: K, value: V) {
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == &key) {
            self.entries.remove(pos);
        } else if self.entries.len() >= self.capacity && !self.entries.is_empty() {
            self.entries.remove(0);
        }
        self.entries.push((key, value));
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

pub struct ApiImageIndex {
    details: Mutex<Lru<String, DetailImages>>,
    comments: Mutex<Lru<String, ContentImages>>,
}

impl ApiImageIndex {
    pub fn new(detail_capacity: usize, comment_capacity: usize) -> Self {
        Self {
            details: Mutex::new(Lru::new(detail_capacity)),
            comments: Mutex::new(Lru::new(comment_capacity)),
        }
    }

    pub fn default() -> Self {
        Self::new(16, 64)
    }

    pub fn detail(&self, platform: &str, id: &str) -> Option<DetailImages> {
        let key = format!("{platform}\0{id}");
        let mut map = self.details.lock().unwrap();
        map.get(&key)
    }

    pub fn put_detail(&self, platform: &str, id: &str, images: DetailImages) {
        let key = format!("{platform}\0{id}");
        let mut map = self.details.lock().unwrap();
        map.put(key, images);
    }

    pub fn comment(&self, platform: &str, id: &str, comment_id: &str) -> Option<ContentImages> {
        let key = format!("{platform}\0{id}\0{comment_id}");
        let mut map = self.comments.lock().unwrap();
        map.get(&key)
    }

    pub fn put_comment(&self, platform: &str, id: &str, comment_id: &str, images: ContentImages) {
        if images.count() == 0 {
            return;
        }
        let key = format!("{platform}\0{id}\0{comment_id}");
        let mut map = self.comments.lock().unwrap();
        map.put(key, images);
    }

    pub fn size(&self) -> (usize, usize) {
        let d = self.details.lock().unwrap().len();
        let c = self.comments.lock().unwrap().len();
        (d, c)
    }
}

