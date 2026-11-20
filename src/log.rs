//! A small on-disk journal of phrases and errors (the last 100 lines).
//! Stored in the state dir so it survives a restart of the daemon.

use std::path::Path;

const MAX_LINES: usize = 200;

pub fn path(dir: &Path) -> std::path::PathBuf {
    dir.join("journal.jsonl")
}

pub fn add(dir: &Path, kind: &str, text: &str) {
    let line = format!(
        "{} {} {}\n",
        chrono_stamp(),
        kind,
        text.replace('\n', " ").trim()
    );
    let file = path(dir);
    let mut lines: Vec<String> = crate::fsx::read_string(&file)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect();
    lines.push(line.trim_end().to_owned());
    while lines.len() > MAX_LINES {
        lines.remove(0);
    }
    let mut out = lines.join("\n");
    out.push('\n');
    let _ = std::fs::write(&file, out);
}

/// Drop all journal entries (the panel's «Очистить» button).
pub fn clear(dir: &Path) {
    let _ = std::fs::write(path(dir), "");
}

/// The last 100 entries, newest first, for the web panel.
pub fn latest(dir: &Path) -> Vec<serde_json::Value> {
    let text = crate::fsx::read_string(&path(dir)).unwrap_or_default();
    text.lines()
        .rev()
        .map(|line| {
            let mut parts = line.splitn(3, ' ');
            serde_json::json!({
                "time": parts.next().unwrap_or(""),
                "kind": parts.next().unwrap_or(""),
                "text": parts.next().unwrap_or(""),
            })
        })
        .take(MAX_LINES)
        .collect()
}

fn chrono_stamp() -> String {
    // UNIX seconds: the speaker is in UTC, but the browser knows the owner's
    // timezone (Asia/Yekaterinburg) and renders local time itself.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_the_last_200_lines() {
        let dir = std::env::temp_dir().join(format!("yatv-log-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..220 {
            add(&dir, "phrase", &format!("line {i}"));
        }
        let lines = latest(&dir);
        assert_eq!(lines.len(), MAX_LINES);
        assert_eq!(lines[0]["text"], "line 219");
        assert_eq!(lines[199]["text"], "line 20");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
