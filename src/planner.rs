//! YATV planner: tasks and reminders by voice and in the panel.
//!
//! «Алиса, напомни через 10 минут выключить плиту», «добавь задачу купить
//! молоко», «какие планы на сегодня», «отметь выполненной купить молоко».
//! Taken from Alice only for phrases we own (parse.rs); all other phrases are
//! left to the stock assistant. Stored in <state dir>/planner.json, written
//! atomically (tmp + rename).
//!
//! Times are local: the speaker's clock is UTC, the offset comes from the
//! settings (the page fills it in from the phone). The Google calendar/tasks
//! layer (calendar.rs / google.rs) is added separately; this module only
//! keeps local tasks and the day's spoken summaries.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
    sync::Mutex,
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Task {
    pub id: u64,
    pub title: String,
    /// Unix time, UTC; None = no date.
    #[serde(default)]
    pub due: Option<i64>,
    /// "" | "daily" | "weekdays" | "weekly" | "monthly"
    #[serde(default)]
    pub repeat: String,
    #[serde(default)]
    pub category: String,
    /// "low" | "normal" | "high"
    #[serde(default = "normal")]
    pub priority: String,
    #[serde(default)]
    pub done: bool,
    /// Say it at `due`.
    #[serde(default)]
    pub remind: bool,
    #[serde(default)]
    pub created: i64,
}

fn normal() -> String {
    "normal".into()
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

static TASKS: Mutex<Option<Vec<Task>>> = Mutex::new(None);
static DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
/// Local time offset from UTC, minutes (Yekaterinburg = 180).
static OFFSET: AtomicI32 = AtomicI32::new(180);
pub static ENABLED: AtomicBool = AtomicBool::new(true);

/// The planner's settings (the panel's «Настройки планировщика»).
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// Warn this many minutes before a deadline (0 = no).
    pub warn: u16,
    /// A deadline with a date only is at this minute of the day (1440 = none).
    pub due_time: u16,
    /// A daily morning brief at `brief_at`.
    pub brief_on: bool,
    pub brief_at: u16,
    /// "all" | "weekdays".
    pub brief_days: String,
}

impl Default for Plan {
    fn default() -> Self {
        Plan { warn: 30, due_time: 18 * 60, brief_on: false, brief_at: 8 * 60, brief_days: "all".into() }
    }
}

static PLAN: Mutex<Option<Plan>> = Mutex::new(None);

pub fn configure_plan(p: Plan) {
    *PLAN.lock().unwrap() = Some(p);
}

fn plan() -> Plan {
    PLAN.lock().unwrap().clone().unwrap_or_default()
}

pub fn configure(enabled: bool, offset_min: i32) {
    ENABLED.store(enabled, Ordering::Release);
    OFFSET.store(offset_min.clamp(-720, 840), Ordering::Release);
}

pub fn offset() -> i64 {
    i64::from(OFFSET.load(Ordering::Acquire)) * 60
}

fn file(dir: &Path) -> PathBuf {
    dir.join("planner.json")
}

fn with<T>(f: impl FnOnce(&mut Vec<Task>) -> T) -> T {
    let mut g = TASKS.lock().unwrap();
    f(g.get_or_insert_with(Vec::new))
}

/// One writer at a time: reminders, the panel and voice all save.
static SAVING: Mutex<()> = Mutex::new(());

fn save() {
    let Some(dir) = DIR.lock().unwrap().clone() else { return };
    let _one = SAVING.lock().unwrap_or_else(|e| e.into_inner());
    let data = with(|l| serde_json::to_vec_pretty(l).unwrap_or_default());
    let tmp = dir.join("planner.json.tmp");
    if std::fs::write(&tmp, &data).is_ok() {
        let _ = std::fs::rename(tmp, file(&dir));
    }
}

// ------------------------------------------------------------------ dates

/// Days since 1970-01-01 → (year, month, day).
pub fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Local day number and seconds into it.
pub fn local(t: i64, off: i64) -> (i64, i64) {
    let l = t + off;
    (l.div_euclid(86_400), l.rem_euclid(86_400))
}

/// Monday = 0.
pub fn weekday(day: i64) -> i64 {
    (day + 3).rem_euclid(7)
}

pub fn next_due(due: i64, repeat: &str, off: i64) -> Option<i64> {
    let (day, secs) = local(due, off);
    let at = |d: i64| d * 86_400 + secs - off;
    Some(match repeat {
        "daily" => at(day + 1),
        "weekly" => at(day + 7),
        "weekdays" => {
            let mut d = day + 1;
            while weekday(d) >= 5 {
                d += 1;
            }
            at(d)
        }
        "monthly" => {
            let (y, m, dd) = civil(day);
            let (y2, m2) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
            let dim = (days_from_civil(if m2 == 12 { y2 + 1 } else { y2 }, if m2 == 12 { 1 } else { m2 + 1 }, 1)
                - days_from_civil(y2, m2, 1)) as u32;
            at(days_from_civil(y2, m2, dd.min(dim)))
        }
        _ => return None,
    })
}

// ---------------------------------------------------------------- Russian

fn num(w: &str) -> Option<i64> {
    if let Ok(n) = w.parse::<i64>() {
        return Some(n);
    }
    Some(match w {
        "одну" | "одна" | "один" | "час" => 1,
        "две" | "два" => 2,
        "три" => 3,
        "четыре" => 4,
        "пять" => 5,
        "шесть" => 6,
        "семь" => 7,
        "восемь" => 8,
        "девять" => 9,
        "десять" => 10,
        "пятнадцать" => 15,
        "двадцать" => 20,
        "тридцать" => 30,
        "сорок" => 40,
        _ => return None,
    })
}

const WEEKDAYS: [&str; 7] = ["понедельник", "вторник", "сред", "четверг", "пятниц", "суббот", "воскресень"];
const MONTHS_GEN: [&str; 12] = ["январ", "феврал", "март", "апрел", "ма", "июн", "июл", "август", "сентябр", "октябр", "ноябр", "декабр"];

fn days_in_month(y: i64, m: u32) -> u32 {
    let (y2, m2) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    (days_from_civil(y2, m2, 1) - days_from_civil(y, m, 1)) as u32
}

fn month(w: &str) -> Option<u32> {
    if w == "мая" || w == "май" {
        return Some(5);
    }
    MONTHS_GEN.iter().position(|m| *m != "ма" && w.starts_with(m)).map(|i| i as u32 + 1)
}

/// What was said about time, in full.
#[derive(Debug, Clone, PartialEq)]
pub struct When {
    pub due: i64,
    pub repeat: String,
    pub title: String,
    /// A clock time or «через …» was said (else only a day: 9:00 is assumed).
    pub has_time: bool,
}

pub fn parse_when(text: &str, now: i64, off: i64) -> Option<(i64, String, String)> {
    parse_when_full(text, now, off).map(|w| (w.due, w.repeat, w.title))
}

