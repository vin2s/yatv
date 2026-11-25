//! Google Calendar and Google Tasks cache for YATV.
//!
//! Reads events and open tasks into a local cache (google-cache.json), so
//! «какие планы на сегодня» works even without a network. Writes go straight
//! to Google and are shown in the panel at once; the next refresh confirms
//! them. The planner and the panel read from here; nothing secret is logged.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use crate::google::{self, GTask};
use crate::planner::{deadline_time, local, now, offset, Event};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CalEvent {
    pub uid: String,
    pub title: String,
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub cal: String,
}

#[derive(Default)]
struct State {
    on: bool,
    /// Calendars to read; empty = the ones selected in Google Calendar itself.
    read: Vec<String>,
    /// Where new events go ("primary" by default) and the list for new tasks.
    write: String,
    list: String,
    cals: Vec<google::Cal>,
    lists: Vec<(String, String)>,
    events: Vec<CalEvent>,
    tasks: Vec<GTask>,
    /// Lists whose open tasks were all read in the last successful fetch.
    full_lists: HashSet<String>,
    error: String,
    /// Last refresh numbers: the journal line appears only when they change.
    last_counts: (usize, usize, usize, usize),
    fetched: i64,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
static WAKE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static AGENT: OnceLock<ureq::Agent> = OnceLock::new();

fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    let mut g = STATE.lock().unwrap();
    f(g.get_or_insert_with(State::default))
}

fn agent() -> Result<&'static ureq::Agent, String> {
    if AGENT.get().is_none() {
        let _ = AGENT.set(google::agent()?);
    }
    AGENT.get().ok_or_else(|| "нет подключения к Google".to_owned())
}

pub fn configure(on: bool, read: &[String], write: &str, list: &str) {
    let changed = with(|s| {
        let changed = s.on != on || s.read != read;
        s.on = on;
        s.read = read.to_vec();
        s.write = if write.is_empty() { "primary".into() } else { write.to_owned() };
        s.list = list.to_owned();
        if !on {
            s.events.clear();
            s.tasks.clear();
            s.full_lists.clear();
            s.error.clear();
        }
        changed
    });
    // Only a real change is worth a journal line: `configure` runs on every
    // settings save, and the panel saves settings often (tz, brief, …).
    if changed {
        crate::log::add(&crate::state_dir(), "info", &format!(
            "календарь Google: {} (календарей читать: {}, запись: {}, список задач: {})",
            if on { "вкл" } else { "выкл" }, read.len(),
            if write.is_empty() { "primary" } else { write },
            if list.is_empty() { "первый" } else { list }
        ));
        wake();
    }
}

pub fn google_on() -> bool {
    with(|s| s.on)
}

pub fn active() -> bool {
    with(|s| s.on || !s.events.is_empty() || !s.tasks.is_empty())
}

pub fn wake() {
    WAKE.store(true, std::sync::atomic::Ordering::Release);
}

pub fn google_tasks() -> Vec<GTask> {
    with(|s| if s.on { s.tasks.clone() } else { vec![] })
}

/// Google Calendar events for the planner's day summary. Google Tasks are
/// listed separately by planner::collect (as `task`), so they are NOT turned
/// into events here — otherwise every task appears twice in the panel.
pub fn between(from: i64, to: i64) -> Vec<Event> {
    with(|s| {
        s.events.iter()
            .filter(|e| e.start < to && e.end.max(e.start + 1) > from)
            .map(|e| Event {
                uid: e.uid.clone(), title: e.title.clone(), start: e.start, end: e.end,
                all_day: e.all_day, cal: e.cal.clone(),
            })
            .collect()
    })
}

