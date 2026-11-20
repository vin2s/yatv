//! YATV — a small, self-contained TV controller for a Yandex Station.
//!
//! It speaks to an Android TV over network ADB, understands the owner's
//! phrases (with or without soundya installed), and offers its own web panel
//! on a port that does not conflict with soundya (8765).

mod action;
mod adb;
mod b64;
mod fsx;
mod log;
mod parse;
mod server;
mod settings;
mod voice;
mod tv;

use std::{path::PathBuf, sync::mpsc::{self, Receiver, Sender}, thread, time::Duration};

pub const VERSION: &str = "0.1.0";
pub const DEFAULT_PORT: u16 = 8766;

/// What the controller thread is asked to do.
#[derive(Debug, Clone)]
pub enum Cmd {
    /// A phrase from Alice's IPC or the panel's text box. `host` is the
    /// optionally named TV («на втором этаже»), empty means the default one.
    Phrase(String, String, Sender<String>),
    /// A direct action, mostly from the panel.
    Act(Box<action::TvAction>, Sender<String>),
    /// Settings changed (TV host, aliases, …).
    Settings(serde_json::Value, Sender<String>),
    /// Stop the daemon.
    Stop,
}

fn state_dir() -> PathBuf {
    std::env::var("YATV_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/data/opt/yatv"))
}

fn main() {
    let dir = state_dir();
    fsx::ensure_dir(&dir);

    let settings = settings::Settings::load(&dir);
    tv::set_phrases(settings.phrase_list());
    tv::set_search_apps(
        settings.tvs.iter()
            .filter(|t| !t.host.is_empty() && !t.search_app.is_empty())
            .map(|t| (t.host.clone(), t.search_app.clone()))
            .collect(),
    );

    let (tx, rx) = mpsc::channel::<Cmd>();
    let port = std::env::var("YATV_WEB_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    // Alice's local IPC: phrases arrive here even if soundya is not installed.
    voice::watch(tx.clone());
    // The panel and the HTTP API: same parser, same TV actions.
    server::serve(port, tx.clone());

    controller(dir, rx);
}

fn controller(dir: PathBuf, rx: Receiver<Cmd>) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Stop => break,
            Cmd::Settings(patch, reply) => {
                // Kept minimal: the panel writes the config file itself. The
                // controller only reloads the in-memory aliases.
                let current = settings::Settings::load(&dir);
                if let Ok(s) = settings::Settings::from_patch(&current, &patch) {
                    let _ = s.save(&dir);
                    tv::set_phrases(s.phrase_list());
                    tv::set_search_apps(
                        s.tvs.iter()
                            .filter(|t| !t.host.is_empty() && !t.search_app.is_empty())
                            .map(|t| (t.host.clone(), t.search_app.clone()))
                            .collect(),
                    );
                }
                let _ = reply.send("OK: настройки сохранены".into());
            }
            Cmd::Phrase(phrase, tv_hint, reply) => {
                // Full trace: every phrase from Alice/the panel, with the TV
                // hint, so a wrong capture can be diagnosed from the journal.
                log::add(&dir, "heard", &format!("{phrase:?} hint={tv_hint:?}"));
                // First: is this even a TV command? Like soundya's device_in,
                // a named device only counts when the rest is a TV action
                // («котел на 65» is Alice's kettle, not our TV).
                let parsed = parse::parse(&phrase);
                if parsed.is_some() {
                    // Only a phrase we actually take: cancel Alice's own
                    // answer, as soundya does only after intent() succeeds.
                    // For all other phrases she answers normally (weather,
                    // greetings, her smart-home entities).
                    voice::cancel_alice();
                    thread::spawn(|| {
                        thread::sleep(std::time::Duration::from_millis(300));
                        voice::cancel_alice();
                    });
                }
                let settings = settings::Settings::load(&dir);
                // The voice hint («на втором этаже») overrides the TV saved
                // with the phrase — say which TV to use right in the phrase.
                let action_tv = if !tv_hint.is_empty() {
                    settings.find_tv(&tv_hint).map(|t| t.host).unwrap_or_default()
                } else {
                    match &parsed {
                        Some(action::TvAction::OpenOn(_, tv)) if !tv.is_empty() => {
                            settings.find_tv(tv).map(|t| t.host).unwrap_or_default()
                        }
                        _ => String::new(),
                    }
                };
                let host = if !action_tv.is_empty() {
                    action_tv
                } else if tv_hint.is_empty() {
                    String::new()
                } else if let Some(tv) = settings.find_tv(&tv_hint) {
                    log::add(&dir, "info", &format!("ТВ по фразе «{tv_hint}»: {}", tv.name));
                    tv.host
                } else if parsed.is_some() {
                    // A real TV command aimed at an unknown TV — say so.
                    let msg = format!("Не знаю телевизор «{tv_hint}» — добавьте его в панели");
                    log::add(&dir, "error", &msg);
                    let _ = reply.send(msg.clone());
                    voice::say(msg);
                    continue;
                } else {
                    // Not a TV command and no known TV: leave it to Alice.
                    log::add(&dir, "skip", &phrase);
                    let _ = reply.send("Не команда ТВ".to_owned());
                    continue;
                };
                // Panel commands: `tv ping`, `tv key up`, `tv openpkg …`.
                if let Some((action, cmd_host)) = direct_command(&phrase) {
                    let target = if cmd_host.is_empty() { host.clone() } else { cmd_host };
                    let answer = run(&dir, &action, &target);
                    let _ = reply.send(answer.clone());
                    let silent = matches!(action, action::TvAction::Apps | action::TvAction::ImeList
                        | action::TvAction::ImeEnable(_) | action::TvAction::ImeDisable(_)
                        | action::TvAction::ImeSet(_) | action::TvAction::KeyboardPrepare)
                        || answer.starts_with('{');
                    if !silent {
                        voice::say(answer);
                    }
                    continue;
                }
                // An unrecognized phrase belongs to Alice (kettle, lights,
                // her own skills). Stay silent; log as `skip` for diagnosis.
                let (answer, speak) = match parsed {
                    Some(action) => {
                        log::add(&dir, "action", &format!("{:?} @ {host}", action));
                        (run(&dir, &action, &host), true)
                    }
                    None => {
                        log::add(&dir, "skip", &phrase);
                        ("Не команда ТВ".to_owned(), false)
                    }
                };
                if speak {
                    log::add(&dir, "phrase", &format!("{phrase} → {answer}"));
                }
                let _ = reply.send(answer.clone());
                if speak {
                    voice::say(answer);
                }
            }
            Cmd::Act(action, reply) => {
                let answer = run(&dir, &action, "");
                let _ = reply.send(answer.clone());
                voice::say(answer);
            }
        }
    }
}