/// «12 марта», «через 10 минут», «завтра в 9» → the moment and the title without time words.
pub fn parse_when_full(text: &str, now: i64, off: i64) -> Option<When> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let (today, now_secs) = local(now, off);
    let mut used = vec![false; words.len()];
    let mut day: Option<i64> = None;
    let mut secs: Option<i64> = None;
    let mut repeat = String::new();
    let mut relative: Option<i64> = None;
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        let take = |n: usize, used: &mut Vec<bool>| (i..(i + n).min(words.len())).for_each(|k| used[k] = true);
        match w {
            "через" => {
                let mut total = 0i64;
                let mut j = i + 1;
                while j < words.len() {
                    let (n, unit_at) = match num(words[j]) {
                        Some(n) if words[j] != "час" => (n, j + 1),
                        _ => (1, j),
                    };
                    let unit = words.get(unit_at).copied().unwrap_or("");
                    let mult = if unit.starts_with("минут") || unit == "мин" {
                        60
                    } else if unit.starts_with("час") {
                        3600
                    } else if unit.starts_with("полчас") {
                        total += 1800;
                        j = unit_at + 1;
                        continue;
                    } else if unit.starts_with("секунд") {
                        1
                    } else if unit.starts_with("день") || unit.starts_with("дня") || unit.starts_with("дней") {
                        86_400
                    } else if unit.starts_with("недел") {
                        7 * 86_400
                    } else {
                        break;
                    };
                    total = total.saturating_add(n.clamp(0, 100_000).saturating_mul(mult));
                    j = unit_at + 1;
                    if words.get(j) == Some(&"и") {
                        j += 1;
                    }
                }
                if total > 0 {
                    relative = Some(total);
                    take(j - i, &mut used);
                    i = j;
                    continue;
                }
            }
            "сегодня" => {
                day = Some(today);
                take(1, &mut used);
            }
            "утром" | "днем" | "днём" | "вечером" | "ночью" if secs.is_none() => {
                secs = Some(match w {
                    "утром" => 9,
                    "вечером" => 19,
                    "ночью" => 23,
                    _ => 13,
                } * 3600);
                take(1, &mut used);
            }
            "на" if words.get(i + 1).is_some_and(|n| WEEKDAYS.iter().any(|d| n.starts_with(d))) => {
                let next = words[i + 1];
                let wd = WEEKDAYS.iter().position(|d| next.starts_with(d)).unwrap_or(0);
                let mut delta = (wd as i64 - weekday(today)).rem_euclid(7);
                if delta == 0 { delta = 7; }
                day = Some(today + delta);
                take(2, &mut used);
            }
            "на" if words.get(i + 1) == Some(&"следующей") && words.get(i + 2).is_some_and(|x| x.starts_with("недел")) => {
                day = Some(today - weekday(today) + 7);
                take(3, &mut used);
            }
            "завтра" => {
                day = Some(today + 1);
                take(1, &mut used);
            }
            "послезавтра" => {
                day = Some(today + 2);
                take(1, &mut used);
            }
            "каждый" | "каждую" | "каждое" | "по" | "ежедневно" => {
                let next = words.get(i + 1).copied().unwrap_or("");
                if w == "ежедневно" || next == "день" {
                    repeat = "daily".into();
                    take(if w == "ежедневно" { 1 } else { 2 }, &mut used);
                } else if next.starts_with("будн") {
                    repeat = "weekdays".into();
                    take(2, &mut used);
                } else if next == "неделю" {
                    repeat = "weekly".into();
                    take(2, &mut used);
                } else if next == "месяц" {
                    repeat = "monthly".into();
                    take(2, &mut used);
                } else if let Some(wd) = WEEKDAYS.iter().position(|d| next.starts_with(d)) {
                    repeat = "weekly".into();
                    let delta = (wd as i64 - weekday(today)).rem_euclid(7);
                    day = Some(today + delta);
                    take(2, &mut used);
                }
            }
            _ if w.parse::<u32>().is_ok_and(|d| (1..=31).contains(&d)) && words.get(i + 1).and_then(|m| month(m)).is_some() => {
                let d = w.parse::<u32>().unwrap_or(1);
                let m = month(words[i + 1]).unwrap_or(1);
                let (y, _, _) = civil(today);
                let found = (y..y + 9).find(|&yy| d <= days_in_month(yy, m) && days_from_civil(yy, m, d) >= today);
                if let Some(yy) = found {
                    day = Some(days_from_civil(yy, m, d));
                    take(2, &mut used);
                }
                i += 2;
                continue;
            }
            "в" | "во" | "к" | "до" if words.get(i + 1).is_some_and(|d| d.parse::<u32>().is_ok()) && words.get(i + 2).and_then(|m| month(m)).is_some() => {
                take(1, &mut used);
            }
            "в" | "во" | "к" | "до" => {
                let next = words.get(i + 1).copied().unwrap_or("");
                if let Some(wd) = WEEKDAYS.iter().position(|d| next.starts_with(d)) {
                    let mut delta = (wd as i64 - weekday(today)).rem_euclid(7);
                    if delta == 0 {
                        delta = 7;
                    }
                    day = Some(today + delta);
                    take(2, &mut used);
                } else if let Some((h, m)) = clock(next) {
                    let (mut h, mut m) = (h, m);
                    let mut n = 2;
                    if !next.contains([':', '.']) {
                        if let Some(mm) = words.get(i + 2).filter(|w| w.len() == 2 || **w == "0").and_then(|w| w.parse::<i64>().ok()).filter(|mm| *mm < 60) {
                            m = mm;
                            n = 3;
                        }
                    }
                    if words.get(i + n).is_some_and(|w| ["час", "часа", "часов"].contains(&w)) {
                        n += 1;
                    }
                    let after = words.get(i + n).copied().unwrap_or("");
                    if after == "вечера" || after == "дня" {
                        if h < 12 {
                            h += 12;
                        }
                        n += 1;
                    } else if after == "утра" || after == "ночи" {
                        if h == 12 {
                            h = 0;
                        }
                        n += 1;
                    }
                    secs = Some(h * 3600 + m * 60);
                    take(n, &mut used);
                }
            }
            _ => {}
        }
        i += 1;
    }
    if let (Some(r), Some(_), None) = (relative, secs, day) {
        if r >= 86_400 && r % 86_400 == 0 {
            day = Some(today + r / 86_400);
            relative = None;
        }
    }
    let has_time = relative.is_some() || secs.is_some();
    let due = if let Some(r) = relative {
        now + r
    } else if day.is_some() || secs.is_some() {
        let s = secs.unwrap_or(9 * 3600);
        let mut d = day.unwrap_or(today);
        if day.is_none() && s <= now_secs {
            d += 1;
        }
        d * 86_400 + s - off
    } else {
        return None;
    };
    let title: String = words
        .iter()
        .zip(&used)
        .filter(|(_, u)| !**u)
        .map(|(w, _)| *w)
        .collect::<Vec<_>>()
        .join(" ");
    let title = title.trim_start_matches("что ").trim_start_matches("о ").trim_start_matches("про ").trim().to_owned();
    Some(When { due, repeat, title: if title.is_empty() { "напоминание".into() } else { title }, has_time })
}

/// «18:30», «18.30», «9», «18 30» → (hours, minutes).
fn clock(w: &str) -> Option<(i64, i64)> {
    let w = w.trim_end_matches('.');
    if let Some((h, m)) = w.split_once([':', '.']) {
        let (h, m) = (h.parse().ok()?, m.parse().ok()?);
        return (h < 24 && m < 60).then_some((h, m));
    }
    let h = num(w)?;
    (0..24).contains(&h).then_some((h, 0))
}

// ------------------------------------------------------------------ tasks