pub fn json() -> Value {
    let (n, off) = (now(), offset());
    with(|s| {
        let read: Vec<&google::Cal> = s.cals.iter()
            .filter(|c| if s.read.is_empty() { c.selected } else { s.read.contains(&c.id) })
            .collect();
        json!({
            "google": {
                "on": s.on,
                "error": s.error,
                "fetched": s.fetched,
                "calendars": s.cals.iter().map(|c| json!({
                    "id": c.id, "name": c.name, "primary": c.primary, "writable": c.writable,
                    "read": read.iter().any(|r| r.id == c.id),
                })).collect::<Vec<_>>(),
                "lists": s.lists.iter().map(|(id, t)| json!({ "id": id, "title": t })).collect::<Vec<_>>(),
                "write": s.write,
                "list": s.list,
                "events": s.events.len(),
                "tasks": s.tasks.len(),
            },
            "on": s.on,
            "error": s.error,
            "fetched": s.fetched,
            "next": s.events.iter().take(8).map(|e| json!({
                "title": e.title, "cal": e.cal, "start": e.start, "all_day": e.all_day,
                "when": crate::planner::say_time(e.start, n, off),
            })).collect::<Vec<_>>(),
        })
    })
}

// ------------------------------------------------------------------ cache

fn cache(dir: &Path) -> PathBuf {
    dir.join("google-cache.json")
}

fn load_cache(dir: &Path) {
    let Some(v) = crate::fsx::read_string(&cache(dir)).and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { return };
    let events: Vec<CalEvent> = serde_json::from_value(v["events"].clone()).unwrap_or_default();
    let tasks: Vec<GTask> = v["tasks"].as_array().map(|a| a.iter().map(|t| GTask {
        id: t["id"].as_str().unwrap_or("").into(), list: t["list"].as_str().unwrap_or("").into(),
        title: t["title"].as_str().unwrap_or("").into(), notes: t["notes"].as_str().unwrap_or("").into(),
        due_day: t["due_day"].as_i64(),
    }).collect()).unwrap_or_default();
    with(|s| {
        s.events = events;
        s.tasks = tasks;
    });
}

fn save_cache(dir: &Path, events: &[CalEvent], tasks: &[GTask]) {
    let v = json!({ "events": events, "tasks": tasks.iter().map(|t| json!({
        "id": t.id, "list": t.list, "title": t.title, "notes": t.notes, "due_day": t.due_day,
    })).collect::<Vec<_>>() });
    let _ = std::fs::write(cache(dir), v.to_string());
}

fn refresh() {
    let (on, read) = with(|s| (s.on, s.read.clone()));
    if !on || !google::connected() {
        return;
    }
    let result = (|| -> Result<(), String> {
        let a = agent()?;
        let off = offset();
        let (today, _) = local(now(), off);
        let (from, to) = ((today - 1) * 86_400 - off, (today + 62) * 86_400 - off);
        let cals = google::calendars(a)?;
        let mut events = Vec::new();
        for c in cals.iter().filter(|c| if read.is_empty() { c.selected } else { read.contains(&c.id) }).take(10) {
            for (id, title, start, end, all_day) in google::events(a, &c.id, from, to, off)? {
                events.push(CalEvent { uid: id, title, start, end, all_day, cal: c.name.clone() });
            }
        }
        // Tasks are optional: a project without the Tasks API still gets calendars.
        let fetched = (|| -> Result<_, String> {
            let lists = google::task_lists(a)?;
            let mut tasks = Vec::new();
            let mut full = HashSet::new();
            for (id, _) in lists.iter().take(10) {
                let got = google::tasks(a, id)?;
                if got.len() < google::TASKS_PAGE {
                    full.insert(id.clone());
                }
                tasks.extend(got);
            }
            Ok((lists, tasks, full))
        })();
        let (lists, tasks, full, tasks_error) = match fetched {
            Ok((l, t, f)) => (l, t, f, String::new()),
            Err(e) => {
                let (l, t, f) = with(|s| (s.lists.clone(), s.tasks.clone(), s.full_lists.clone()));
                (l, t, f, format!("Задачи Google: {e}"))
            }
        };
        eprintln!("google: {} calendars, {} events, {} lists, {} open tasks", cals.len(), events.len(), lists.len(), tasks.len());
        // Same numbers as before → not worth a journal line (it would flood out
        // phrases); log only changes and errors.
        let changed = with(|s| s.last_counts != (cals.len(), events.len(), lists.len(), tasks.len()));
        if changed {
            crate::log::add(&crate::state_dir(), "info", &format!(
                "Google обновлён: календарей {}, событий {}, списков {}, открытых задач {}",
                cals.len(), events.len(), lists.len(), tasks.len()
            ));
            with(|s| s.last_counts = (cals.len(), events.len(), lists.len(), tasks.len()));
        }
        if let Some(dir) = DIR.lock().unwrap().clone() {
            save_cache(&dir, &events, &tasks);
        }
        with(|s| {
            s.cals = cals; s.lists = lists; s.events = events; s.tasks = tasks;
            s.full_lists = full; s.error = tasks_error; s.fetched = now();
        });
        Ok(())
    })();
    if let Err(e) = result {
        eprintln!("google: {e}");
        crate::log::add(&crate::state_dir(), "error", &format!("Google: {e}"));
        with(|s| s.error = e);
    }
}

