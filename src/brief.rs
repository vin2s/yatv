//! The morning brief: news from an RSS source, weather (Open-Meteo) and the
//! day's plans, spoken one phrase at a time. Triggered by «Алиса, доброе утро».
//! No keys: the news source is a plain RSS URL typed in the panel; the weather
//! is Open-Meteo by the coordinates in the settings.

use std::time::Duration;

use crate::{planner, settings::Settings, voice};

/// The feeds offered in the panel: key, name, address.
/// A generation counter instead of stop files: «стоп» bumps the generation,
/// the running brief sees its own generation is over and quits. A new brief
/// takes the fresh generation, so an old leftover stop can never kill it.
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// «Стоп» (or a new brief replacing the old one) ends the current brief.
pub fn request_stop() -> bool {
    let before = GENERATION.load(std::sync::atomic::Ordering::Acquire);
    GENERATION.store(before + 1, std::sync::atomic::Ordering::Release);
    voice::cancel_alice();
    before > 0
}

fn current_generation() -> u64 {
    GENERATION.load(std::sync::atomic::Ordering::Acquire)
}

/// Normalizes a spoken phrase the same way the controller does: lower case,
/// yo → ye, no wake word «алиса,», no trailing punctuation.
pub fn normalize(text: &str) -> String {
    let t = text.trim().to_lowercase().replace('ё', "е");
    let t = t.strip_prefix("алиса, ").unwrap_or(&t).trim().to_owned();
    t.trim_end_matches(['?', '!', '.']).trim().to_owned()
}

/// Is this the phrase the owner chose to trigger the brief? Empty = never.
/// The trigger may hold several variants, comma-separated («новости, включи новости»).
pub fn is_trigger(phrase: &str, trigger: &str) -> bool {
    let p = normalize(phrase);
    if p.is_empty() {
        return false;
    }
    trigger.split([',', ';']).map(normalize).filter(|t| !t.is_empty()).any(|t| t == p)
}

/// Should this phrase be left to stock Alice entirely? The brief's trigger is
/// NOT intercepted unless it matches — then «доброе утро» stays hers.
pub fn trigger_from(s: &Settings) -> String {
    s.extra.get("brief_trigger").and_then(|v| v.as_str()).unwrap_or("").to_owned()
}

pub const SOURCES: [(&str, &str, &str); 6] = [
    ("ria", "РИА Новости", "https://ria.ru/export/rss2/archive/index.xml"),
    ("rbc", "РБК", "https://rssexport.rbc.ru/rbcnews/news/30/full.rss"),
    ("lenta", "Лента.ру", "https://lenta.ru/rss/news"),
    ("habr", "Хабр", "https://habr.com/ru/rss/news/?fl=ru"),
    ("kommersant", "Коммерсантъ", "https://www.kommersant.ru/RSS/news.xml"),
    ("tass", "ТАСС", "https://tass.ru/rss/v2.xml"),
];

pub fn feed_url(source: &str) -> Option<String> {
    let s = source.trim();
    if s.starts_with("https://") || s.starts_with("http://") {
        return Some(s.to_owned());
    }
    SOURCES.iter().find(|(k, _, _)| *k == s).map(|(_, _, u)| (*u).to_owned())
}

fn source_name(source: &str) -> String {
    let s = source.trim();
    SOURCES.iter().find(|(k, _, _)| *k == s).map_or(s.to_owned(), |(_, n, _)| (*n).to_owned())
}

/// One news item: the headline and the detail read out after it.
#[derive(Debug, Clone, PartialEq)]
pub struct Story {
    pub title: String,
    pub detail: String,
}