pub fn add(title: &str, due: Option<i64>, repeat: &str, remind: bool, category: &str, priority: &str) -> Task {
    let mut t = Task {
        id: 0,
        title: title.trim().chars().take(200).collect(),
        due,
        repeat: if ["daily", "weekdays", "weekly", "monthly"].contains(&repeat) { repeat.into() } else { String::new() },
        category: category.trim().chars().take(40).collect(),
        priority: if ["low", "high"].contains(&priority) { priority.into() } else { "normal".into() },
        done: false,
        remind,
        created: now(),
    };
    with(|l| {
        t.id = l.iter().map(|t| t.id).max().unwrap_or(0) + 1;
        l.push(t.clone());
    });
    save();
    t
}

fn norm(s: &str) -> String {
    s.to_lowercase().replace('ё', "е")
}

/// An open reminder with the same title at the same minute already exists.
fn duplicate(title: &str, due: i64) -> bool {
    let t = norm(title);
    let minute = due / 60;
    with(|l| l.iter().any(|x| !x.done && x.remind && norm(&x.title) == t
        && x.due.is_some_and(|d| d / 60 == minute)))
}

/// Best match for a spoken title among open tasks.
fn find(title: &str) -> Option<u64> {
    let t = norm(title);
    with(|l| {
        let open: Vec<Task> = l.iter().filter(|x| !x.done).cloned().collect();
        open.iter()
            .find(|x| norm(&x.title) == t)
            .or_else(|| open.iter().find(|x| norm(&x.title).contains(&t) || t.contains(&norm(&x.title))))
            .or_else(|| open.iter().find(|x| t.split_whitespace().all(|w| norm(&x.title).contains(w))))
            .map(|x| x.id)
    })
}

pub fn set_done(id: u64, done: bool) -> bool {
    let ok = with(|l| match l.iter_mut().find(|t| t.id == id) {
        Some(t) => {
            match (done, t.due.and_then(|d| next_due(d, &t.repeat, offset()))) {
                (true, Some(next)) => t.due = Some(next),
                _ => t.done = done,
            }
            true
        }
        None => false,
    });
    save();
    ok
}

pub fn edit(id: u64, title: &str, due: Option<i64>, repeat: &str, priority: &str) -> Option<Task> {
    let title: String = title.trim().chars().take(200).collect();
    if title.is_empty() {
        return None;
    }
    let t = with(|l| {
        let t = l.iter_mut().find(|t| t.id == id)?;
        t.title = title;
        t.due = due;
        t.repeat = if ["daily", "weekdays", "weekly", "monthly"].contains(&repeat) && due.is_some() { repeat.into() } else { String::new() };
        t.priority = if ["low", "high"].contains(&priority) { priority.into() } else { "normal".into() };
        Some(t.clone())
    })?;
    save();
    Some(t)
}

pub fn delete(id: u64) -> bool {
    let ok = with(|l| {
        let before = l.len();
        l.retain(|t| t.id != id);
        before != l.len()
    });
    save();
    ok
}

// ---------------------------------------------------------------- summary

pub fn plural(n: usize, one: &str, few: &str, many: &str) -> String {
    let w = match (n % 10, n % 100) {
        (1, x) if x != 11 => one,
        (2..=4, x) if !(12..=14).contains(&x) => few,
        _ => many,
    };
    format!("{n} {w}")
}

pub fn in_minutes(m: i64) -> String {
    match m {
        ..=1 => "Через минуту".into(),
        30 => "Через полчаса".into(),
        60 => "Через час".into(),
        90 => "Через полтора часа".into(),
        120 => "Через два часа".into(),
        m => {
            let (n10, n100) = (m % 10, m % 100);
            let w = if n10 == 1 && n100 != 11 {
                "минуту"
            } else if (2..=4).contains(&n10) && !(12..=14).contains(&n100) {
                "минуты"
            } else {
                "минут"
            };
            format!("Через {m} {w}")
        }
    }
}

const WEEKDAY_NAMES: [&str; 7] = ["понедельник", "вторник", "среда", "четверг", "пятница", "суббота", "воскресенье"];
const WEEKDAY_ON: [&str; 7] = ["в понедельник", "во вторник", "в среду", "в четверг", "в пятницу", "в субботу", "в воскресенье"];
const MONTHS: [&str; 12] = ["января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября", "декабря"];

pub fn hm(t: i64, off: i64) -> String {
    let (_, s) = local(t, off);
    format!("{}:{:02}", s / 3600, s / 60 % 60)
}

fn day_head(day: i64, today: i64) -> String {
    let (_, m, d) = civil(day);
    let wd = WEEKDAY_NAMES[weekday(day) as usize];
    let date = format!("{d} {}", MONTHS[(m - 1) as usize]);
    match day - today {
        0 => format!("Сегодня {wd}, {date}"),
        1 => format!("Завтра {wd}, {date}"),
        _ => {
            let mut c = wd.chars();
            format!("{}{}, {date}", c.next().map(|x| x.to_uppercase().collect::<String>()).unwrap_or_default(), c.as_str())
        }
    }
}

fn day_word(day: i64, today: i64) -> String {
    match day - today {
        0 => "сегодня".into(),
        1 => "завтра".into(),
        2 => "послезавтра".into(),
        3..=6 => WEEKDAY_ON[weekday(day) as usize].into(),
        _ => {
            let (_, m, d) = civil(day);
            format!("{d} {}", MONTHS[(m - 1) as usize])
        }
    }
}

/// «19:30», «завтра в 9:00», «12 марта в 18:00».
pub fn say_time(t: i64, now: i64, off: i64) -> String {
    let (d, s) = local(t, off);
    let (today, _) = local(now, off);
    let hm = format!("{}:{:02}", s / 3600, s / 60 % 60);
    match d - today {
        0 => format!("в {hm}"),
        1 => format!("завтра в {hm}"),
        2 => format!("послезавтра в {hm}"),
        _ => {
            let (_, m, dd) = civil(d);
            format!("{dd} {} в {hm}", MONTHS[(m - 1) as usize])
        }
    }
}

/// A time in a task's notes: «до 18:00», «к 9».
pub fn deadline_time(text: &str) -> Option<i64> {
    let t = text.to_lowercase();
    let words: Vec<&str> = t.split(|c: char| c.is_whitespace() || c == ',' || c == '(' || c == ')').filter(|w| !w.is_empty()).collect();
    for (i, w) in words.iter().enumerate() {
        let w = w.trim_end_matches(['.', ';', '!']);
        if let Some((h, m)) = w.split_once([':', '.']) {
            if let (Ok(h), Ok(m)) = (h.parse::<i64>(), m.parse::<i64>()) {
                if h < 24 && m < 60 && m.to_string().len() <= 2 {
                    return Some(h * 3600 + m * 60);
                }
            }
        }
        if i > 0 && ["до", "к", "в"].contains(&words[i - 1]) {
            if let Ok(h) = w.parse::<i64>() {
                if (0..24).contains(&h) {
                    let pm = words.get(i + 1).is_some_and(|n| *n == "вечера" || *n == "дня") && h < 12;
                    return Some((h + if pm { 12 } else { 0 }) * 3600);
                }
            }
        }
    }
    None
}

/// An event in the day's timeline (iCal/Google events, Google tasks).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Event {
    pub uid: String,
    pub title: String,
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub cal: String,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub kind: &'static str,
    pub title: String,
    pub at: Option<i64>,
    pub end: Option<i64>,
    pub all_day: bool,
    pub day: Option<i64>,
    pub source: String,
    pub id: String,
    pub list: String,
    pub priority: String,
}