/// Direct panel commands. `tv@<host> <command>` targets one TV; plain
/// `tv <command>` uses the default one.
fn direct_command(text: &str) -> Option<(action::TvAction, String)> {
    let t = text.trim();
    let (arg, at_host) = if let Some(after) = t.strip_prefix("tv@") {
        let (host, rest) = after.split_once(' ').unwrap_or((after, ""));
        (rest.trim(), host.trim().to_owned())
    } else if let Some(after) = t.strip_prefix("tv ") {
        (after.trim(), String::new())
    } else {
        return None;
    };
    let (what, value) = arg.split_once(' ').unwrap_or((arg, ""));
    let value = value.trim();
    use action::TvAction::*;
    let action = match what {
        "ping" => Some(Ping),
        "wake" | "on" => Some(Wake),
        "sleep" | "off" => Some(Sleep),
        "apps" => Some(Apps),
        "open" if !value.is_empty() => Some(Open(value.to_owned())),
        "openpkg" if !value.is_empty() => Some(OpenPackage(value.to_owned())),
        "close" if !value.is_empty() => Some(Close(value.to_owned())),
        "search" if !value.is_empty() => Some(Search(value.to_owned())),
        "youtube" if !value.is_empty() => Some(YouTube(value.to_owned())),
        "tap" if !value.is_empty() => Some(Tap(value.to_owned())),
        "first" => Some(PlayFirst),
        "key" => {
            let code = match value {
                "up" => "KEYCODE_DPAD_UP",
                "down" => "KEYCODE_DPAD_DOWN",
                "left" => "KEYCODE_DPAD_LEFT",
                "right" => "KEYCODE_DPAD_RIGHT",
                "center" | "ok" => "KEYCODE_DPAD_CENTER",
                "back" => "KEYCODE_BACK",
                "home" => "KEYCODE_HOME",
                "play" => "KEYCODE_MEDIA_PLAY_PAUSE",
                "volume_up" => "KEYCODE_VOLUME_UP",
                "volume_down" => "KEYCODE_VOLUME_DOWN",
                _ => return None,
            };
            Some(Key(code, 1))
        }
        "ime" => match value {
            "list" => Some(ImeList),
            v if v.starts_with("enable ") => Some(ImeEnable(v[7..].trim().to_owned())),
            v if v.starts_with("disable ") => Some(ImeDisable(v[8..].trim().to_owned())),
            v if v.starts_with("set ") => Some(ImeSet(v[4..].trim().to_owned())),
            _ => None,
        },
        "keyboard-prepare" => Some(KeyboardPrepare),
        _ => None,
    };
    action.map(|a| (a, at_host))
}

fn run(dir: &PathBuf, action: &action::TvAction, host_override: &str) -> String {
    let settings = settings::Settings::load(dir);
    let host = if !host_override.trim().is_empty() {
        host_override.trim().to_owned()
    } else {
        settings.tv_host.clone()
    };
    if host.trim().is_empty() {
        return "error: адрес телевизора не задан в настройках".to_owned();
    }
    match tv::run(&host, dir, action) {
        Ok(text) => text,
        Err(e) => {
            let msg = format!("error: {e} @ {host}");
            log::add(dir, "error", &msg);
            msg
        }
    }
}

/// Marked ready once the panel and the voice listener are up: the installer
/// waits for this file and rolls back if it does not appear.
pub fn mark_ready() {
    let run = std::env::var("YATV_RUN").unwrap_or_else(|_| "/run".into());
    let path = std::path::Path::new(&run).join("yatv.ready");
    let text = format!("{VERSION} {}\n", std::process::id());
    if let Err(e) = std::fs::write(&path, text) {
        eprintln!("ready: {}: {e}", path.display());
    }
}

/// The small delay before the first connection: give the TV time to wake.
pub fn settle() {
    thread::sleep(Duration::from_millis(300));
}
