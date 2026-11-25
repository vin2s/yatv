//! Full configuration transfer: export the speaker's TVs, phrases, planner,
//! brief, Google and update settings plus tasks and caches into one JSON, and
//! import it on another speaker (so a new device starts with the same setup).

use std::path::Path;

use serde_json::{json, Value};

use crate::{fsx, settings::Settings};

const FILES: [&str; 3] = ["planner.json", "google-cache.json", "brief-day"];

/// Everything needed to clone this speaker's setup to another one.
pub fn export(dir: &Path) -> Value {
    let s = Settings::load(dir);
    let mut files = serde_json::Map::new();
    for name in FILES {
        if let Some(text) = fsx::read_string(&dir.join(name)) {
            files.insert(name.to_owned(), Value::String(text));
        }
    }
    json!({
        "format": "yatv-config",
        "version": 1,
        "exported": crate::planner::now(),
        "from_version": crate::VERSION,
        "settings": s,
        "files": files,
    })
}

/// Restore an exported configuration. Unknown fields are kept; a damaged or
/// foreign JSON is refused. Settings are saved before the in-memory state is
/// re-applied by the caller (main.rs).
pub fn import(dir: &Path, body: &str) -> Result<Value, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| format!("не JSON: {e}"))?;
    if v.get("format").and_then(Value::as_str) != Some("yatv-config") {
        return Err("это не конфиг yatv (нет format=yatv-config)".into());
    }
    let settings = v.get("settings").ok_or("в файле нет настроек")?;
    let parsed: Settings = serde_json::from_value(settings.clone()).map_err(|e| format!("настройки не прочитались: {e}"))?;
    parsed.save(dir).map_err(|e| format!("не сохранилось: {e}"))?;
    if let Some(files) = v.get("files").and_then(Value::as_object) {
        for (name, content) in files {
            // Only the known files, never an arbitrary path from the archive.
            if !FILES.contains(&name.as_str()) {
                continue;
            }
            let text = content.as_str().unwrap_or("");
            let _ = std::fs::write(dir.join(name), text);
        }
    }
    Ok(json!({
        "ok": true,
        "tvs": parsed.tvs.len(),
        "phrases": parsed.phrases.len(),
        "files": v.get("files").and_then(Value::as_object).map(|m| m.len()).unwrap_or(0),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_has_settings_and_format() {
        let dir = std::env::temp_dir().join(format!("yatv-transfer-{}", std::process::id()));
        let _ = std::fs::create_dir(&dir);
        let s = Settings::default();
        s.save(&dir).unwrap();
        let v = export(&dir);
        assert_eq!(v["format"], "yatv-config");
        assert!(v["settings"].is_object());
        let body = v.to_string();
        let back = import(&dir, &body).unwrap();
        assert_eq!(back["ok"], true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_rejects_foreign_json() {
        let dir = std::env::temp_dir().join(format!("yatv-transfer-bad-{}", std::process::id()));
        let _ = std::fs::create_dir(&dir);
        assert!(import(&dir, "{}").is_err());
        assert!(import(&dir, "not json").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