impl Item {
    fn spoken(&self, off: i64) -> String {
        match (self.kind, self.at) {
            (_, _) if self.all_day => format!("{} (весь день)", self.title),
            ("reminder", Some(t)) => format!("в {} {}", hm(t, off), self.title),
            (_, Some(t)) => format!("{} до {}", self.title, hm(t, off)),
            _ => self.title.clone(),
        }
    }
}

fn local_items(from: i64, to: i64, off: i64) -> Vec<Item> {
    with(|l| {
        l.iter()
            .filter(|t| !t.done && t.due.is_some())
            .filter_map(|t| {
                let due = t.due?;
                let d = local(due, off).0;
                (d >= from && d < to).then(|| Item {
                    kind: if t.remind { "reminder" } else { "task" },
                    title: t.title.clone(),
                    at: Some(due),
                    end: None,
                    all_day: false,
                    day: Some(d),
                    source: "local".into(),
                    id: t.id.to_string(),
                    list: String::new(),
                    priority: t.priority.clone(),
                })
            })
            .collect()
    })
}

pub fn collect(from: i64, to: i64, off: i64) -> Vec<Item> {
    let mut out = Vec::new();
    for e in crate::calendar::between(from * 86_400 - off, to * 86_400 - off) {
        let d = local(e.start, off).0;
        out.push(Item {
            kind: "event",
            title: e.title.clone(),
            at: Some(e.start),
            end: Some(e.end),
            all_day: e.all_day,
            day: Some(d.max(from)),
            source: e.cal.clone(),
            id: e.uid.clone(),
            list: String::new(),
            priority: "normal".into(),
        });
    }
    out.extend(local_items(from, to, off));
    for g in crate::calendar::google_tasks() {
        let Some(d) = g.due_day else { continue };
        if d >= from && d < to {
            out.push(Item {
                kind: "task",
                title: g.title.clone(),
                at: deadline_time(&format!("{} {}", g.title, g.notes)).map(|s| d * 86_400 + s - off),
                end: None,
                all_day: false,
                day: Some(d),
                source: "google".into(),
                id: g.id.clone(),
                list: g.list.clone(),
                priority: "normal".into(),
            });
        }
    }
    out.sort_by_key(|i| (i.day, !(i.kind == "event" && i.all_day), i.at.unwrap_or(i64::MAX), i.kind != "event"));
    out
}

pub fn overdue(now: i64, off: i64) -> Vec<Item> {
    let mut out: Vec<Item> = with(|l| {
        l.iter()
            .filter(|t| !t.done && !t.remind && t.due.is_some_and(|d| d < now))
            .map(|t| Item {
                kind: "task",
                title: t.title.clone(),
                at: t.due,
                end: None,
                all_day: false,
                day: t.due.map(|d| local(d, off).0),
                source: "local".into(),
                id: t.id.to_string(),
                list: String::new(),
                priority: t.priority.clone(),
            })
            .collect()
    });
    let (today, _) = local(now, off);
    for g in crate::calendar::google_tasks() {
        let Some(d) = g.due_day else { continue };
        let at = deadline_time(&format!("{} {}", g.title, g.notes)).map(|s| d * 86_400 + s - off);
        if d < today || at.is_some_and(|a| a < now) {
            out.push(Item {
                kind: "task", title: g.title.clone(), at, end: None, all_day: false,
                day: Some(d), source: "google".into(), id: g.id.clone(),
                list: g.list.clone(), priority: "normal".into(),
            });
        }
    }
    out
}

pub fn undated() -> Vec<Item> {
    let mut out: Vec<Item> = with(|l| {
        l.iter()
            .filter(|t| !t.done && t.due.is_none())
            .map(|t| Item {
                kind: "task", title: t.title.clone(), at: None, end: None, all_day: false,
                day: None, source: "local".into(), id: t.id.to_string(),
                list: String::new(), priority: t.priority.clone(),
            })
            .collect()
    });
    for g in crate::calendar::google_tasks().into_iter().filter(|g| g.due_day.is_none()) {
        out.push(Item {
            kind: "task", title: g.title.clone(), at: None, end: None, all_day: false,
            day: None, source: "google".into(), id: g.id.clone(), list: g.list.clone(),
            priority: "normal".into(),
        });
    }
    out
}

fn list_of(items: &[String], max: usize) -> String {
    let mut parts = items.iter().take(max).cloned().collect::<Vec<_>>();
    if items.len() > max {
        parts.push(format!("и ещё {}", plural(items.len() - max, "одно", "два", "")));
    }
    parts.join(", ")
}

/// «Какие планы», «что нужно сделать»,расписание», дедлайны и т.д.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ask {
    Plans(i64),
    Todo(i64),
    Schedule(i64),
    Next,
    Deadlines,
    Week,
    /// Monday..Sunday of the next calendar week (not just the next 7 days).
    NextWeek,
    Brief,
}

