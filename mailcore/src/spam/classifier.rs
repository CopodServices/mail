//! A small online logistic-regression classifier (FTRL-Proximal over signed,
//! feature-hashed tokens), modeled on the design used by Stalwart's `nlp`
//! crate. It learns from user feedback and scores in constant time.

use crate::spam::config::ClassifierConfig;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use xxhash_rust::xxh3::xxh3_64;

const MAX_GRAD: f32 = 1e4;
const MAX_TOKENS: usize = 10_000;

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "you", "your", "with", "this", "that", "from", "have", "are", "was",
    "not", "but", "all", "any", "can", "has", "her", "his", "our", "out", "who", "will", "would",
    "there", "their", "been", "were", "they", "them", "then", "than", "into", "over", "just",
    "about", "after", "also", "when", "what", "which", "how", "why", "its", "it's",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Classifier {
    hash_mask: u64,
    alpha: f32,
    beta: f32,
    l1: f32,
    l2: f32,
    z: Vec<f32>,
    n: Vec<f32>,
    bias_z: f32,
    bias_n: f32,
    pub trained: u64,
}

impl Classifier {
    pub fn new(config: &ClassifierConfig) -> Self {
        let size = config.feature_hash_size.max(1024).next_power_of_two();
        Self {
            hash_mask: (size - 1) as u64,
            alpha: config.alpha.max(1e-6),
            beta: config.beta.max(1e-6),
            l1: config.l1.max(0.0),
            l2: config.l2.max(0.0),
            z: vec![0.0; size],
            n: vec![0.0; size],
            bias_z: 0.0,
            bias_n: 0.0,
            trained: 0,
        }
    }

    fn feature(&self, token: &str) -> (usize, f32) {
        let hash = xxh3_64(token.as_bytes());
        let idx = (hash & self.hash_mask) as usize;
        let sign = if hash & (1 << 63) == 0 { 1.0 } else { -1.0 };
        (idx, sign)
    }

    fn weight_at(&self, idx: usize) -> f32 {
        let z = self.z[idx];
        if z.abs() <= self.l1 {
            0.0
        } else {
            -(z - self.l1 * z.signum()) / ((self.beta + self.n[idx].sqrt()) / self.alpha + self.l2)
        }
    }

    fn bias(&self) -> f32 {
        let z = self.bias_z;
        if z.abs() <= self.l1 {
            0.0
        } else {
            -(z - self.l1 * z.signum()) / ((self.beta + self.bias_n.sqrt()) / self.alpha + self.l2)
        }
    }

    fn unique(&self, tokens: &[String]) -> Vec<(usize, f32)> {
        let mut seen = HashSet::new();
        let mut features = Vec::new();
        for token in tokens {
            let (idx, sign) = self.feature(token);
            if seen.insert(idx) {
                features.push((idx, sign));
            }
        }
        features
    }

    /// The probability the message is spam, in `[0, 1]`.
    pub fn score(&self, tokens: &[String]) -> f32 {
        let mut z = self.bias();
        for (idx, sign) in self.unique(tokens) {
            z += self.weight_at(idx) * sign;
        }
        sigmoid(z)
    }

    /// One online FTRL-Proximal step toward the given label.
    pub fn learn(&mut self, tokens: &[String], spam: bool) {
        let features = self.unique(tokens);
        let mut z = self.bias();
        for (idx, sign) in &features {
            z += self.weight_at(*idx) * sign;
        }
        let y = if spam { 1.0 } else { 0.0 };
        let grad = (sigmoid(z) - y).clamp(-MAX_GRAD, MAX_GRAD);

        for (idx, sign) in features {
            let gi = grad * sign;
            let sigma = ((self.n[idx] + gi * gi).sqrt() - self.n[idx].sqrt()) / self.alpha;
            let weight = self.weight_at(idx);
            self.z[idx] += gi - sigma * weight;
            self.n[idx] += gi * gi;
        }

        let sigma = ((self.bias_n + grad * grad).sqrt() - self.bias_n.sqrt()) / self.alpha;
        self.bias_z += grad - sigma * self.bias();
        self.bias_n += grad * grad;
        self.trained += 1;
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

fn sigmoid(z: f32) -> f32 {
    let z = z.clamp(-35.0, 35.0);
    if z >= 0.0 {
        1.0 / (1.0 + (-z).exp())
    } else {
        let exp = z.exp();
        exp / (1.0 + exp)
    }
}

/// The per-scope model set: one model per mailbox plus a global model.
#[derive(Clone)]
pub struct ClassifierSet {
    inner: Arc<RwLock<HashMap<String, Classifier>>>,
    config: ClassifierConfig,
}

impl ClassifierSet {
    pub fn new(config: ClassifierConfig) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            config,
        }
    }