// ------------------------------------------------------------------ writing

pub fn add_event(title: &str, start: i64, end: i64, all_day: bool) -> Result<String, String> {
    let cal = with(|s| s.write.clone());
    let id = google::add_event(agent()?, &cal, title, start, end, all_day, offset())?;
    let name = with(|s| {
        let name = s.cals.iter().find(|c| c.id == cal || (cal == "primary" && c.primary)).map_or("Google".into(), |c| c.name.clone());
        s.events.push(CalEvent { uid: id.clone(), title: title.to_owned(), start, end, all_day, cal: name.clone() });
        name
    });
    wake();
    Ok(name)
}

pub fn add_task(title: &str, due_day: Option<i64>, notes: &str) -> Result<(String, String, String), String> {
    let list = with(|s| if s.list.is_empty() {
        s.lists.first().map(|l| l.0.clone()).unwrap_or_else(|| "@default".into())
    } else { s.list.clone() });
    let id = google::add_task(agent()?, &list, title, due_day, notes)?;
    with(|s| s.tasks.push(GTask { id: id.clone(), list: list.clone(), title: title.to_owned(), notes: notes.to_owned(), due_day }));
    wake();
    let name = with(|s| s.lists.iter().find(|l| l.0 == list).map_or("Google Задачи".into(), |l| l.1.clone()));
    Ok((name, list, id))
}

pub fn complete_task(list: &str, id: &str) -> Result<(), String> {
    google::complete_task(agent()?, list, id)?;
    with(|s| s.tasks.retain(|t| !(t.list == list && t.id == id)));
    wake();
    Ok(())
}

pub fn delete_task(list: &str, id: &str) -> Result<(), String> {
    google::delete_task(agent()?, list, id)?;
    with(|s| s.tasks.retain(|t| !(t.list == list && t.id == id)));
    wake();
    Ok(())
}

pub fn update_task(list: &str, id: &str, title: &str, due_day: Option<i64>, notes: &str) -> Result<(), String> {
    google::update_task(agent()?, list, id, title, due_day, notes)?;
    with(|s| {
        if let Some(t) = s.tasks.iter_mut().find(|t| t.list == list && t.id == id) {
            t.title = title.to_owned(); t.due_day = due_day; t.notes = notes.to_owned();
        }
    });
    wake();
    Ok(())
}

/// Loads the cache, refreshes every 5 minutes, and wakes on demand.
pub fn run(dir: &Path) {
    *DIR.lock().unwrap() = Some(dir.to_owned());
    load_cache(dir);
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(10));
        let wanted = WAKE.swap(false, std::sync::atomic::Ordering::Acquire);
        let due = with(|s| now().saturating_sub(s.fetched) >= 300);
        if wanted || due {
            refresh();
        }
    });
}