pub fn agenda_text(ask: Ask, now: i64, off: i64) -> String {
    let (today, _) = local(now, off);
    match ask {
        Ask::Plans(d) | Ask::Todo(d) | Ask::Schedule(d) => {
            let day = today + d;
            let items = collect(day, day + 1, off);
            // «Сейчас» filters only timed items: an all-day event is the whole
            // day, and a dateless one has no moment to pass.
            let tasks: Vec<&Item> = items.iter()
                .filter(|i| d != 0 || i.all_day || i.at.is_none_or(|a| a >= now))
                // «Что нужно сделать» is about tasks, not the calendar.
                .filter(|i| !matches!(ask, Ask::Todo(_)) || i.kind != "event")
                .collect();
            let late: Vec<Item> = if d == 0 { overdue(now, off).into_iter().filter(|i| i.day < Some(today) || tasks.iter().all(|t| t.id != i.id)).collect() } else { vec![] };
            let mut out = vec![day_head(day, today)];
            let tk = |out: &mut Vec<String>| {
                if tasks.is_empty() && late.is_empty() {
                    out.push(if d == 0 { "Задач на сегодня нет".into() } else { "Задач нет".into() });
                    return;
                }
                if !tasks.is_empty() {
                    out.push(list_of(&tasks.iter().map(|t| t.spoken(off)).collect::<Vec<_>>(), 8));
                }
                if !late.is_empty() {
                    out.push(format!("Просрочено: {}", list_of(&late.iter().map(|t| t.title.clone()).collect::<Vec<_>>(), 4)));
                }
            };
            match ask {
                Ask::Schedule(_) => {
                    let timed: Vec<&Item> = tasks.iter().filter(|t| t.at.is_some()).copied().collect();
                    if timed.is_empty() {
                        out.push(if d == 0 { "Расписание на сегодня свободно".into() } else { "Расписание свободно".into() });
                    } else {
                        out.push(format!("Расписание: {}", list_of(&timed.iter().map(|i| i.spoken(off)).collect::<Vec<_>>(), 10)));
                    }
                }
                Ask::Todo(_) => {
                    tk(&mut out);
                }
                _ => {
                    if tasks.is_empty() && late.is_empty() {
                        out.push(if d == 0 { "Планов нет, день свободен".into() } else { "Планов нет".into() });
                    } else {
                        tk(&mut out);
                    }
                }
            }
            format!("{}.", out.join(". "))
        }
        Ask::Next => {
            let items = collect(today, today + 8, off);
            match items.iter().filter(|i| i.at.is_some_and(|a| a > now)).min_by_key(|i| i.at) {
                Some(i) => {
                    let at = i.at.unwrap_or(now);
                    let left = (at - now + 30) / 60;
                    if local(at, off).0 == today && left < 180 {
                        format!("Дальше в {} {} — {}.", hm(at, off), i.title, in_minutes(left).to_lowercase())
                    } else {
                        format!("Следующее — {} в {}: {}.", day_word(local(at, off).0, today), hm(at, off), i.title)
                    }
                }
                None => "Ближайших планов со временем нет.".into(),
            }
        }
        Ask::Deadlines => {
            let late = overdue(now, off);
            let soon: Vec<Item> = collect(today, today + 8, off).into_iter().filter(|i| i.at.is_some_and(|a| a >= now)).collect();
            if late.is_empty() && soon.is_empty() {
                return "На неделе сроков нет.".into();
            }
            let mut parts = Vec::new();
            if !late.is_empty() {
                parts.push(format!("Просрочено: {}", list_of(&late.iter().map(|t| t.title.clone()).collect::<Vec<_>>(), 4)));
            }
            if !soon.is_empty() {
                let lines: Vec<String> = soon.iter().map(|t| format!("{} — {}", day_word(t.day.unwrap_or(today), today), t.title)).collect();
                parts.push(format!("Сроки: {}", list_of(&lines, 6)));
            }
            format!("{}.", parts.join(". "))
        }
        Ask::NextWeek => {
            // Next Monday, not «today + 7».
            let monday = today - weekday(today) + 7;
            let sunday = monday + 6;
            let items = collect(monday, sunday + 1, off);
            let mut parts = Vec::new();
            for day in monday..=sunday {
                let of: Vec<&Item> = items.iter().filter(|i| i.day == Some(day)).collect();
                if of.is_empty() {
                    continue;
                }
                // Always name the weekday here, even beyond the next 7 days.
                let w = WEEKDAY_ON[weekday(day) as usize].to_owned();
                let mut c = w.chars();
                let w = format!("{}{}", c.next().map(|x| x.to_uppercase().collect::<String>()).unwrap_or_default(), c.as_str());
                parts.push(format!("{w}: {}", list_of(&of.iter().map(|i| i.title.clone()).collect::<Vec<_>>(), 3)));
            }
            if parts.is_empty() {
                "На следующей неделе планов нет.".into()
            } else {
                format!("На следующей неделе. {}.", parts.join(". "))
            }
        }
        Ask::Week => {
            let items = collect(today, today + 7, off);
            let mut parts = Vec::new();
            for day in today..today + 7 {
                let of: Vec<&Item> = items.iter().filter(|i| i.day == Some(day)).collect();
                if of.is_empty() {
                    continue;
                }
                let w = day_word(day, today);
                let mut c = w.chars();
                let w = format!("{}{}", c.next().map(|x| x.to_uppercase().collect::<String>()).unwrap_or_default(), c.as_str());
                parts.push(format!("{w}: {}", list_of(&of.iter().map(|i| i.title.clone()).collect::<Vec<_>>(), 3)));
            }
            if parts.is_empty() {
                "На неделе планов нет.".into()
            } else {
                format!("На неделе. {}.", parts.join(". "))
            }
        }
        Ask::Brief => {
            let mut s = format!("Доброе утро! {}", agenda_text(Ask::Plans(0), now, off));
            let tomorrow: Vec<String> = collect(today + 1, today + 2, off).into_iter().map(|i| i.title).collect();
            if !tomorrow.is_empty() {
                s.push_str(&format!(" Завтра: {}.", list_of(&tomorrow, 3)));
            }
            s
        }
    }
}

pub fn agenda(day: Option<i64>) -> String {
    agenda_text(match day {
        Some(d) => Ask::Plans(d),
        None => Ask::Todo(0),
    }, now(), offset())
}

/// The panel's `plan json` answer.
pub fn json() -> Value {
    let (now, off) = (now(), offset());
    let (today, _) = local(now, off);
    let mut timeline: Vec<Value> = overdue(now, off).iter().map(|i| item_json(i, now, off, true)).collect();
    timeline.extend(collect(today, today + 62, off).iter().map(|i| item_json(i, now, off, false)));
    timeline.extend(undated().iter().map(|i| item_json(i, now, off, false)));
    let mut items: Vec<Task> = with(|l| l.clone());
    items.sort_by_key(|t| (t.done, t.due.unwrap_or(i64::MAX), t.id));
    json!({
        "enabled": ENABLED.load(Ordering::Acquire),
        "offset_min": off / 60,
        "google": crate::calendar::google_on(),
        "plan": {
            "warn": plan().warn, "due_time": plan().due_time, "brief_on": plan().brief_on,
            "brief_at": plan().brief_at, "brief_days": plan().brief_days.clone(), "target": "local",
            "mode": "simple"
        },
        "today": agenda_text(Ask::Plans(0), now, off),
        "items": timeline,
        "tasks": items.iter().map(|t| {
            let mut v = serde_json::to_value(t).unwrap_or_default();
            v["when"] = t.due.map(|d| say_time(d, now, off)).into();
            v["overdue"] = (!t.done && t.due.is_some_and(|d| d < now)).into();
            v
        }).collect::<Vec<_>>(),
    })
}

fn item_json(i: &Item, now: i64, off: i64, late: bool) -> Value {
    let (today, _) = local(now, off);
    // A date without a time (e.g. a Google Task) has no `at`, but the
    // calendar still needs to place it on the right day.
    let day_date = |d: i64| {
        let (y, m, dd) = civil(d);
        format!("{y:04}-{m:02}-{dd:02}")
    };
    json!({
        "kind": i.kind, "title": i.title, "source": i.source, "id": i.id, "list": i.list,
        "at": i.at, "end": i.end, "all_day": i.all_day, "day": i.day.map(|d| d - today),
        "date": i.day.map(day_date),
        "day_word": i.day.map(|d| day_word(d, today)),
        "time": i.at.filter(|_| !i.all_day).map(|a| hm(a, off)),
        "overdue": late, "priority": i.priority,
    })
}

// ------------------------------------------------------------------- voice

#[derive(Debug, Clone, PartialEq)]
pub enum Voice {
    Remind(String),
    AddTask(String),
    AddEvent(String),
    Agenda(Option<i64>),
    Ask(Ask),
    Done(String),
    Delete(String),
    /// «создай задачу в гугле …» — always to Google Tasks.
    AddGoogle(String),
    /// «доброе утро» with the brief switched on.
    Brief,
}

impl Voice {
    pub fn is_query(&self) -> bool {
        matches!(self, Voice::Agenda(_) | Voice::Ask(_) | Voice::Brief)
    }
}

fn without_day(text: &str) -> String {
    const DAYS: [&str; 6] = ["сегодня", "завтра", "послезавтра", "сегодняшний", "завтрашний", "сейчас"];
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut out = Vec::new();
    for (i, w) in words.iter().enumerate() {
        let next_is_day = words.get(i + 1).is_some_and(|n| DAYS.contains(n));
        if DAYS.contains(w) || (*w == "на" && next_is_day) || (out.is_empty() && matches!(*w, "а" | "ну" | "скажи" | "подскажи")) {
            continue;
        }
        out.push(*w);
    }
    out.join(" ")
}