/// The first `max` items of an RSS feed: (title, plain description).
pub fn stories(xml: &str, max: usize) -> Vec<Story> {
    let mut out = Vec::new();
    let mut rest = xml;
    while out.len() < max {
        let Some(at) = ["<item", "<entry"].iter().filter_map(|t| rest.find(t)).min() else { break };
        rest = &rest[at + 5..];
        let Some(open) = rest.find("<title") else { break };
        let Some(gt) = rest[open..].find('>') else { break };
        let body = &rest[open + gt + 1..];
        let Some(close) = body.find("</title>") else { break };
        let title = clean(&body[..close]);
        rest = &body[close..];
        let detail = rest.find("<description").and_then(|d| {
            let after = &rest[d..];
            let gt = after.find('>')?;
            let body = &after[gt + 1..];
            let end = body.find("</description>")?;
            Some(clean(&body[..end]))
        }).unwrap_or_default();
        if !title.is_empty() {
            out.push(Story { title: speakable(&title), detail: speakable(&detail) });
        }
    }
    out
}

/// CDATA, tags and entities out; one plain line.
fn clean(raw: &str) -> String {
    let t = raw.trim().trim_start_matches("<![CDATA[").trim_end_matches("]]>");
    let mut s = String::with_capacity(t.len());
    let mut in_tag = false;
    for c in t.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => s.push(c),
            _ => {}
        }
    }
    let s = s
        .replace("&quot;", "\"").replace("&#34;", "\"")
        .replace("&apos;", "'").replace("&#39;", "'")
        .replace("&laquo;", "«").replace("&raquo;", "»")
        .replace("&nbsp;", " ").replace("&#160;", " ")
        .replace("&mdash;", "—").replace("&ndash;", "–")
        .replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&");
    numeric_entities(&s).split_whitespace().collect::<Vec<_>>().join(" ")
}

