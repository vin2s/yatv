//! YATV — a small, self-contained TV controller for a Yandex Station.
//!
//! It speaks to an Android TV over network ADB, understands the owner's
//! phrases (with or without yatv installed), and offers its own web panel
//! on a port that does not conflict with yatv (8765).

mod action;
mod adb;
mod b64;
mod brief;
mod calendar;
mod fsx;
mod google;
mod log;
mod parse;
mod planner;
mod server;
mod settings;
mod sha256;
mod voice;
mod tv;
mod transfer;
mod update;

use std::{path::PathBuf, sync::mpsc::{self, Receiver, Sender}, thread, time::Duration};

pub const VERSION: &str = "0.2.9";
pub const DEFAULT_PORT: u16 = 8766;

/// Shared update checker (the panel and the background watcher use it).
static UPDATES: std::sync::OnceLock<update::Updates> = std::sync::OnceLock::new();
fn updates() -> &'static update::Updates {
    UPDATES.get_or_init(update::Updates::default)
}

/// What the controller thread is asked to do.
#[derive(Debug, Clone)]
pub enum Cmd {
    /// A phrase from Alice's IPC or the panel's text box. `host` is the
    /// optionally named TV («на втором этаже»), empty means the default one.
    Phrase(String, String, Sender<String>),
    /// A direct action, mostly from the panel.
    Act(Box<action::TvAction>, Sender<String>),
    /// Planner command from the panel: `plan json`, `plan add …`, `plan done 1`.
    Plan(String, Sender<String>),
    /// Settings changed (TV host, aliases, …).
    Settings(serde_json::Value, Sender<String>),
    /// Update check/install from the panel or the background watcher.
    Update(String, Sender<String>),
    /// Export/import the whole configuration between speakers.
    Transfer(String, String, Sender<String>),
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
    planner::configure(true, settings.extra.get("tz_offset").and_then(|v| v.as_i64()).unwrap_or(180) as i32);
    apply_planner(&settings);
    planner::run(&dir);
    apply_google(&settings);
    calendar::run(&dir);
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

    // Alice's local IPC: phrases arrive here even if yatv is not installed.
    voice::watch(tx.clone());
    // The panel and the HTTP API: same parser, same TV actions.
    server::serve(port, tx.clone());