fn asked_day(text: &str, today: i64) -> i64 {
    let w = text.to_lowercase();
    // Only use the full parser for weekdays/calendar dates. For «сегодня/
    // завтра/послезавтра» keep the simple offsets — parse_when_full fills a
    // default clock time and would shift the day depending on the hour.
    let weekday_or_date = WEEKDAYS.iter().any(|d| w.contains(d))
        || ["январ", "феврал", "март", "апрел", "май", "июн", "июл", "август", "сентябр", "октябр", "ноябр", "декабр"].iter().any(|m| w.contains(m));
    if weekday_or_date {
        if let Some(when) = parse_when_full(text, now(), offset()) {
            let (d, _) = local(when.due, offset());
            return d;
        }
    }
    if w.contains("послезавтра") {
        today + 2
    } else if w.contains("завтра") {
        today + 1
    } else {
        today
    }
}

/// The strict planner whitelist. Everything else returns None and stays with Alice.
pub fn voice(text: &str) -> Option<Voice> {
    let text = text.trim();
    for p in ["напомни мне ", "напомни ", "напоминай мне ", "напоминай ", "поставь напоминание ", "создай напоминание "] {
        if let Some(rest) = text.strip_prefix(p) {
            return (!rest.trim().is_empty()).then(|| Voice::Remind(rest.trim().to_owned()));
        }
    }
    for p in ["добавь в календарь ", "запиши в календарь ", "поставь в календарь ", "запланируй ", "создай событие ", "добавь событие "] {
        if let Some(rest) = text.strip_prefix(p) {
            return (!rest.trim().is_empty()).then(|| Voice::AddEvent(rest.trim().to_owned()));
        }
    }
    for p in ["добавь встречу ", "создай встречу ", "назначь встречу "] {
        if let Some(rest) = text.strip_prefix(p) {
            return (!rest.trim().is_empty()).then(|| Voice::AddEvent(format!("встреча {}", rest.trim())));
        }
    }
    for p in [
        "создай задачу в гугле ", "создай задачу в гугл ", "добавь задачу в гугл ", "добавь задачу в гугле ",
        "добавь в гугл задачи ", "запиши в гугл ",
    ] {
        if let Some(rest) = text.strip_prefix(p) {
            return (!rest.trim().is_empty()).then(|| Voice::AddGoogle(rest.trim().to_owned()));
        }
    }
    for p in ["добавь задачу ", "добавь в список дел ", "добавь в планы ", "добавь в задачи ", "запиши задачу ", "новая задача ", "поставь задачу ", "создай задачу "] {
        if let Some(rest) = text.strip_prefix(p) {
            return (!rest.trim().is_empty()).then(|| Voice::AddTask(rest.trim().to_owned()));
        }
    }
    for p in ["добавь дедлайн ", "поставь дедлайн ", "дедлайн "] {
        if let Some(rest) = text.strip_prefix(p) {
            return (!rest.trim().is_empty()).then(|| Voice::AddTask(rest.trim().to_owned()));
        }
    }
    for p in ["удали задачу ", "удали напоминание ", "убери задачу "] {
        if let Some(rest) = text.strip_prefix(p) {
            return (!rest.trim().is_empty()).then(|| Voice::Delete(rest.trim().to_owned()));
        }
    }
    for p in ["отметь выполненной ", "отметь как выполненную ", "отметь выполненным ", "выполнено ", "я сделал ", "я сделала ", "задача выполнена ", "я выполнил ", "я выполнила ", "готово "] {
        if let Some(rest) = text.strip_prefix(p) {
            return (!rest.trim().is_empty()).then(|| Voice::Done(rest.trim().trim_start_matches("задачу ").to_owned()));
        }
    }
    // «Доброе утро» is deliberately NOT intercepted: it belongs to Yandex.
    // The morning brief is available by voice only via «расскажи про мой день»
    // and via the panel's «Сказать вслух» button.
    let (today, _) = local(now(), offset());
    let day = asked_day(text, today);
    let bare = without_day(text);
    let has = |ws: &[&str]| ws.iter().any(|w| text.contains(w) || bare.contains(w));
    if has(&["что у меня дальше", "что дальше по план", "что у меня следующ", "что следующее", "следующая встреча", "следующее событие", "куда мне дальше"]) {
        return Some(Voice::Ask(Ask::Next));
    }
    if has(&["дедлайн", "какие сроки", "какие у меня сроки", "что горит", "что просрочено", "что я просрочил", "что я просрочила"]) {
        return Some(Voice::Ask(Ask::Deadlines));
    }
    if has(&["следующую неделю", "на следующей неделе"]) {
        return Some(Voice::Ask(Ask::NextWeek));
    }
    if has(&["на неделю", "на этой неделе", "на эту неделю", "что на неделе"])
        && has(&["план", "задач", "что", "расписан"]) {
        return Some(Voice::Ask(Ask::Week));
    }
    if has(&["расскажи про мой день", "расскажи о моем дне", "сводка на день", "сводку на день", "утренняя сводка", "как выглядит мой день", "обзор дня"]) {
        return Some(Voice::Ask(Ask::Brief));
    }
    if has(&["расписание", "что у меня в календаре", "что в календаре", "какие встречи", "какие у меня встречи", "какие события"]) {
        return Some(Voice::Ask(Ask::Schedule(day - today)));
    }
    if has(&["нужно сделать", "надо сделать", "должен сделать", "должна сделать", "список дел", "какие задачи", "какие у меня задачи", "мои задачи", "что сделать"]) {
        return Some(Voice::Ask(Ask::Todo(day - today)));
    }
    let plans = ["какие планы", "какие у меня планы", "что у меня сегодня", "что у меня завтра", "что у меня на", "что запланировано", "мои планы", "что по планам", "какие дела", "какие у меня дела"];
    if plans.iter().any(|p| text.starts_with(p) || bare.starts_with(p)) {
        // A phrase like «какие планы жизнь» starts with «какие планы» but is
        // nonsense — it must stay with Alice. Accept only a day/period tail
        // (сегодня, завтра, на понедельник, на этой неделе) or a bare question.
        let matched = plans.iter().find(|p| text.starts_with(**p) || bare.starts_with(**p)).copied().unwrap_or("");
        let tail = text.strip_prefix(matched).unwrap_or("").trim().trim_matches(['?', '!', '.']).trim();
        let tail = tail.strip_prefix("у меня").unwrap_or(tail).trim();
        // Every tail word must be a day/period word. «на завтра кофе» has an
        // unknown word «кофе» and stays with Alice.
        let day_words = ["сегодня", "завтра", "послезавтра", "сейчас", "на", "в", "во", "эту", "этой", "следующ", "недел", "понедельник", "вторник", "сред", "четверг", "пятниц", "суббот", "воскресен", "числ", "месяц", "году", "утром", "днем", "днём", "вечером", "ночью"];
        let tail_ok = tail.split_whitespace().all(|w| {
            let w = w.trim_matches([',', '?', '!', '.']).to_lowercase();
            w.is_empty() || w == "и" || w == "какие" || w == "у" || w == "меня"
                || day_words.iter().any(|d| w.contains(d))
        });
        if tail.is_empty() || tail_ok {
            return Some(Voice::Ask(Ask::Plans(day - today)));
        }
    }
    None
}

