//! The whole YATV configuration: a TV and the owner's own app names.
//! Deliberately tiny — no music, no accounts, no network services.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::action::normalize_phrase;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TvDevice {
    #[serde(default)]
    pub name: String,
    /// Other ways to say the same TV: «спальная», «наверху».
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub host: String,
    /// App used for «найди фильм X» on this TV (package name).
    #[serde(default)]
    pub search_app: String,
}

/// A phrase YATV owns: «free tv» → open this app, on a named TV or the
/// default one. Replaces the old owned_phrases / app_phrases split.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct OwnedPhrase {
    #[serde(default)]
    pub phrase: String,
    #[serde(default)]
    pub pkg: String,
    /// TV name this phrase belongs to; empty = the first/default TV.
    #[serde(default)]
    pub tv: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Settings {
    #[serde(default)]
    pub tv_host: String,
    #[serde(default)]
    pub tv_name: String,
    #[serde(default)]
    pub tvs: Vec<TvDevice>,
    #[serde(default)]
    pub phrases: Vec<OwnedPhrase>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            tv_host: String::new(),
            tv_name: String::new(),
            tvs: Vec::new(),
            phrases: Vec::new(),
            extra: serde_json::Map::new(),
        }
    }
}

impl Settings {
    pub fn path(dir: &Path) -> std::path::PathBuf {
        dir.join("config.json")
    }

    /// All configured TVs. An old config with only tv_host/tv_name still
    /// works: it becomes one device called by its saved name.
    pub fn tv_list(&self) -> Vec<TvDevice> {
        if self.tvs.is_empty() && !self.tv_host.trim().is_empty() {
            vec![TvDevice {
                name: self.tv_name.clone(),
                aliases: Vec::new(),
                host: self.tv_host.clone(),
                search_app: String::new(),
            }]
        } else {
            self.tvs.clone()
        }
    }

    /// The TV named in a phrase like «на втором этаже». Empty means default.
    pub fn find_tv(&self, spoken: &str) -> Option<TvDevice> {
        let key = |s: &str| {
            let s = s.trim().trim_start_matches("на ").trim();
            // Alice says «на 2 этаже» instead of «на втором этаже».
            let with_words = s
                .replace("1", "первый")
                .replace("2", "второй")
                .replace("3", "третий")
                .replace("4", "четвертый")
                .replace("5", "пятый");
            let merged: String = crate::action::normalize_phrase(&with_words)
                .to_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric())
                .collect();
            // Drop vowels so «второй» and «втором» share a key.
            merged.chars().filter(|c| !"аеёиоуыэюя".contains(*c)).collect::<String>()
        };
        let want = key(spoken);
        if want.is_empty() {
            return None;
        }
        let w = want.chars().take(3).collect::<String>();
        let matches = |candidate: &str| {
            let n = key(candidate);
            let n = n.chars().take(3).collect::<String>();
            !n.is_empty() && !w.is_empty() && (n == w || n.contains(&w) || w.contains(&n))
        };
        self.tv_list().into_iter().find(|t| {
            matches(&t.name) || t.aliases.iter().any(|a| matches(a))
        })
    }

    pub fn load(dir: &Path) -> Self {
        let path = Self::path(dir);
        match crate::fsx::read_string(&path) {
            Some(text) => serde_json::from_str(&text).unwrap_or_default(),
            None => Self::default(),
        }
    }

    /// Merge a partial patch from the panel onto the current config.
    /// Fields not present in the patch keep their saved values: saving just
    /// the app aliases used to wipe tv_host and break the TV connection.
    pub fn from_patch(base: &Self, patch: &serde_json::Value) -> Result<Self, String> {
        let mut base = serde_json::to_value(base).map_err(|e| e.to_string())?;
        let obj = base.as_object_mut().ok_or("expected object")?;
        let patch = patch.as_object().ok_or("expected object")?;
        for (k, v) in patch {
            obj.insert(k.clone(), v.clone());
        }
        serde_json::from_value(base).map_err(|e| e.to_string())
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let path = Self::path(dir);
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }

    /// (phrase, package, TV name) for the voice parser. Empty TV name means
    /// the first configured TV (the default one).
    pub fn phrase_list(&self) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for p in &self.phrases {
            let pkg = p.pkg.trim();
            if pkg.is_empty() || !crate::tv::valid_package(pkg) {
                continue;
            }
            for name in p.phrase.split([',', ';', '|']) {
                let mut n = normalize_phrase(name);
                // Accept both a bare alias («free tv») and a full phrase
                // («включи free tv на втором этаже»): strip the leading verb
                // and the trailing TV hint, keep the TV in `tv` instead.
                for verb in ["включи ", "включить ", "запусти ", "открой ", "поставь "] {
                    if n.starts_with(verb) {
                        n = n[verb.len()..].trim().to_owned();
                        break;
                    }
                }
                if let Some(at) = n.rfind(" на ") {
                    let tail = n[at..].trim().to_owned();
                    if tail.split_whitespace().count() <= 4 {
                        n = n[..at].trim().to_owned();
                    }
                }
                if n.is_empty() || out.iter().any(|(phrase, _, _)| *phrase == n) {
                    continue;
                }
                out.push((n, pkg.to_owned(), p.tv.trim().to_owned()));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_tv_by_its_spoken_name() {
        let mut s = Settings::default();
        s.tvs = vec![
            TvDevice { name: "гостиная".into(), aliases: Vec::new(), host: "192.168.1.188".into(), search_app: String::new() },
            TvDevice { name: "на втором этаже".into(), aliases: vec!["спальная".into(), "наверху".into()], host: "192.168.1.128".into(), search_app: String::new() },
        ];
        assert_eq!(s.find_tv("на втором этаже").unwrap().host, "192.168.1.128");
        assert_eq!(s.find_tv("на 2 этаже").unwrap().host, "192.168.1.128");
        assert_eq!(s.find_tv("на спальне").unwrap().host, "192.168.1.128");
        assert_eq!(s.find_tv("на кухне"), None);
    }

    #[test]
    fn phrases_survive_a_partial_save() {
        let mut s = Settings::default();
        s.tv_host = "192.168.1.188".into();
        s.phrases.push(OwnedPhrase {
            phrase: "фри тв, free tv".into(),
            pkg: "ru.cn.atx".into(),
            tv: String::new(),
        });
        // Saving only the phrases (as the panel does) must not wipe tv_host.
        let patch = serde_json::json!({ "phrases": s.phrases });
        let back = Settings::from_patch(&s, &patch).unwrap();
        assert_eq!(back.tv_host, "192.168.1.188");
        let list = back.phrase_list();
        assert_eq!(list.len(), 2);
        assert!(list.iter().all(|(_, p, _)| p == "ru.cn.atx"));
    }
}
