//! Decoded-image LRU with a byte budget.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::decode::DecodedImage;

struct Slot {
    img: Arc<DecodedImage>,
    used: u64,
}

pub struct ImageCache {
    map: HashMap<PathBuf, Slot>,
    budget: usize,
    bytes: usize,
    clock: u64,
}

impl ImageCache {
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            map: HashMap::new(),
            budget: budget_bytes,
            bytes: 0,
            clock: 0,
        }
    }

    pub fn get(&mut self, path: &Path) -> Option<Arc<DecodedImage>> {
        self.clock += 1;
        let slot = self.map.get_mut(path)?;
        slot.used = self.clock;
        Some(slot.img.clone())
    }

    #[cfg(test)]
    pub fn contains(&self, path: &Path) -> bool {
        self.map.contains_key(path)
    }

    /// Insert, then evict least-recently-used entries over budget, sparing `keep`.
    pub fn insert(&mut self, path: PathBuf, img: Arc<DecodedImage>, keep: &[&Path]) {
        self.clock += 1;
        self.bytes += img.bytes();
        if let Some(old) = self.map.insert(
            path,
            Slot {
                img,
                used: self.clock,
            },
        ) {
            self.bytes -= old.img.bytes();
        }
        while self.bytes > self.budget {
            let victim = self
                .map
                .iter()
                .filter(|(p, _)| !keep.contains(&p.as_path()))
                .min_by_key(|(_, s)| s.used)
                .map(|(p, _)| p.clone());
            let Some(victim) = victim else { break };
            self.remove(&victim);
        }
    }

    pub fn remove(&mut self, path: &Path) {
        if let Some(old) = self.map.remove(path) {
            self.bytes -= old.img.bytes();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(bytes: usize) -> Arc<DecodedImage> {
        Arc::new(DecodedImage {
            size: [1, 1],
            pixels: vec![0; bytes],
            format: "PNG",
            source_size: [1, 1],
        })
    }

    #[test]
    fn evicts_least_recent_but_spares_kept() {
        let mut c = ImageCache::new(250);
        c.insert("a".into(), img(100), &[]);
        c.insert("b".into(), img(100), &[]);
        c.get(Path::new("a"));
        c.insert("c".into(), img(100), &[]);
        assert!(c.contains(Path::new("a")) && !c.contains(Path::new("b")));
        c.insert("d".into(), img(100), &[Path::new("a")]);
        assert!(c.contains(Path::new("a")) && !c.contains(Path::new("c")));
    }
}