fn duration(text: &str) -> (i64, String) {
    let w: Vec<&str> = text.split_whitespace().collect();
    for i in 0..w.len() {
        if w[i] != "на" {
            continue;
        }
        let (secs, n) = match (w.get(i + 1).copied(), w.get(i + 2).copied()) {
            (Some("час"), _) => (3600, 2),
            (Some("полчаса"), _) => (1800, 2),
            (Some("полтора"), Some(u)) if u.starts_with("час") => (5400, 3),
            (Some(x), Some(u)) if num(x).is_some() && u.starts_with("час") => (num(x).unwrap_or(1) * 3600, 3),
            (Some(x), Some(u)) if num(x).is_some() && u.starts_with("минут") => (num(x).unwrap_or(30) * 60, 3),
            _ => continue,
        };
        let rest: Vec<&str> = w.iter().enumerate().filter(|(k, _)| *k < i || *k >= i + n).map(|(_, x)| *x).collect();
        return (secs, rest.join(" "));
    }
    (3600, text.to_owned())
}

fn nominative(title: &str) -> String {
    const WORDS: [(&str, &str); 8] = [
        ("встречу", "встреча"), ("тренировку", "тренировка"), ("поездку", "поездка"), ("планёрку", "планёрка"),
        ("презентацию", "презентация"), ("консультацию", "консультация"), ("игру", "игра"), ("лекцию", "лекция"),
    ];
    let (first, rest) = title.split_once(' ').unwrap_or((title, ""));
    match WORDS.iter().find(|(acc, _)| *acc == first.to_lowercase()) {
        Some((_, nom)) if rest.is_empty() => (*nom).to_owned(),
        Some((_, nom)) => format!("{nom} {rest}"),
        None => title.to_owned(),
    }
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|x| x.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

fn find_google(title: &str) -> Option<crate::google::GTask> {
    let t = norm(title);
    let all = crate::calendar::google_tasks();
    all.iter()
        .find(|x| norm(&x.title) == t)
        .or_else(|| all.iter().find(|x| norm(&x.title).contains(&t) || t.contains(&norm(&x.title))))
        .or_else(|| all.iter().find(|x| t.split_whitespace().all(|w| norm(&x.title).contains(w))))
        .cloned()
}

/// Does what was said; the answer is spoken back through Alice's TTS.
pub fn handle(v: Voice) -> String {
    let (now, off) = (now(), offset());
    let google = crate::calendar::google_on();
    match v {
        Voice::AddGoogle(rest) => {
            if !google {
                return "Google не подключён: подключите его на странице колонки, вкладка «Планы»".into();
            }
            let w = parse_when_full(&rest, now, off);
            let title = cap(&w.as_ref().map_or(rest.clone(), |w| w.title.clone()));
            let day = w.as_ref().map(|w| local(w.due, off).0);
            let notes = w.as_ref().filter(|w| w.has_time).map(|w| format!("до {}", hm(w.due, off))).unwrap_or_default();
            match crate::calendar::add_task(&title, day, &notes) {
                Ok((name, _, _)) => format!("Добавила в Google «{name}»: {title}"),
                Err(e) => format!("Google не принял задачу: {e}"),
            }
        }
        Voice::Remind(rest) => match parse_when(&rest, now, off) {
            Some((due, repeat, title)) => {
                // The same title at the same minute is a duplicate: Alice says
                // «напомни …» twice, or the owner retries, and both would fire.
                if !duplicate(&title, due) {
                    add(&title, Some(due), &repeat, true, "", "normal");
                }
                let rep = match repeat.as_str() {
                    "daily" => ", и так каждый день",
                    "weekdays" => ", и так по будням",
                    "weekly" => ", и так каждую неделю",
                    "monthly" => ", и так каждый месяц",
                    _ => "",
                };
                format!("Напомню {}: {title}{rep}", say_time(due, now, off))
            }
            None => "Когда напомнить? Скажите, например: напомни через 10 минут или напомни завтра в 9".into(),
        },
        Voice::AddEvent(rest) => {
            let (_len, rest) = duration(&rest);
            let Some(w) = parse_when_full(&rest, now, off) else {
                return "Когда? Скажите, например: добавь в календарь встречу с Петей завтра в 15:00".into();
            };
            let title = cap(&nominative(&w.title));
            let (start, end) = if w.has_time { (w.due, w.due + 3600) } else { (local(w.due, off).0 * 86_400 - off, 0) };
            match crate::calendar::add_event(&title, start, end, !w.has_time) {
                Ok(cal) => {
                    let when = if w.has_time { say_time(w.due, now, off) } else { day_word(local(w.due, off).0, local(now, off).0) };
                    format!("Добавила в календарь «{cal}»: {title}, {when}")
                }
                Err(e) => {
                    add(&title, Some(w.due), "", true, "", "normal");
                    format!("Google не ответил, записала в планы: {title}. Ошибка: {e}")
                }
            }
        }
        Voice::AddTask(rest) => {
            let w = parse_when_full(&rest, now, off);
            let title = cap(&w.as_ref().map_or(rest.clone(), |w| w.title.clone()));
            let due_text = w.as_ref().map(|w| {
                let d = day_word(local(w.due, off).0, local(now, off).0);
                if w.has_time { format!(", срок {d} до {}", hm(w.due, off)) } else { format!(", срок {d}") }
            });
            if google {
                let day = w.as_ref().map(|w| local(w.due, off).0);
                let notes = w.as_ref().filter(|w| w.has_time).map(|w| format!("до {}", hm(w.due, off))).unwrap_or_default();
                match crate::calendar::add_task(&title, day, &notes) {
                    Ok((name, _, _)) => return format!("Добавила задачу в «{name}»: {title}{}", due_text.clone().unwrap_or_default()),
                    Err(e) => eprintln!("planner: Google task not added: {e}"),
                }
            }
            let due = w.as_ref().map(|w| {
                if w.has_time || plan().due_time >= 1440 {
                    w.due
                } else {
                    local(w.due, off).0 * 86_400 + i64::from(plan().due_time) * 60 - off
                }
            });
            add(&title, due, w.as_ref().map_or("", |w| w.repeat.as_str()), false, "", "normal");
            format!("Добавила задачу: {title}{}", due_text.unwrap_or_default())
        }
        Voice::Agenda(day) => agenda(day),
        Voice::Ask(a) => agenda_text(a, now, off),
        Voice::Brief => agenda_text(Ask::Brief, now, off),
        Voice::Done(title) => {
            if let Some(id) = find(&title) {
                set_done(id, true);
                let t = with(|l| l.iter().find(|x| x.id == id).map(|x| x.title.clone())).unwrap_or(title);
                return format!("Отметила: {t}");
            }
            if google {
                let g = find_google(&title);
                if let Some(g) = g {
                    return match crate::calendar::complete_task(&g.list, &g.id) {
                        Ok(_) => format!("Отметила в Google: {}", g.title),
                        Err(e) => format!("Google не ответил: {e}"),
                    };
                }
            }
            format!("Не нашла задачу «{title}»")
        }
        Voice::Delete(title) => {
            if let Some(id) = find(&title) {
                delete(id);
                return "Удалила".into();
            }
            if google {
                let g = find_google(&title);
                if let Some(g) = g {
                    return match crate::calendar::delete_task(&g.list, &g.id) {
                        Ok(_) => format!("Удалила из Google: {}", g.title),
                        Err(e) => format!("Google не ответил: {e}"),
                    };
                }
            }
            format!("Не нашла задачу «{title}»")
        }
    }
}

// --------------------------------------------------------- warnings & brief

static WARNED: Mutex<Vec<(String, i64)>> = Mutex::new(Vec::new());

/// Deadlines (local and Google) coming due within `plan.warn` minutes.
fn deadlines_due(n: i64, off: i64) -> Vec<(String, i64, String)> {
    let p = plan();
    if p.warn == 0 {
        return vec![];
    }
    let lead = i64::from(p.warn) * 60;
    let (today, _) = local(n, off);
    let mut out = Vec::new();
    for t in with(|l| l.iter().filter(|t| !t.done && !t.remind && t.due.is_some()).cloned().collect::<Vec<_>>()) {
        let at = t.due.unwrap();
        if at > n && at - n <= lead {
            out.push((format!("local:{}", t.id), at, t.title.clone()));
        }
    }
    for g in crate::calendar::google_tasks() {
        let Some(day) = g.due_day.filter(|d| (today..=today + 1).contains(d)) else { continue };
        let secs = deadline_time(&format!("{} {}", g.title, g.notes))
            .or((p.due_time < 1440).then(|| i64::from(p.due_time) * 60));
        if let Some(at) = secs.map(|s| day * 86_400 + s - off).filter(|a| *a > n && *a - n <= lead) {
            out.push((format!("google:{}", g.id), at, g.title.clone()));
        }
    }
    out
}

fn warn_deadlines(n: i64, off: i64) {
    for (key, at, title) in deadlines_due(n, off) {
        let mut w = WARNED.lock().unwrap();
        if w.iter().any(|(k, a)| *k == key && *a == at) {
            continue;
        }
        w.push((key, at));
        if w.len() > 300 {
            w.drain(..100);
        }
        drop(w);
        let text = format!("{} срок: {title}", in_minutes((at - n + 30) / 60));
        eprintln!("planner: {text}");
        crate::log::add(&crate::state_dir(), "info", &text);
        crate::voice::say(text);
    }
}

fn morning_brief(dir: &Path) {
    let p = plan();
    if !p.brief_on {
        return;
    }
    let (n, off) = (now(), offset());
    let (today, secs) = local(n, off);
    let at = i64::from(p.brief_at) * 60;
    if secs < at || secs > at + 2 * 3600 || (p.brief_days == "weekdays" && weekday(today) >= 5) {
        return;
    }
    let marker = dir.join("brief-day");
    if crate::fsx::read_string(&marker).and_then(|t| t.trim().parse::<i64>().ok()) == Some(today) {
        return;
    }
    let _ = std::fs::write(&marker, today.to_string());
    let text = agenda_text(Ask::Brief, n, off);
    eprintln!("planner: brief: {text}");
    crate::log::add(dir, "info", &format!("утренняя сводка: {text}"));
    crate::voice::say(text);
}

// ------------------------------------------------------------- reminders

/// Load tasks, then say due reminders and daily summaries.
pub fn run(dir: &Path) {
    let list: Vec<Task> = crate::fsx::read_string(&file(dir))
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    *TASKS.lock().unwrap() = Some(list);
    *DIR.lock().unwrap() = Some(dir.to_owned());
    let brief_dir = dir.to_owned();
    let log_dir = dir.to_owned();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(10));
        if !ENABLED.load(Ordering::Acquire) {
            continue;
        }
        let t = now();
        let off = offset();
        warn_deadlines(t, off);
        morning_brief(&brief_dir);
        let due: Vec<(u64, String)> = with(|l| {
            l.iter()
                .filter(|x| x.remind && !x.done && x.due.is_some_and(|d| d <= t))
                .map(|x| (x.id, x.title.clone()))
                .collect()
        });
        for (id, title) in due {
            let late = with(|l| l.iter().find(|x| x.id == id).and_then(|x| x.due)).map_or(0, |d| t - d);
            if late < 6 * 3600 {
                let text = if late > 600 { format!("Пропущенное напоминание: {title}") } else { format!("Напоминаю: {title}") };
                eprintln!("planner: {text}");
                crate::log::add(&log_dir, "phrase", &text);
                crate::voice::say(text);
            }
            set_done(id, true);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip() {
        for d in [-1000, 0, 1, 20_000, 20_700] {
            let (y, m, day) = civil(d);
            assert_eq!(days_from_civil(y, m, day), d);
        }
    }

    #[test]
    fn same_reminder_twice_is_a_duplicate() {
        let now = days_from_civil(2026, 11, 22) * 86_400 + 12 * 3600;
        let off = 3 * 3600;
        let due = now + 600;
        assert!(!duplicate("полить цветы", due));
        add("полить цветы", Some(due), "", true, "", "normal");
        // The same title within the same minute: duplicate. Another minute: fine.
        assert!(duplicate("полить цветы", due));
        assert!(duplicate("Полить Цветы", due + 30));
        assert!(!duplicate("полить цветы", due + 120));
        let _ = (now, off);
    }

    #[test]
    fn parses_relative_and_clock_times() {
        let now = days_from_civil(2026, 11, 20) * 86_400 + 10 * 3600;
        let off = 3 * 3600;
        let w = parse_when_full("позвонить маме через 10 минут", now, off).unwrap();
        assert_eq!(w.due, now + 600);
        assert_eq!(w.title, "позвонить маме");
        assert!(w.has_time);
        let w = parse_when_full("встреча завтра в 15:00", now, off).unwrap();
        assert_eq!(w.title, "встреча");
        assert_eq!(local(w.due, off).1, 15 * 3600);
        let w = parse_when_full("оплатить интернет 12 марта", now, off + 0).unwrap();
        let (y, m, d) = civil(local(w.due, off).0);
        assert_eq!((y, m, d), (2027, 3, 12));
    }

    #[test]
    fn strict_whitelist() {
        assert_eq!(voice("какая погода"), None);
        // «доброе утро» belongs to Yandex, never to us.
        assert_eq!(voice("доброе утро"), None);
        assert_eq!(voice("доброе утро алиса"), None);
        assert!(matches!(voice("напомни через 5 минут выпить воды"), Some(Voice::Remind(_))));
        assert!(matches!(voice("добавь задачу купить молоко"), Some(Voice::AddTask(_))));
        assert!(matches!(voice("какие у меня сегодня планы"), Some(Voice::Ask(Ask::Plans(0)))));
        assert!(matches!(voice("какие планы на сегодня"), Some(Voice::Ask(Ask::Plans(0)))));
        // A nonsense tail must stay with Alice.
        assert_eq!(voice("какие планы жизнь"), None);
        assert_eq!(voice("какие планы на завтра кофе"), None);
        // A weekday must resolve to that weekday, not silently to today.
        let sat = voice("какие планы на субботу");
        assert!(matches!(sat, Some(Voice::Ask(Ask::Plans(d))) if d > 0));
        assert_eq!(voice("найди фильм колобок"), None);
        assert!(matches!(voice("добавь задачу проверить планы завтра в 10"), Some(Voice::AddTask(_))));
        assert!(matches!(voice("добавь дедлайн сдать отчёт завтра"), Some(Voice::AddTask(_))));
        assert!(matches!(voice("что мне сегодня нужно сделать"), Some(Voice::Ask(Ask::Todo(0)))));
        assert!(matches!(voice("какие у меня дедлайны"), Some(Voice::Ask(Ask::Deadlines))));
        assert!(matches!(voice("какие планы на следующую неделю"), Some(Voice::Ask(Ask::NextWeek))));
        assert!(matches!(voice("какие планы на неделю"), Some(Voice::Ask(Ask::Week))));
        assert_eq!(voice("найди фильм колобок"), None);
    }
}