fn numeric_entities(s: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '&' && chars.get(i + 1) == Some(&'#') {
            if let Some(end) = (i..chars.len()).find(|j| chars[*j] == ';') {
                let code: String = chars[i + 2..end].iter().collect();
                let n = if let Some(hex) = code.strip_prefix('x').or_else(|| code.strip_prefix('X')) {
                    u32::from_str_radix(hex, 16).ok()
                } else {
                    code.parse::<u32>().ok()
                };
                if let Some(c) = n.and_then(char::from_u32) {
                    out.push(c);
                    i = end + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// The first `max` item titles of an RSS (or Atom) feed, as plain text.
pub fn titles(xml: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while out.len() < max {
        let Some(at) = ["<item", "<entry"].iter().filter_map(|t| rest.find(t)).min() else { break };
        rest = &rest[at + 5..];
        let Some(open) = rest.find("<title") else { break };
        let Some(gt) = rest[open..].find('>') else { break };
        let body = &rest[open + gt + 1..];
        let Some(close) = body.find("</title>") else { break };
        let t = clean(&body[..close]);
        if !t.is_empty() {
            out.push(t);
        }
        rest = &body[close..];
    }
    out
}

/// Removes emoji and odd symbols so TTS does not read them as nonsense.
fn speakable(t: &str) -> String {
    t.chars()
        .map(|c| if c.is_alphanumeric() || " .,:;!?—–-«»\"'()%№+".contains(c) { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Fetches the RSS and returns up to `max` stories (headline + detail).
pub fn news_stories(source: &str, max: usize) -> Result<Vec<Story>, String> {
    let url = feed_url(source).ok_or("источник новостей не задан")?;
    let agent = crate::google::agent().map_err(|e| e.to_string())?;
    let body = agent.get(&url).timeout(Duration::from_secs(15)).call()
        .map_err(|e| format!("нет связи с источником: {e}"))?
        .into_string().map_err(|e| e.to_string())?;
    eprintln!("brief: feed {url}: {} bytes, items={}, titles={}", body.len(), body.matches("<item").count(), body.matches("<title").count());
    let mut list: Vec<Story> = stories(&body, max.max(1) * 2).into_iter()
        .map(|s| Story { title: cut(&s.title, 160), detail: cut(&s.detail, 500) })
        .filter(|s| !s.title.is_empty()).collect();
    eprintln!("brief: parsed {} stories", list.len());
    if list.is_empty() {
        return Err("в ленте нет новостей".into());
    }
    list.truncate(max.max(1));
    Ok(list)
}

fn cut(t: &str, max: usize) -> String {
    if t.chars().count() <= max { return t.to_owned(); }
    let head: String = t.chars().take(max).collect();
    head.rsplit_once(' ').map_or(head.clone(), |(a, _)| a.to_owned())
}

/// Current temperature in plain words from Open-Meteo (`temp`, `text`, `max`, `min`).
pub fn weather_phrase(w: &serde_json::Value) -> Option<String> {
    let deg = |v: &serde_json::Value| v.as_f64().map(|t| t.round() as i64).map(|t| if t > 0 { format!("+{t}") } else { t.to_string() });
    let now = deg(&w["temp"])?;
    let text = w["text"].as_str().unwrap_or("");
    let mut s = if text.is_empty() { format!("Сейчас {now}.") } else { format!("Сейчас {now}, {text}.") };
    if let (Some(max), Some(min)) = (deg(&w["max"]), deg(&w["min"])) {
        s.push_str(&format!(" Днём до {max}, ночью до {min}."));
    }
    Some(s)
}

/// WMO weather codes in words.
pub fn weather_words(code: i64) -> &'static str {
    match code {
        0 => "ясно", 1..=2 => "переменная облачность", 3 => "пасмурно",
        45..=48 => "туман", 51..=57 => "морось", 61..=67 => "дождь",
        71..=77 => "снег", 80..=82 => "ливень", 85..=86 => "снегопад",
        95..=99 => "гроза", _ => "облачно",
    }
}

/// Weather providers offered in the panel: key, name, API URL (city → URL).
/// Russian providers with a public, keyless endpoint; Open-Meteo as fallback.
pub const WEATHER_SOURCES: [(&str, &str); 5] = [
    ("open-meteo", "Open-Meteo (без ключа, по координатам)"),
    ("yandex", "Яндекс.Погода"),
    ("gismeteo", "Гисметео"),
    ("rp5", "RP5 (Расписаниепогоды)"),
    ("custom", "Своя ссылка (JSON с полем temp)"),
];

/// The URL a provider needs. Yandex/Gismeteo/RP5 have no keyless public API:
/// their entries work only when the owner typed a link in the weather URL field.
pub fn weather_url_for(provider: &str, lat: f64, lon: f64) -> Option<String> {
    match provider {
        "open-meteo" => {
            let (la, lo) = (lat.to_string(), lon.to_string());
            Some(format!("https://api.open-meteo.com/v1/forecast?latitude={la}&longitude={lo}&current=temperature_2m,weather_code&daily=temperature_2m_max,temperature_2m_min&forecast_days=1"))
        }
        "custom" | "" => None, // only the owner's URL
        _ => None, // yandex/gismeteo/rp5 need the owner's URL
    }
}

/// Fetches weather from the chosen provider's page or the owner's URL.
/// The answer is a JSON with `temp` (custom links); Open-Meteo is normalized here.
pub fn weather(provider: &str, lat: f64, lon: f64, custom_url: &str) -> Result<serde_json::Value, String> {
    let agent = crate::google::agent().map_err(|e| e.to_string())?;
    let v: serde_json::Value = if !custom_url.trim().is_empty() {
        agent.get(custom_url.trim()).timeout(Duration::from_secs(15)).call()
            .map_err(|e| format!("ваш источник погоды не ответил: {e}"))?
            .into_json().map_err(|e| format!("не погодный JSON: {e}"))?
    } else {
        let url = weather_url_for(provider, lat, lon)
            .ok_or("у этого провайдера нет открытого API — задайте свою ссылку на JSON с полем temp")?;
        let v: serde_json::Value = agent.get(&url).timeout(Duration::from_secs(15)).call()
            .map_err(|e| format!("сервис погоды не ответил: {e}"))?
            .into_json().map_err(|e| e.to_string())?;
        let code = v["current"]["weather_code"].as_i64().unwrap_or(-1);
        serde_json::json!({
            "temp": v["current"]["temperature_2m"].as_f64(),
            "text": weather_words(code),
            "max": v["daily"]["temperature_2m_max"][0].as_f64(),
            "min": v["daily"]["temperature_2m_min"][0].as_f64(),
        })
    };
    if v["temp"].as_f64().is_none() {
        return Err("в ответе нет температуры (нужен JSON с полем temp)".into());
    }
    Ok(v)
}

/// Says everything, one phrase at a time; returns what was said (for the panel).
/// Says news, weather and the day's plans, one phrase at a time. «стоп» or a
/// newer trigger phrase ends the running brief (its generation is over).
pub fn run(s: &Settings) -> Vec<String> {
    // A new brief ends any previous one: only one brief speaks at a time.
    let my_gen = GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
    let mut said = Vec::new();
    crate::log::add(&crate::state_dir(), "info", "сводка: началась");
    let result = run_inner(s, my_gen, &mut said);
    if let Err(e) = result {
        eprintln!("brief: {e}");
        crate::log::add(&crate::state_dir(), "error", &format!("сводка: {e}"));
    }
    if current_generation() == my_gen {
        crate::log::add(&crate::state_dir(), "info", &format!("сводка: сказано {} фраз", said.len()));
    }
    said
}

/// The panel's order of blocks: ids separated by commas. Blocks whose checkbox
/// is off are skipped when speaking, but stay in the saved order — so checking
/// them back puts them where they were.
fn block_order(s: &Settings) -> Vec<String> {
    let on = |k: &str| s.extra.get(k).and_then(|v| v.as_bool()).unwrap_or(true);
    let news_on = !s.extra.get("brief_news").and_then(|v| v.as_str()).unwrap_or("").trim().is_empty();
    let raw = s.extra.get("brief_order").and_then(|v| v.as_str()).unwrap_or("");
    let mut ids: Vec<String> = raw.split(',').map(|x| x.trim().to_owned()).filter(|x| !x.is_empty()).collect();
    // Heal older saves where a disabled block was dropped from the order.
    for id in ["weather", "plans", "news"] {
        if !ids.iter().any(|x| x == id) {
            ids.push(id.to_owned());
        }
    }
    ids.into_iter().filter(|id| match id.as_str() {
        "weather" => on("brief_weather"),
        "plans" => on("brief_plans"),
        n if n == "news" || n.starts_with("news") => news_on,
        _ => true,
    }).collect()
}

fn run_inner(s: &Settings, my_gen: u64, said: &mut Vec<String>) -> Result<(), String> {
    let mut say = |text: String| -> bool {
        // My generation is over: «стоп» said or a newer brief started.
        if current_generation() != my_gen {
            crate::log::add(&crate::state_dir(), "info", "сводка: остановлена фразой «стоп»");
            return false;
        }
        voice::say_wait(&text);
        said.push(text.clone());
        crate::log::add(&crate::state_dir(), "phrase", &format!("сводка: {text}"));
        true
    };
    // Build every block first, then speak them in the owner's order.
    let mut blocks: Vec<(String, Vec<String>)> = Vec::new();

    let weather_url = s.extra.get("brief_weather_url").and_then(|v| v.as_str()).unwrap_or("");
    let provider = s.extra.get("brief_weather_provider").and_then(|v| v.as_str()).unwrap_or("open-meteo");
    // Weather is read from the chosen provider or the owner's URL.
    if s.extra.get("brief_weather").and_then(|v| v.as_bool()).unwrap_or(true) && (!weather_url.trim().is_empty() || provider == "open-meteo") {
        match weather(provider, 0.0, 0.0, weather_url) {
            Ok(w) => if let Some(p) = weather_phrase(&w) { blocks.push(("weather".into(), vec![p])); },
            Err(e) => {
                eprintln!("brief: weather: {e}");
                crate::log::add(&crate::state_dir(), "error", &format!("сводка: погода: {e}"));
            },
        }
    }
    if s.extra.get("brief_plans").and_then(|v| v.as_bool()).unwrap_or(true) {
        blocks.push(("plans".into(), vec![planner::agenda(Some(0))]));
    }
    let news_count = s.extra.get("brief_news_count").and_then(|v| v.as_u64()).unwrap_or(5).clamp(1, 10) as usize;
    let source = s.extra.get("brief_news").and_then(|v| v.as_str()).unwrap_or("");
    if !source.trim().is_empty() {
        match news_stories(source, news_count) {
            Ok(list) => {
                crate::log::add(&crate::state_dir(), "info", &format!("сводка: новостей загружено {}", list.len()));
                for (i, story) in list.iter().enumerate() {
                    let mut lines = vec![format!("Новость {} из {}. {}", i + 1, list.len(), story.title)];
                    if !story.detail.is_empty() && story.detail != story.title {
                        lines.push(story.detail.clone());
                    }
                    blocks.push((format!("news{}", i + 1), lines));
                }
            }
            Err(e) => {
                eprintln!("brief: news: {e}");
                crate::log::add(&crate::state_dir(), "error", &format!("сводка: новости: {e}"));
                blocks.push(("news".into(), vec!["Новости сейчас не загрузились".into()]));
            }
        }
    }
    // Speak in the configured order; `news` stands for all news blocks.
    let order = block_order(s);
    let mut spoken: Vec<String> = Vec::new();
    for id in order {
        if id == "news" {
            for (bid, lines) in blocks.iter().filter(|(b, _)| b.starts_with("news")) {
                if spoken.contains(bid) { continue; }
                for line in lines { if !say(line.clone()) { return Ok(()); } }
                spoken.push(bid.clone());
            }
        } else if let Some((bid, lines)) = blocks.iter().find(|(b, _)| *b == id) {
            for line in lines { if !say(line.clone()) { return Ok(()); } }
            spoken.push(bid.clone());
        }
    }
    // Anything not named in the order still gets said.
    for (bid, lines) in &blocks {
        if spoken.contains(bid) { continue; }
        for line in lines { if !say(line.clone()) { return Ok(()); } }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrase_trigger_is_exact_and_optional() {
        assert!(is_trigger("Алиса, включи новости", "включи новости"));
        assert!(is_trigger("включи новости.", "Алиса, включи новости"));
        assert!(!is_trigger("доброе утро", "включи новости"));
        assert!(!is_trigger("включи новости", ""));
        // Several variants, comma-separated: any one of them triggers.
        assert!(is_trigger("новости", "новости, включи новости,"));
        assert!(is_trigger("включи новости", "новости, включи новости,"));
        assert!(!is_trigger("доброе утро", "новости, включи новости,"));
        assert_eq!(normalize(" Алиса, Включи Новости! "), "включи новости");
    }

    #[test]
    fn parses_rss_titles_and_details() {
        let xml = r#"<?xml version="1.0"?><rss><channel><title>Feed</title>
          <item><title><![CDATA[Первая &laquo;новость&raquo;]]></title><description><![CDATA[Подробности: <b>случилось</b> событие.]]></description><link>x</link></item>
          <item><title>Вторая &amp; третья</title><description>Ещё детали.</description></item></channel></rss>"#;
        let s = stories(xml, 5);
        assert_eq!(s[0].title, "Первая «новость»");
        assert_eq!(s[0].detail, "Подробности: случилось событие.");
        assert_eq!(s[1].title, "Вторая третья");
    }

    #[test]
    fn weather_phrase_and_feed_url() {
        let w = serde_json::json!({"temp": 8.2, "text": "пасмурно", "max": 9.1, "min": -1.4});
        assert_eq!(weather_phrase(&w).as_deref(), Some("Сейчас +8, пасмурно. Днём до +9, ночью до -1."));
        assert!(feed_url("kommersant").unwrap().contains("kommersant.ru"));
        assert!(feed_url("https://x.ru/rss").unwrap().contains("x.ru"));
        assert!(feed_url("").is_none());
    }
}