    // Check GitHub for a newer release; if auto is on, install it silently.
    thread::spawn(|| {
        let u = updates();
        loop {
            if u.due() {
                match u.check() {
                    Ok(Some(rel)) => {
                        log::add(&crate::state_dir(), "info", &format!("доступно обновление {}: будет установлено", rel.version));
                        // Auto-install is on by default; the panel can turn it off.
                        let s = settings::Settings::load(&crate::state_dir());
                        if s.extra.get("update_auto").and_then(|v| v.as_bool()).unwrap_or(true) {
                            if let Err(e) = u.install() {
                                eprintln!("update: auto install failed: {e}");
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(e) => eprintln!("update: auto check failed: {e}"),
                }
            }
            thread::sleep(Duration::from_secs(1800));
        }
    });

    controller(dir, rx);
}

fn controller(dir: PathBuf, rx: Receiver<Cmd>) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Cmd::Stop => break,
            Cmd::Transfer(what, body, reply) => {
                let answer = match what.as_str() {
                    "export" => transfer::export(&dir).to_string(),
                    "import" => match transfer::import(&dir, &body) {
                        Ok(v) => {
                            // Re-apply the in-memory state without a restart.
                            let s = settings::Settings::load(&dir);
                            planner::configure(true, s.extra.get("tz_offset").and_then(|v| v.as_i64()).unwrap_or(180) as i32);
                            apply_planner(&s);
                            apply_google(&s);
                            tv::set_phrases(s.phrase_list());
                            tv::set_search_apps(s.tvs.iter().filter(|t| !t.host.is_empty() && !t.search_app.is_empty())
                                .map(|t| (t.host.clone(), t.search_app.clone())).collect());
                            calendar::wake();
                            format!("OK: конфигурация импортирована: {}", v)
                        }
                        Err(e) => format!("error: {e}"),
                    },
                    _ => "usage: export | import <json>".into(),
                };
                let _ = reply.send(answer);
            }
            Cmd::Update(what, reply) => {
                let answer = match what.as_str() {
                    "check" => match updates().check() {
                        Ok(Some(r)) => format!("OK: доступна версия {} — {}", r.version, if r.notes.is_empty() { "обновление" } else { &r.notes }),
                        Ok(None) => format!("OK: последняя версия {}", VERSION),
                        Err(e) => format!("error: {e}"),
                    },
                    "json" => updates().json().to_string(),
                    "install" => match updates().install() {
                        Ok(s) => format!("OK: {s}"),
                        Err(e) => format!("error: {e}"),
                    },
                    _ => "usage: update check|json|install".into(),
                };
                let _ = reply.send(answer);
            }
            Cmd::Settings(patch, reply) => {
                // Google client id/secret are secrets in the journal: log only presence.
                let g_id = patch.get("g_client_id").and_then(|v| v.as_str()).unwrap_or("");
                if patch.get("g_client_id").is_some() {
                    log::add(&dir, "google", &format!(
                        "сохранение ключа: client_id={}, secret={}",
                        if g_id.is_empty() { "ПУСТО" } else { "есть" },
                        if patch.get("g_client_secret").and_then(|v| v.as_str()).unwrap_or("").is_empty() { "ПУСТО" } else { "есть" }
                    ));
                }
                // Kept minimal: the panel writes the config file itself. The
                // controller only reloads the in-memory aliases.
                let current = settings::Settings::load(&dir);
                if let Ok(s) = settings::Settings::from_patch(&current, &patch) {
                    let _ = s.save(&dir);
                    planner::configure(true, s.extra.get("tz_offset").and_then(|v| v.as_i64()).unwrap_or(180) as i32);
                    apply_planner(&s);
                    apply_google(&s);
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
                // «Стоп» belongs to Alice unless our brief is actually speaking.
                // When it is, we end our own reading — otherwise we must NOT
                // cancel her: her backend is what stops her own music.
                let said_text = planner_text(&phrase);
                if ["стоп", "стоп сводка", "останови сводку", "хватит"].contains(&said_text.as_str()) {
                    if brief::is_active() {
                        brief::request_stop();
                        log::add(&dir, "info", "сводка: остановлена фразой «стоп»");
                        let _ = reply.send("OK: сводка остановлена".to_owned());
                        continue;
                    }
                    // No brief: leave it entirely to Alice and do not cancel her.
                    log::add(&dir, "skip", &phrase);
                    let _ = reply.send("Не команда ТВ".to_owned());
                    continue;
                }
                // The brief runs only on the phrase the owner typed in the panel.
                // Any other phrase (including «доброе утро» by default) is left
                // entirely to stock Alice.
                let s = settings::Settings::load(&dir);
                let trigger = brief::trigger_from(&s);
                if brief::is_trigger(&phrase, &trigger)
                    && s.extra.get("brief_on").and_then(|v| v.as_bool()).unwrap_or(true) {
                    log::add(&dir, "action", &format!("сводка по фразе {phrase:?}"));
                    voice::cancel_alice();
                    let answer = "OK: сводка началась".to_owned();
                    let _ = reply.send(answer.clone());
                    thread::spawn(move || { brief::run(&s); });
                    continue;
                }
                // Planner has its own narrow whitelist. A plan phrase is ours
                // (cancel Alice, answer, speak) and never reaches the TV parser.
                // «Напомни…» is the exception: Alice's reminders announce through
                // the умный дом on every speaker at once, and a yatv-owned copy
                // would repeat the same words seconds later on this one only —
                // both engines were firing on one phrase.
                let plan_intent = planner::voice(&planner_text(&phrase));
                let ours = !matches!(plan_intent, Some(planner::Voice::Remind(_)));
                if let (true, Some(v)) = (ours, plan_intent) {
                    log::add(&dir, "action", &format!("план: {phrase:?}"));
                    // Stock Alice answers «планы/сегодня/завтра» herself: take
                    // the phrase away from her, as on other controllers.
                    voice::cancel_alice();
                    thread::spawn(|| {
                        thread::sleep(std::time::Duration::from_millis(250));
                        voice::cancel_alice();
                        thread::sleep(std::time::Duration::from_millis(400));
                        voice::cancel_alice();
                    });
                    let answer = planner::handle(v);
                    log::add(&dir, "phrase", &format!("{phrase} → {answer}"));
                    let _ = reply.send(answer.clone());
                    voice::say(answer);
                    continue;
                }
                // First: is this even a TV command? Like yatv's device_in,
                // a named device only counts when the rest is a TV action
                // («котел на 65» is Alice's kettle, not our TV).
                let parsed = parse::parse(&phrase);
                if parsed.is_some() {
                    // Only a phrase we actually take: cancel Alice's own
                    // answer, as yatv does only after intent() succeeds.
                    // For all other phrases she answers normally (weather,
                    // greetings, her smart-home entities).
                    voice::cancel_alice();
                    thread::spawn(|| {
                        thread::sleep(std::time::Duration::from_millis(250));
                        voice::cancel_alice();
                        thread::sleep(std::time::Duration::from_millis(400));
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
            Cmd::Plan(command, reply) => {
                let answer = plan_command(&command);
                let _ = reply.send(answer);
            }
        }
    }
}

fn apply_planner(s: &settings::Settings) {
    let defaults = planner::Plan::default();
    planner::configure_plan(planner::Plan {
        warn: s.extra.get("plan_warn").and_then(|v| v.as_u64()).map(|v| v.clamp(0, 240) as u16).unwrap_or(defaults.warn),
        due_time: s.extra.get("plan_due_time").and_then(|v| v.as_u64()).map(|v| v.clamp(0, 1440) as u16).unwrap_or(defaults.due_time),
        brief_on: s.extra.get("brief_on").and_then(|v| v.as_bool()).unwrap_or(defaults.brief_on),
        brief_at: s.extra.get("brief_at").and_then(|v| v.as_u64()).map(|v| v.clamp(0, 1439) as u16).unwrap_or(defaults.brief_at),
        brief_days: s.extra.get("brief_days").and_then(|v| v.as_str()).map(|v| if v == "weekdays" { "weekdays".into() } else { "all".into() }).unwrap_or(defaults.brief_days),
    });
}

fn get_bool(s: &settings::Settings, key: &str) -> bool {
    s.extra.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}
fn get_strings(s: &settings::Settings, key: &str) -> Vec<String> {
    s.extra.get(key).and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default()
}
fn get_string(s: &settings::Settings, key: &str) -> String {
    s.extra.get(key).and_then(|v| v.as_str()).unwrap_or("").to_owned()
}
fn apply_google(s: &settings::Settings) {
    google::configure(google::Config {
        client_id: get_string(s, "g_client_id"), secret: get_string(s, "g_client_secret"), refresh: get_string(s, "g_refresh"),
    });
    calendar::configure(get_bool(s, "g_on") && google::connected(), &get_strings(s, "g_read"),
        &get_string(s, "g_write"), &get_string(s, "g_list"));
}

/// Alice's phrases arrive with the wake word; the panel sends clean text.
fn planner_text(text: &str) -> String {
    let t = text.trim().to_lowercase().replace('ё', "е");
    let t = t.strip_prefix("алиса, ").unwrap_or(&t).trim().to_owned();
    t.trim_end_matches(['?', '!', '.']).trim().to_owned()
}

/// Pure classification of a phrase, used by the debug panel. The live
/// `Cmd::Phrase` handler below mirrors this order; keep them in sync.
pub fn classify_phrase(phrase: &str) -> serde_json::Value {
    let s = settings::Settings::load(&crate::state_dir());
    let trigger = brief::trigger_from(&s);
    let brief_on = s.extra.get("brief_on").and_then(|v| v.as_bool()).unwrap_or(true);
    let said_text = planner_text(phrase);

    let stop = ["стоп", "стоп сводка", "останови сводку", "хватит"].contains(&said_text.as_str());
    if stop {
        return serde_json::json!({
            "owner": if brief::is_active() { "yatv" } else { "alice" },
            "route": "stop",
            "brief_active": brief::is_active(),
            "detail": if brief::is_active() { "YATV остановит сводку" } else { "сводки нет — фраза остаётся Алисе" },
        });
    }
    if brief::is_trigger(phrase, &trigger) && brief_on {
        return serde_json::json!({
            "owner": "yatv", "route": "brief",
            "detail": "запущена утренняя сводка (новости/погода/планы)",
        });
    }
    let plan_intent = planner::voice(&said_text);
    if plan_intent.is_some() {
        if matches!(plan_intent, Some(planner::Voice::Remind(_))) {
            return serde_json::json!({
                "owner": "alice", "route": "remind",
                "detail": "напоминания остаются штатной Алисе",
            });
        }
        return serde_json::json!({
            "owner": "yatv", "route": "planner",
            "detail": "планировщик: фраза из белого списка",
        });
    }
    let parsed = parse::parse(&said_text);
    if parsed.is_some() {
        return serde_json::json!({
            "owner": "yatv", "route": "tv",
            "detail": format!("команда ТВ: {:?}", parsed),
        });
    }
    // A phrase explicitly configured in the panel is YATV's too.
    let lower = said_text.to_lowercase();
    let configured = s.phrases.iter().any(|p| {
        p.phrase.to_lowercase().split([',', ';']).any(|v| !v.trim().is_empty() && lower.contains(v.trim()))
    });
    if configured {
        return serde_json::json!({
            "owner": "yatv", "route": "phrase",
            "detail": "совпадает с фразой из настроек ТВ",
        });
    }
    serde_json::json!({
        "owner": "alice", "route": "alice",
        "detail": "YATV не распознал — фраза остаётся Алисе",
    })
}

/// The panel's `plan …` commands.
fn plan_command(command: &str) -> String {
    let (what, rest) = command.split_once(' ').unwrap_or((command, ""));
    // Read-only panel commands («json», «text», «brief json», …) are polled
    // constantly — logging them would push the real events out of the journal.
    let noisy = matches!(what, "json" | "text" | "speak") && rest.trim() != "test";
    let answer = plan_dispatch(what, rest);
    if !noisy {
        log::add(&crate::state_dir(), "cmd", &format!("{what} {}", redact(&answer)));
    }
    answer
}

/// Secrets never reach the journal: a pasted OAuth address holds a code.
fn redact(text: &str) -> String {
    let mut out = text.to_owned();
    for marker in ["code=", "client_secret=", "g_refresh"] {
        if let Some(at) = out.find(marker) {
            let tail = out[at..].split_whitespace().next().unwrap_or("");
            out = out.replace(tail, "<скрыто>");
        }
    }
    out.chars().take(300).collect()
}

/// Panel view name → planner question. A bare integer is a day offset from
/// today (the calendar sends the selected day that way); negative = past.
fn parse_ask(view: &str) -> planner::Ask {
    match view.trim() {
        "today" => planner::Ask::Plans(0),
        "tomorrow" => planner::Ask::Plans(1),
        "week" => planner::Ask::Week,
        "nextweek" => planner::Ask::NextWeek,
        "deadlines" => planner::Ask::Deadlines,
        "brief" => planner::Ask::Brief,
        other => match other.parse::<i64>() {
            Ok(d) => planner::Ask::Plans(d),
            Err(_) => planner::Ask::Plans(0),
        },
    }
}

fn plan_dispatch(what: &str, rest: &str) -> String {
    match what {
        "json" => {
            let mut v = planner::json();
            if let Some(obj) = v.as_object_mut() {
                obj.insert("calendar".into(), crate::calendar::json());
            }
            v.to_string()
        },
        "add" => match serde_json::from_str::<serde_json::Value>(rest) {
            Ok(v) if v["title"].as_str().is_some_and(|t| !t.trim().is_empty()) => {
                let t = planner::add(
                    v["title"].as_str().unwrap_or(""),
                    v["due"].as_i64(),
                    v["repeat"].as_str().unwrap_or(""),
                    v["remind"].as_bool().unwrap_or(v["due"].is_i64()),
                    v["category"].as_str().unwrap_or(""),
                    v["priority"].as_str().unwrap_or("normal"),
                );
                format!("OK: добавлено «{}»", t.title)
            }
            _ => "error: нужен заголовок".into(),
        },
        "done" => if planner::set_done(rest.trim().parse().unwrap_or(0), true) { "OK".into() } else { "error: нет такой задачи".into() },
        "undone" => if planner::set_done(rest.trim().parse().unwrap_or(0), false) { "OK".into() } else { "error: нет такой задачи".into() },
        "del" => if planner::delete(rest.trim().parse().unwrap_or(0)) { "OK".into() } else { "error: нет такой задачи".into() },
        "edit" => {
            let v: serde_json::Value = serde_json::from_str(rest).unwrap_or_default();
            match planner::edit(v["id"].as_u64().unwrap_or(0), v["title"].as_str().unwrap_or(""), v["due"].as_i64(),
                                v["repeat"].as_str().unwrap_or(""), v["priority"].as_str().unwrap_or("normal")) {
                Some(t) => format!("OK: сохранено «{}»", t.title),
                None => "error: нет такой задачи или пустое название".into(),
            }
        }
        "gdone" => {
            let v: serde_json::Value = serde_json::from_str(rest).unwrap_or_default();
            match crate::calendar::complete_task(v["list"].as_str().unwrap_or(""), v["id"].as_str().unwrap_or("")) {
                Ok(_) => "OK: выполнено".into(),
                Err(e) => format!("error: {e}"),
            }
        }
        "gdel" => {
            let v: serde_json::Value = serde_json::from_str(rest).unwrap_or_default();
            match crate::calendar::delete_task(v["list"].as_str().unwrap_or(""), v["id"].as_str().unwrap_or("")) {
                Ok(_) => "OK: удалено".into(),
                Err(e) => format!("error: {e}"),
            }
        }
        "gedit" => {
            let v: serde_json::Value = serde_json::from_str(rest).unwrap_or_default();
            let title = v["title"].as_str().unwrap_or("").trim().to_owned();
            if title.is_empty() {
                "error: нужно название".into()
            } else {
                match crate::calendar::update_task(
                    v["list"].as_str().unwrap_or(""), v["id"].as_str().unwrap_or(""), &title,
                    v["due_day"].as_i64(), v["notes"].as_str().unwrap_or("")) {
                    Ok(_) => format!("OK: сохранено «{title}»"),
                    Err(e) => format!("error: {e}"),
                }
            }
        }
        "say" => match planner::voice(&planner_text(rest)) {
            Some(v) => planner::handle(v),
            None => "error: не поняла фразу".into(),
        },
        "calendar" => {
            calendar::wake();
            "OK: обновляю календарь".into()
        }
        "syncspeak" => {
            // «Обновить и прочитать вслух»: refresh Google data, then read the
            // chosen day aloud. A bare integer is a day offset from today
            // (the calendar panel sends the selected day that way).
            calendar::wake();
            let ask = parse_ask(rest.trim());
            let text = planner::agenda_text(ask, planner::now(), planner::offset());
            voice::say(text.clone());
            format!("OK: обновляю и читаю вслух: {}", text)
        }
        "brief" => {
            let s = settings::Settings::load(&crate::state_dir());
            match rest.trim() {
                "test" => {
                    let s2 = s.clone();
                    thread::spawn(move || { brief::run(&s2); });
                    "OK: сводка началась".to_owned()
                }
                "json" => serde_json::json!({
                    "on": s.extra.get("brief_on").and_then(|v| v.as_bool()).unwrap_or(true),
                    "trigger": brief::trigger_from(&s),
                    "weather_url": s.extra.get("brief_weather_url").and_then(|v| v.as_str()).unwrap_or(""),
                    "weather_provider": s.extra.get("brief_weather_provider").and_then(|v| v.as_str()).unwrap_or("open-meteo"),
                    "weather_sources": brief::WEATHER_SOURCES.iter().map(|(k, n)| serde_json::json!({"key": k, "name": n})).collect::<Vec<_>>(),
                    "order": s.extra.get("brief_order").and_then(|v| v.as_str()).unwrap_or("weather,plans,news"),
                    "news": s.extra.get("brief_news").and_then(|v| v.as_str()).unwrap_or(""),
                    "news_count": s.extra.get("brief_news_count").and_then(|v| v.as_u64()).unwrap_or(5),
                    "weather": s.extra.get("brief_weather").and_then(|v| v.as_bool()).unwrap_or(true),
                    "plans": s.extra.get("brief_plans").and_then(|v| v.as_bool()).unwrap_or(true),
                    "lat": s.extra.get("brief_lat").and_then(|v| v.as_f64()),
                    "lon": s.extra.get("brief_lon").and_then(|v| v.as_f64()),
                    "sources": brief::SOURCES.iter().map(|(k, n, _)| serde_json::json!({"key": k, "name": n})).collect::<Vec<_>>(),
                }).to_string(),
                _ => "usage: brief test | brief json".into(),
            }
        }
        "google" => {
            let (what, arg) = rest.split_once(' ').unwrap_or((rest, ""));
            match what {
                "url" => {
                    let s = settings::Settings::load(&crate::state_dir());
                    let id = get_string(&s, "g_client_id");
                    let secret = get_string(&s, "g_client_secret");
                    log::add(&crate::state_dir(), "google", &format!(
                        "запрос ссылки: client_id={}, secret={}",
                        if id.is_empty() { "ПУСТО" } else { "есть" },
                        if secret.is_empty() { "ПУСТО" } else { "есть" }
                    ));
                    if id.is_empty() || secret.is_empty() {
                        "error: сначала сохраните ID и секрет клиента (шаг 1: загрузите JSON ключа или введите вручную)".into()
                    } else {
                        format!("OK {}", google::auth_url(&id))
                    }
                }
                "device" => {
                    let s = settings::Settings::load(&crate::state_dir());
                    let (id, secret) = (get_string(&s, "g_client_id"), get_string(&s, "g_client_secret"));
                    log::add(&crate::state_dir(), "google", &format!(
                        "запрос device-кода: client_id={}, secret={}",
                        if id.is_empty() { "ПУСТО" } else { "есть" },
                        if secret.is_empty() { "ПУСТО" } else { "есть" }
                    ));
                    if id.is_empty() || secret.is_empty() {
                        "error: сначала сохраните ID и секрет клиента (шаг 1: загрузите JSON ключа или введите вручную)".into()
                    } else {
                        let r = google::agent().and_then(|a| google::device_start(&a, &id, &secret));
                        match r {
                            Ok(prompt) => {
                                log::add(&crate::state_dir(), "google", &format!("device-код получен: {}", prompt.user_code));
                                voice::say(format!("Зайдите на {} и введите код {}", prompt.verification_url, prompt.user_code));
                                serde_json::json!({ "ok": true, "user_code": prompt.user_code, "url": prompt.verification_url }).to_string()
                            }
                            Err(e) => {
                                log::add(&crate::state_dir(), "google", &format!("device-код: ошибка: {e}"));
                                format!("error: {e}")
                            }
                        }
                    }
                }
                "device-poll" => {
                    log::add(&crate::state_dir(), "google", "опрос device-токена");
                    let r = google::agent().and_then(|a| google::device_poll(&a));
                    match r {
                        Ok(None) => {
                            log::add(&crate::state_dir(), "google", "device: пользователь ещё не разрешил");
                            "pending".into()
                        }
                        Ok(Some(refresh)) => {
                            log::add(&crate::state_dir(), "google", "device: токен получен, сохраняю");
                            let dir = crate::state_dir();
                            let mut s = settings::Settings::load(&dir);
                            s.extra.insert("g_refresh".into(), serde_json::json!(refresh));
                            s.extra.insert("g_on".into(), serde_json::json!(true));
                            if let Err(e) = s.save(&dir) {
                                format!("error: не сохранилось: {e}")
                            } else {
                                apply_google(&s);
                                "OK: Google подключён".into()
                            }
                        }
                        Err(e) => {
                            log::add(&crate::state_dir(), "google", &format!("device: ошибка: {e}"));
                            format!("error: {e}")
                        },
                    }
                }
                "code" => {
                let dir = crate::state_dir();
                let mut s = settings::Settings::load(&dir);
                let (id, secret, pasted) = (get_string(&s, "g_client_id"), get_string(&s, "g_client_secret"), arg.trim().to_owned());
                log::add(&dir, "google", &format!(
                    "обмен кода: client_id={}, secret={}, адрес={}",
                    if id.is_empty() { "ПУСТО" } else { "есть" },
                    if secret.is_empty() { "ПУСТО" } else { "есть" },
                    redact(&pasted)
                ));
                let (tx, rx) = std::sync::mpsc::channel();
                // Exchange in a thread: the network must not block the controller.
                std::thread::spawn(move || {
                    let r = google::agent().and_then(|a| google::exchange(&a, &id, &secret, &pasted));
                    let _ = tx.send(r);
                });
                match rx.recv_timeout(std::time::Duration::from_secs(40)) {
                    Ok(Ok(refresh)) => {
                        log::add(&dir, "google", "код принят, сохраняю refresh-токен");
                        s.extra.insert("g_refresh".into(), serde_json::json!(refresh));
                        s.extra.insert("g_on".into(), serde_json::json!(true));
                        if let Err(e) = s.save(&dir) {
                            format!("error: не сохранилось: {e}")
                        } else {
                            apply_google(&s);
                            "OK: Google подключён".into()
                        }
                    }
                    Ok(Err(e)) => {
                        log::add(&dir, "google", &format!("обмен кода: ошибка: {e}"));
                        format!("error: {e}")
                    },
                    Err(_) => {
                        log::add(&dir, "google", "обмен кода: таймаут");
                        "error: timeout".into()
                    },
                }
            }
            "off" => {
                let dir = crate::state_dir();
                log::add(&dir, "google", "отключение Google");
                let mut s = settings::Settings::load(&dir);
                s.extra.insert("g_refresh".into(), serde_json::json!(""));
                s.extra.insert("g_on".into(), serde_json::json!(false));
                if let Err(e) = s.save(&dir) {
                    format!("error: не сохранилось: {e}")
                } else {
                    apply_google(&s);
                    "OK: Google отключён".into()
                }
            }
            _ => "usage: google url | google code <адрес> | google off".into(),
            }
        }
        "text" | "speak" => {
            let ask = parse_ask(rest.trim());
            let text = planner::agenda_text(ask, planner::now(), planner::offset());
            if what == "speak" {
                voice::say(text);
                "OK".into()
            } else {
                text
            }
        }
        _ => "usage: plan json | add <json> | done|undone|del <id> | edit <json> | say <phrase> | text|speak <view>".into(),
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
        settings.tv_list().first().map(|t| t.host.clone()).unwrap_or_else(|| settings.tv_host.clone())
    };
    if host.trim().is_empty() {
        return "error: адрес телевизора не задан в настройках".to_owned();
    }
    match tv::run(&host, dir, action) {
        Ok(text) => {
            log::add(dir, "action", &format!("ТВ {host}: {:?} → {}", action, text.chars().take(120).collect::<String>()));
            text
        }
        Err(e) => {
            let msg = format!("error: {e} @ {host}");
            log::add(dir, "error", &msg);
            msg
        }
    }
}

/// Marked ready once the panel and the voice listener are up: the installer
/// waits for this file and rolls back if it does not appear.
#[cfg(test)]
mod plan_tests {
    use super::*;

    #[test]
    fn plan_say_adds_a_task() {
        let r = plan_command("say добавь задачу купить хлеб завтра в 10");
        assert!(!r.starts_with("error"), "{r}");
    }

    #[test]
    fn debug_classifies_owned_and_alice_phrases() {
        // A planner phrase belongs to YATV; a reminder stays with Alice.
        let plans = classify_phrase("какие планы на сегодня");
        assert_eq!(plans["owner"], "yatv");
        assert_eq!(plans["route"], "planner");
        let remind = classify_phrase("напомни позвонить врачу");
        assert_eq!(remind["owner"], "alice");
        assert_eq!(remind["route"], "remind");
        // With no active brief, «стоп» is Alice's.
        let stop = classify_phrase("стоп");
        assert_eq!(stop["owner"], "alice");
    }
}

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
