//! Updates from GitHub Releases: the panel shows a newer version, and one
//! button (or auto mode) downloads the archive and installs it in its own
//! session so it can stop and restart this very process.

use std::{
    path::Path,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

use serde_json::Value;

const REPO: &str = "vin2s/yatv";
const RELEASES: &str = "https://api.github.com/repos/vin2s/yatv/releases/latest";
const LATEST_PAGE: &str = "https://github.com/vin2s/yatv/releases/latest";
const DOWNLOAD: &str = "https://github.com/vin2s/yatv/releases/download";
/// The archive for this speaker's CPU: armv7 for the older stations, aarch64 for the newer ones.
pub fn asset() -> &'static str {
    if cfg!(target_arch = "aarch64") { "yatv-aarch64.tar" } else { "yatv-armv7.tar" }
}
const TAR: &str = "/tmp/yatv-update.tar";
const DIR: &str = "/tmp/yatv-update";
const LOG: &str = "/tmp/yatv-update.log";
/// Check on our own at most this often (the page's button checks at once).
const EVERY: Duration = Duration::from_secs(6 * 3600);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub notes: String,
    pub url: String,
    /// api.github.com asset URL — same trusted host as the version check.
    pub api_url: String,
    pub size: u64,
    pub digest: String,
}

#[derive(Default)]
pub struct Updates {
    latest: Mutex<Option<Release>>,
    checked: Mutex<Option<Instant>>,
    error: Mutex<Option<String>>,
    state: Mutex<Option<String>>,
}

static INSTALLING: AtomicBool = AtomicBool::new(false);

/// "v0.2.0" → (0, 2, 0); anything unreadable is (0, 0, 0).
fn parse_version(v: &str) -> (u64, u64, u64) {
    let mut n = v.trim().trim_start_matches(['v', 'V']).split(['.', '-', '+']).map(|p| p.parse().unwrap_or(0));
    (n.next().unwrap_or(0), n.next().unwrap_or(0), n.next().unwrap_or(0))
}

pub fn newer(candidate: &str, current: &str) -> bool {
    parse_version(candidate) > parse_version(current)
}

/// Without the API: GitHub redirects /releases/latest to /tag/vX.Y.Z. The API
/// is limited to 60 requests/hour per address, and many speakers share one IP.
fn latest_from_page() -> Result<String, String> {
    let agent = crate::google::agent().map_err(|e| e.to_string())?;
    let resp = agent.get(LATEST_PAGE).timeout(Duration::from_secs(15)).call()
        .map_err(|e| format!("нет связи с GitHub: {e}"))?;
    let url = resp.get_url().to_string();
    let v = url.rsplit('/').next().unwrap_or("").trim_start_matches('v').to_owned();
    if v.is_empty() { Err("GitHub не сообщил версию".into()) } else { Ok(v) }
}

fn fetch_latest() -> Result<(String, String), String> {
    let version = match latest_from_page() {
        Ok(v) => v,
        Err(_) => {
            // API fallback: it also gives the asset link and release notes.
            let agent = crate::google::agent().map_err(|e| e.to_string())?;
            let v: Value = agent.get(RELEASES).set("User-Agent", "yatv").timeout(Duration::from_secs(15)).call()
                .map_err(|e| format!("нет связи с GitHub: {e}"))?.into_json().map_err(|e| e.to_string())?;
            let ver = v.get("tag_name").and_then(Value::as_str).unwrap_or("").trim_start_matches('v').to_owned();
            let notes = v.get("body").and_then(Value::as_str).unwrap_or("").chars().take(1500).collect();
            return Ok((ver, notes));
        }
    };
    // Notes are optional; the redirect alone is enough to know a version exists.
    Ok((version, String::new()))
}

/// Release notes + asset link from the API. The redirect check is cheap and
/// API-limit-free, but gives no description; this fills it in (best effort) so
/// the panel can show «что изменилось» before installing.
fn fetch_details() -> (String, String) {
    let agent = match crate::google::agent() { Ok(a) => a, Err(_) => return (String::new(), String::new()) };
    let resp = match agent.get(RELEASES).set("User-Agent", "yatv").timeout(Duration::from_secs(15)).call() {
        Ok(r) => r, Err(_) => return (String::new(), String::new())
    };
    let v: Value = match resp.into_json() {
        Ok(v) => v, Err(_) => return (String::new(), String::new())
    };
    let notes = v.get("body").and_then(Value::as_str).unwrap_or("").chars().take(1500).collect();
    let api = v.get("assets").and_then(Value::as_array).and_then(|a| a.iter()
        .find(|x| x.get("name").and_then(Value::as_str) == Some(asset()))
        .and_then(|x| x.get("url").and_then(Value::as_str).map(str::to_owned))).unwrap_or_default();
    (notes, api)
}

impl Updates {
    pub fn due(&self) -> bool {
        self.checked.lock().unwrap().map_or(true, |t| t.elapsed() > EVERY)
    }