    pub fn min_samples(&self) -> u64 {
        self.config.min_samples
    }

    pub fn trained(&self, scope: &str) -> u64 {
        self.inner
            .read()
            .ok()
            .and_then(|models| models.get(scope).map(|model| model.trained))
            .unwrap_or(0)
    }

    /// The spam probability once the scope has learned enough, else `None`.
    pub fn predict(&self, scope: &str, tokens: &[String]) -> Option<f32> {
        if self.trained(scope) < self.config.min_samples {
            return None;
        }
        self.inner
            .read()
            .ok()
            .and_then(|models| models.get(scope).map(|model| model.score(tokens)))
    }

    pub fn learn(&self, scope: &str, tokens: &[String], spam: bool) {
        if let Ok(mut models) = self.inner.write() {
            let model = models
                .entry(scope.to_string())
                .or_insert_with(|| Classifier::new(&self.config));
            model.learn(tokens, spam);
        }
    }

    /// A fresh, untrained model with the configured parameters.
    pub fn fresh(&self) -> Classifier {
        Classifier::new(&self.config)
    }

    /// Swap in a rebuilt model for a scope.
    pub fn replace(&self, scope: &str, model: Classifier) {
        if let Ok(mut models) = self.inner.write() {
            models.insert(scope.to_string(), model);
        }
    }

    pub fn scopes(&self) -> Vec<String> {
        self.inner
            .read()
            .map(|models| models.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn export(&self, scope: &str) -> Option<Vec<u8>> {
        self.inner
            .read()
            .ok()
            .and_then(|models| models.get(scope).map(Classifier::to_bytes))
    }

    pub fn import(&self, scope: &str, bytes: &[u8]) -> bool {
        let Some(model) = Classifier::from_bytes(bytes) else {
            return false;
        };
        if let Ok(mut models) = self.inner.write() {
            models.insert(scope.to_string(), model);
        }
        true
    }
}

/// Lowercase the text, split on anything that is not alphanumeric, drop
/// stopwords and very short/long tokens, and cap the total.
pub fn tokenize<'a>(parts: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<String> {
    let stop: HashSet<&str> = STOPWORDS.iter().copied().collect();
    let mut out = Vec::new();
    for (prefix, text) in parts {
        for raw in text.split(|c: char| !c.is_alphanumeric()) {
            if raw.len() < 2 || raw.len() > 40 {
                continue;
            }
            let lowered = raw.to_ascii_lowercase();
            if stop.contains(lowered.as_str()) {
                continue;
            }
            out.push(format!("{prefix}:{lowered}"));
            if out.len() >= MAX_TOKENS {
                return out;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learns_to_separate_spam_from_ham() {
        let config = ClassifierConfig {
            min_samples: 1,
            ..ClassifierConfig::default()
        };
        let set = ClassifierSet::new(config);
        let spam = tokenize([
            ("s", "cheap viagra pills now"),
            ("b", "buy cheap meds online"),
        ]);
        let ham = tokenize([
            ("s", "meeting notes"),
            ("b", "here are the notes from today"),
        ]);
        for _ in 0..40 {
            set.learn("global", &spam, true);
            set.learn("global", &ham, false);
        }
        let spam_score = set.predict("global", &spam).unwrap();
        let ham_score = set.predict("global", &ham).unwrap();
        assert!(spam_score > 0.5, "spam scored {spam_score}");
        assert!(ham_score < 0.5, "ham scored {ham_score}");
    }

    #[test]
    fn round_trips_through_bytes() {
        let set = ClassifierSet::new(ClassifierConfig {
            min_samples: 1,
            ..ClassifierConfig::default()
        });
        let tokens = tokenize([("b", "hello world")]);
        set.learn("m1", &tokens, true);
        let bytes = set.export("m1").unwrap();
        let restored = ClassifierSet::new(ClassifierConfig {
            min_samples: 1,
            ..ClassifierConfig::default()
        });
        assert!(restored.import("m1", &bytes));
        assert_eq!(restored.trained("m1"), 1);
    }

    #[test]
    fn predict_is_gated_by_the_minimum_sample_count() {
        let set = ClassifierSet::new(ClassifierConfig {
            min_samples: 3,
            ..ClassifierConfig::default()
        });
        let tokens = tokenize([("b", "hello world")]);
        assert!(set.predict("global", &tokens).is_none());
        set.learn("global", &tokens, true);
        set.learn("global", &tokens, true);
        assert!(set.predict("global", &tokens).is_none());
        set.learn("global", &tokens, true);
        assert!(set.predict("global", &tokens).is_some());
    }
}