    pub fn check(&self) -> Result<Option<Release>, String> {
        *self.checked.lock().unwrap() = Some(Instant::now());
        let (version, mut notes) = match fetch_latest() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("update: cannot check: {e}");
                *self.error.lock().unwrap() = Some(e.clone());
                return Err(e);
            }
        };
        // The redirect path has no notes and no asset link; fill both from the
        // API once per check (best effort — the redirect still decides version).
        let (api_notes, api_url) = fetch_details();
        if notes.is_empty() { notes = api_notes; }
        *self.error.lock().unwrap() = None;
        let rel = Release {
            version: version.clone(), notes,
            url: format!("{DOWNLOAD}/v{version}/{}", asset()),
            api_url,
            size: 0, digest: String::new(),
        };
        *self.latest.lock().unwrap() = Some(rel.clone());
        Ok(newer(&version, crate::VERSION).then_some(rel))
    }

    pub fn json(&self) -> Value {
        let latest = self.latest.lock().unwrap().clone();
        serde_json::json!({
            "current": crate::VERSION,
            "latest": latest.as_ref().map(|r| r.version.clone()),
            "available": latest.as_ref().is_some_and(|r| newer(&r.version, crate::VERSION)),
            "notes": latest.as_ref().map(|r| r.notes.clone()).unwrap_or_default(),
            "installing": INSTALLING.load(Ordering::Acquire),
            "state": *self.state.lock().unwrap(),
            "error": *self.error.lock().unwrap(),
            "asset": asset(),
        })
    }

    fn set_state(&self, s: &str) {
        eprintln!("update: {s}");
        crate::log::add(&crate::state_dir(), "info", &format!("обновление: {s}"));
        *self.state.lock().unwrap() = Some(s.to_owned());
    }

    /// Downloads the archive and starts the installer; returns at once.
    pub fn install(&self) -> Result<String, String> {
        if INSTALLING.swap(true, Ordering::AcqRel) {
            return Err("обновление уже устанавливается".into());
        }
        let result = self.install_inner();
        if result.is_err() {
            INSTALLING.store(false, Ordering::Release);
        }
        result
    }

    fn install_inner(&self) -> Result<String, String> {
        let version = match self.check()? {
            Some(r) => r,
            None => return Err(format!("уже стоит последняя версия {}", crate::VERSION)),
        };
        self.set_state(&format!("скачиваю {}…", version.version));
        let ver = version.version.clone();
        let browser_url = format!("{DOWNLOAD}/v{ver}/{}", asset());
        // The speaker's wget has an old CA bundle (BusyBox has no HTTPS at all).
        // ureq carries webpki roots; download via api.github.com first (same
        // trusted host as the version check), then the browser URL, then wget.
        let downloaded = (|| -> Result<(), String> {
            let agent = crate::google::agent().map_err(|e| e.to_string())?;
            let mut last = String::new();
            for (u, api) in [(&version.api_url, true), (&browser_url, false)] {
                if u.is_empty() { continue; }
                let mut req = agent.get(u).timeout(Duration::from_secs(60));
                if api { req = req.set("Accept", "application/octet-stream").set("User-Agent", "yatv"); }
                match req.call() {
                    Ok(resp) => {
                        let mut body = Vec::new();
                        resp.into_reader().read_to_end(&mut body).map_err(|e| e.to_string())?;
                        if body.len() < 1000 { last = "пустой ответ".into(); continue; }
                        std::fs::write(TAR, &body).map_err(|e| e.to_string())?;
                        return Ok(());
                    }
                    Err(e) => last = format!("ureq: {e}"),
                }
            }
            Err(last)
        })();
        if let Err(e) = downloaded {
            eprintln!("update: ureq download failed: {e}; trying wget");
            let out = Command::new("wget")
                .args(["-q", "--no-check-certificate", "-O", TAR, "-T", "60", &browser_url])
                .stdin(Stdio::null())
                .output()
                .map_err(|e| format!("wget: {e}"))?;
            if !out.status.success() {
                let _ = std::fs::remove_file(TAR);
                return Err(format!("не скачалось {}: {}", browser_url, String::from_utf8_lossy(&out.stderr).trim()));
            }
        }
        if !crate::fsx::read_bytes(Path::new(TAR)).map(|b| b.len() > 1000).unwrap_or(false) {
            let _ = std::fs::remove_file(TAR);
            return Err("архив скачался пустым".into());
        }
        self.set_state(&format!("устанавливаю {ver}…"));
        // Unpack and run the installer in its own session: it stops yatv
        // (this process) and starts the new one. The pause lets this answer
        // reach the page first.
        let script = format!(
            "rm -rf {DIR} && mkdir -p {DIR} && tar -xf {TAR} -C {DIR} && \
             (nohup setsid sh -c 'sleep 2; sh {DIR}/yatv/install.sh' > {LOG} 2>&1 < /dev/null &) && echo started"
        );
        let out = Command::new("sh").arg("-c").arg(&script).stdin(Stdio::null()).output()
            .map_err(|e| format!("не запустился установщик: {e}"))?;
        if !String::from_utf8_lossy(&out.stdout).contains("started") {
            return Err(format!("архив не распаковался: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        // If this process is still alive long after, the installer stopped
        // before restarting yatv; report it instead of «installing…» forever.
        let version2 = ver.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(240));
            let tail = crate::fsx::read_string(Path::new(LOG)).unwrap_or_default();
            let tail: String = tail.lines().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join(" / ");
            eprintln!("update: installer for {version2} did not finish in 240 s: {tail}");
        });
        Ok(format!("Устанавливаю yatv {ver}. Через минуту вернусь"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(newer("0.2.0", "0.1.0"));
        assert!(newer("v0.1.1", "0.1.0"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.0.9", "0.1.0"));
    }

    #[test]
    fn asset_matches_target() {
        assert!(asset().starts_with("yatv-") && asset().ends_with(".tar"));
    }
}
