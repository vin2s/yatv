//! Android TV / projector by voice: switch on and off, open apps, search films
//! and YouTube, pause, volume. Talks to the TV over ADB (adb.rs); the TV needs
//! «Отладка по сети» (network ADB) on, and the first time a press on «Разрешить».

use std::{path::Path, time::Duration};

use crate::{adb, action::TvAction};

/// Spoken app names → parts of their package names (first match wins).
/// (shown name, spoken names, package names or parts of them)
const APPS: &[(&str, &[&str], &[&str])] = &[
    ("YouTube", &["ютуб", "youtube", "ютьюб"], &["com.google.android.youtube.tv", "youtube.tv", "youtube"]),
    ("YouTube Music", &["ютуб музык", "ютуб мьюзик"], &["youtube.tv.unplugged", "youtube.music", "youtube.tvmusic"]),
    ("Кинопоиск", &["кинопоиск"], &["kinopoisk"]),
    ("ivi", &["иви", "ivi"], &["ivi"]),
    ("Okko", &["окко", "okko"], &["okko"]),
    ("Wink", &["винк", "wink"], &["wink", "ru.rt.video"]),
    ("KION", &["кион", "kion"], &["kion", "mts.mtstv", "ru.mts"]),
    ("Netflix", &["нетфликс", "netflix"], &["netflix"]),
    ("Twitch", &["твич", "twitch"], &["twitch"]),
    ("VK Видео", &["вк видео", "вк", "вконтакте"], &["vkvideo", "vk.video", "com.vk"]),
    ("RUTUBE", &["рутуб", "rutube"], &["rutube"]),
    ("PREMIER", &["премьер"], &["premier"]),
    ("Амедиатека", &["амедиатек"], &["amediateka"]),
    ("Смотрёшка", &["смотрешк"], &["smotreshka"]),
    ("Plex", &["плекс", "plex"], &["plex"]),
    ("Kodi", &["коди", "kodi"], &["kodi"]),
    ("VLC", &["влц", "vlc"], &["videolan"]),
    ("Spotify", &["спотифай", "spotify"], &["spotify"]),
    ("Яндекс Музыка", &["яндекс музык"], &["yandex.music"]),
    ("Браузер", &["браузер", "хром"], &["browser", "chrome"]),
    ("Настройки", &["настройки", "параметры"], &["com.android.tv.settings", "settings"]),
    ("Google Play", &["гугл плей", "плей маркет", "плеймаркет", "магазин"], &["com.android.vending"]),
    ("Lampa", &["лампа", "lampa"], &["lampa"]),
    ("KinoPub", &["кинопаб", "kinopub"], &["kinopub"]),
    ("HD VideoBox", &["видеобокс", "hd videobox"], &["videobox"]),
    ("LazyMedia Deluxe", &["лмд", "лэнд", "делюкс", "lmd", "лм делюкс", "lm deluxe"], &["lazycatsoftware.lmd"]),
    ("Free TV", &["фри ти ви", "фритиви", "free tv", "free-tv", "atx", "атикс"], &["ru.cn.atx"]),
    ("SmartTube", &["смарт тьюб", "смарттьюб", "смарт туб", "смарт тюб", "смарт тьюбе", "smarttube", "smart tube"], &["liskovsoft.smarttubetv", "smarttube"]),
    ("VPN", &["впн", "vpn", "випиэн"], &["vpn", "wireguard", "amnezia", "outline", "hiddify", "v2ray", "nekobox", "openvpn"]),
];

/// Video apps a search can go into («найди … на рутубе»); not music (Alice's), settings, VPN.
const VIDEO_APPS: &[&str] = &["Кинопоиск", "ivi", "Okko", "Wink", "KION", "Netflix", "Twitch", "VK Видео", "RUTUBE", "PREMIER",
    "Амедиатека", "Смотрёшка", "Plex", "Kodi", "Lampa", "KinoPub", "HD VideoBox", "SmartTube"];

/// Apps that search in a screen of their own: (package, the activity to open).
/// The query is the «query» extra of that screen.
const OWN_SEARCH: &[(&str, &str)] = &[(
    "com.lazycatsoftware.lmd",
    "com.lazycatsoftware.lazymediadeluxe.ui.tv.activities.ActivityTvSearch",
)];

/// The shown name of the video app `said` starts with («рутубе» → RUTUBE).
fn video_app(said: &str) -> Option<&'static str> {
    let s = said.trim();
    APPS.iter()
        .filter(|(label, _, _)| VIDEO_APPS.contains(label))
        .find(|(_, names, _)| names.iter().any(|n| s.starts_with(n) && s.len() <= n.len() + 6))
        .map(|(label, _, _)| *label)
}

/// «найди маму в 16 на рутубе» → ("рутубе", "маму в 16"): a search inside a video app.
pub fn search_in_phrase(text: &str) -> Option<(String, String)> {
    let rest = ["найди ", "поищи ", "покажи ", "включи ", "поставь ", "запусти "].iter().find_map(|v| text.strip_prefix(v))?;
    let kind = |q: &str| -> String {
        ["фильм ", "сериал ", "мультфильм ", "мультик ", "видео ", "ролик "].iter().find_map(|p| q.strip_prefix(p)).unwrap_or(q).trim().to_owned()
    };
    // The app first: «найди на рутубе фильм дюна», «включи в вк видео клипы».
    if let Some(after) = ["на ", "в ", "во "].iter().find_map(|p| rest.strip_prefix(p)) {
        let words: Vec<&str> = after.split_whitespace().collect();
        for n in [2, 1] {
            if words.len() > n && video_app(&words[..n].join(" ")).is_some() {
                let q = kind(&words[n..].join(" "));
                if !q.is_empty() {
                    return Some((words[..n].join(" "), q));
                }
            }
        }
    }
    for sep in [" на ", " в ", " во "] {
        if let Some(i) = rest.rfind(sep) {
            let (q, app) = (rest[..i].trim(), rest[i + sep.len()..].trim());
            if q.is_empty() || video_app(app).is_none() {
                continue;
            }
            let q = ["фильм ", "сериал ", "мультфильм ", "мультик ", "видео ", "ролик "].iter().find_map(|p| q.strip_prefix(p)).unwrap_or(q);
            return Some((app.to_owned(), q.trim().to_owned()));
        }
    }
    None
}

/// A video app's own search page, for apps without Android's search screen (RUTUBE): opened
/// as a link in that app.
fn search_url(label: &str, q: &str) -> Option<String> {
    let q = percent_encode(q);
    let base = match label {
        "RUTUBE" => "https://rutube.ru/search/?query=",
        "VK Видео" => "https://vkvideo.ru/?q=",
        "ivi" => "https://www.ivi.ru/search/?q=",
        "Okko" => "https://okko.tv/search/",
        "Wink" => "https://wink.ru/search?query=",
        "KION" => "https://kion.ru/search?text=",
        "PREMIER" => "https://premier.one/search?query=",
        "Кинопоиск" => "https://hd.kinopoisk.ru/search?text=",
        "Twitch" => "https://www.twitch.tv/search?term=",
        _ => return None,
    };
    Some(format!("{base}{q}"))
}

/// The shown name of the app a package belongs to (APPS), for its search page.
fn label_of_package(pkg: &str) -> Option<&'static str> {
    APPS.iter().find(|(_, _, parts)| parts.iter().any(|p| pkg.contains(p))).map(|(label, _, _)| *label)
}

/// Does what was said start with a known app's name («смарт тьюб», «ютуб»)?
pub fn app_named(said: &str) -> bool {
    let s = said.trim();
    APPS.iter().any(|(_, names, _)| names.iter().any(|n| s.starts_with(n)))
}

/// A video app by its spoken name («ютуб», «кинопоиск»): what «открой …» without a device
/// may mean on a TV. Not music apps (Alice's), the browser, settings or the VPN.
pub fn screen_app(said: &str) -> bool {
    let s = said.trim();
    !s.starts_with("ютуб муз")
        && !s.starts_with("ютуб мьюз")
        && APPS.iter().any(|(label, names, _)| (*label == "YouTube" || VIDEO_APPS.contains(label)) && names.iter().any(|n| s.starts_with(n)))
}

/// Apps only the TV has: said without «на проекторе», the phrase is still the TV's.
pub fn tv_only_app(said: &str) -> bool {
    ["смарт тьюб", "смарттьюб", "смарт туб", "смарт тюб", "smarttube", "smart tube"].iter().any(|n| said.contains(n))
}

/// Panel phrases: (phrase, package, TV name). Empty TV = default TV.
static PHRASES: std::sync::Mutex<Vec<(String, String, String)>> = std::sync::Mutex::new(Vec::new());

pub fn set_phrases(list: Vec<(String, String, String)>) {
    *PHRASES.lock().unwrap() = list;
}

/// The app phrase with the TV it belongs to: (package, TV name).
pub fn phrase_for(text: &str) -> Option<(String, String)> {
    let key = crate::action::normalize_phrase(text);
    let stem = |s: &str| -> String {
        app_name_key(s).chars().take(2).collect()
    };
    let want = stem(&key);
    PHRASES.lock().unwrap().iter().find(|(phrase, _, _)| {
        phrase == &key
            || (!want.is_empty() && stem(phrase) == want)
    })
    .map(|(_, pkg, tv)| (pkg.clone(), tv.clone()))
}

/// Package for a spoken app name, using the built-in aliases only (no ADB).
pub fn find_package_by_name(spoken: &str) -> Option<String> {
    let key = crate::action::normalize_phrase(spoken);
    APPS.iter().find_map(|(_, names, packages)| {
        if names.iter().any(|n| {
            let n = crate::action::normalize_phrase(n);
            !n.is_empty() && (key == n || key.contains(&n) || n.contains(&key))
        }) {
            packages.first().map(|p| (*p).to_owned())
        } else {
            None
        }
    })
}


/// A loose key for a name: Alice may write «эфм плеер» as «fm player», «лмд» as
/// «lmd». Cyrillic is transliterated, then the vowels are dropped, so both
/// spellings meet. For the user's own names only: the whole phrase must still
/// match, so music is not taken.
pub fn app_name_key(said: &str) -> String {
    let words: String = said.to_lowercase().replace('ё', "е").split_whitespace().collect::<Vec<_>>().join("");
    translit(&words).chars().filter(|c| !"aeiouy".contains(*c)).collect()
}

/// «найди фильм колобок на делюкс» → («найди фильм колобок», «делюкс»): the app
/// named at the end of the phrase.
pub fn split_app_suffix(text: &str) -> Option<(String, String)> {
    let t = text.trim();
    let at = t.rfind(" на ")?;
    let (head, tail) = t.split_at(at);
    let app = tail.trim_start().strip_prefix("на ")?.trim();
    if app.is_empty() || app.split_whitespace().count() > 3 {
        return None;
    }
    let head = head.trim();
    (!head.is_empty()).then(|| (head.to_owned(), app.to_owned()))
}

// ---------------------------------------------------------------- the screen

/// Something on the TV's screen (uiautomator): its words and where it is.
#[derive(Debug, Clone, PartialEq)]
pub struct UiNode {
    pub label: String,
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
    pub clickable: bool,
}

impl UiNode {
    fn center(&self) -> (i32, i32) {
        ((self.x1 + self.x2) / 2, (self.y1 + self.y2) / 2)
    }
    fn area(&self) -> i64 {
        i64::from(self.x2 - self.x1).max(0) * i64::from(self.y2 - self.y1).max(0)
    }
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(" {name}=\"");
    let start = tag.find(&key)? + key.len();
    let end = tag[start..].find('"')? + start;
    Some(&tag[start..end])
}

fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&apos;", "'").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// uiautomator's XML → the nodes that have words (text or content-desc) and a place.
pub fn ui_nodes(xml: &str) -> Vec<UiNode> {
    let mut out = Vec::new();
    for tag in xml.split("<node").skip(1) {
        let tag = tag.split('>').next().unwrap_or("");
        let text = attr(tag, "text").map(unescape).unwrap_or_default();
        let desc = attr(tag, "content-desc").map(unescape).unwrap_or_default();
        let label = if text.trim().is_empty() { desc } else { text };
        let Some(b) = attr(tag, "bounds") else { continue };
        let n: Vec<i32> = b.split(|c: char| !c.is_ascii_digit() && c != '-').filter(|x| !x.is_empty()).filter_map(|x| x.parse().ok()).collect();
        let [x1, y1, x2, y2] = n[..] else { continue };
        let yes = |k: &str| attr(tag, k) == Some("true");
        out.push(UiNode { label: label.trim().to_owned(), x1, y1, x2, y2, clickable: yes("clickable") || yes("focusable") });
    }
    out
}

fn norm(s: &str) -> String {
    s.to_lowercase().replace('ё', "е").chars().map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Words said → the thing on the screen: the whole label first, then the label
/// inside what was said or the other way round, then the stems («историю» → «история»).
pub fn find_on_screen<'a>(nodes: &'a [UiNode], said: &str) -> Option<&'a UiNode> {
    let q = norm(said);
    if q.is_empty() {
        return None;
    }
    let stem = |w: &str| w.chars().take(5).collect::<String>();
    let score = |n: &UiNode| -> u8 {
        let l = norm(&n.label);
        if l.is_empty() {
            0
        } else if l == q {
            4
        } else if l.chars().count() >= 3 && (q.contains(&l) || l.contains(&q)) {
            3
        } else if q.split(' ').all(|w| l.split(' ').any(|lw| stem(lw) == stem(w))) {
            2
        } else if q.split(' ').next().is_some_and(|w| w.chars().count() >= 4 && l.split(' ').any(|lw| stem(lw) == stem(w))) {
            1
        } else {
            0
        }
    };
    nodes
        .iter()
        .filter(|n| n.area() > 0)
        .map(|n| (score(n), n))
        .filter(|(s, _)| *s > 0)
        // Best match; then a pressable one; then the smaller (the item, not its whole row).
        .max_by_key(|(s, n)| (*s, n.clickable, -n.area()))
        .map(|(_, n)| n)
}

/// The first video of a list or grid: pressable, with words, out of the left menu
/// (the left fifth of the screen), not a menu item; the top one, then the left one.
pub fn first_item<'a>(nodes: &'a [UiNode]) -> Option<&'a UiNode> {
    let w = nodes.iter().map(|n| n.x2).max().unwrap_or(0);
    let h = nodes.iter().map(|n| n.y2).max().unwrap_or(0);
    let menu = ["главная", "история", "подписки", "поиск", "настройки", "библиотека", "плейлисты", "home", "history", "subscriptions", "search", "settings"];
    nodes
        .iter()
        .filter(|n| n.clickable && !n.label.is_empty() && n.x1 >= w / 5 && n.y1 > h / 10)
        .filter(|n| !menu.contains(&norm(&n.label).as_str()))
        .filter(|n| n.area() > i64::from(w) * i64::from(h) / 400)
        .min_by_key(|n| (n.y1 / 40, n.x1))
}

/// The words a VPN's main button may have, best first.
const START_WORDS: [&str; 8] = ["подключить", "подключиться", "включить", "connect", "start", "старт", "запустить", "tap to connect"];

fn dump_screen(sh: &dyn Fn(&str) -> Result<String, String>) -> Result<Vec<UiNode>, String> {
    // uiautomator needs a still screen; a moving one («could not get idle state») is tried again.
    for _ in 0..3 {
        let out = sh("uiautomator dump /data/local/tmp/yatv-ui.xml >/dev/null 2>&1; cat /data/local/tmp/yatv-ui.xml 2>/dev/null")?;
        let nodes = ui_nodes(&out);
        if !nodes.is_empty() {
            return Ok(nodes);
        }
        std::thread::sleep(Duration::from_millis(1200));
    }
    Err("Не получилось прочитать экран телевизора".into())
}

fn tap(sh: &dyn Fn(&str) -> Result<String, String>, n: &UiNode) -> Result<(), String> {
    let (x, y) = n.center();
    sh(&format!("input tap {x} {y}")).map(|_| ())
}

fn open_app(sh: &dyn Fn(&str) -> Result<String, String>, name: &str) -> Result<String, String> {
    wake_if_asleep(sh);
    let list = sh("pm list packages")?;
    let packages: Vec<String> = list.lines().filter_map(|l| l.trim().strip_prefix("package:")).map(str::to_owned).collect();
    let Some(pkg) = find_package(name, &packages) else {
        return Err(format!("Не нашла на телевизоре приложение {name}"));
    };
    let out = sh(&launch_script(&pkg))?;
    if out.contains("Error") || out.contains("No activities") || out.contains("Exception") {
        return Err(format!("Телевизор не открыл {name}"));
    }
    Ok(pkg)
}

/// Labels on the screen for the log when nothing matched (to tune the words).
fn seen(nodes: &[UiNode]) -> String {
    nodes.iter().filter(|n| !n.label.is_empty()).take(25).map(|n| n.label.chars().take(30).collect::<String>()).collect::<Vec<_>>().join(" | ")
}

fn translit(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        let t = match c {
            'а' => "a", 'б' => "b", 'в' => "v", 'г' => "g", 'д' => "d", 'е' | 'ё' | 'э' => "e", 'ж' => "zh",
            'з' => "z", 'и' | 'й' | 'ы' => "i", 'к' => "k", 'л' => "l", 'м' => "m", 'н' => "n", 'о' => "o",
            'п' => "p", 'р' => "r", 'с' => "s", 'т' => "t", 'у' => "u", 'ф' => "f", 'х' => "h", 'ц' => "c",
            'ч' => "ch", 'ш' => "sh", 'щ' => "sch", 'ю' => "yu", 'я' => "ya",
            c if c.is_ascii_alphanumeric() => {
                out.push(c.to_ascii_lowercase());
                continue;
            }
            _ => "",
        };
        out.push_str(t);
    }
    out
}

/// A package as people know it: «com.google.android.youtube.tv» → «YouTube».
/// A known app by its package (the longest matching part wins: «…youtube.tvmusic»
/// is YouTube Music); another: the telling part of its name, the package after it.
pub fn label(package: &str) -> String {
    let full = |p: &str| p.matches('.').count() >= 2;
    let known = APPS
        .iter()
        .filter_map(|(shown, _, parts)| {
            let len = parts.iter().filter(|p| if full(p) { package == **p } else { package.contains(**p) }).map(|p| p.len()).max()?;
            Some((len, *shown))
        })
        .max_by_key(|(len, _)| *len);
    if let Some((_, shown)) = known {
        return shown.to_owned();
    }
    const PLAIN: [&str; 14] = ["com", "org", "net", "ru", "tv", "app", "apps", "android", "google", "mobile", "atv", "androidtv", "release", "free"];
    let word = package.split('.').filter(|w| !PLAIN.contains(w) && w.len() > 1).max_by_key(|w| w.len()).unwrap_or(package);
    let mut c = word.chars();
    let word = c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default();
    format!("{word} ({package})")
}

/// The installed package for a spoken app name.
pub fn find_package(spoken: &str, packages: &[String]) -> Option<String> {
    let spoken = spoken.trim().to_lowercase();
    // The most specific name wins: «ютуб музыку» is YouTube Music, not YouTube.
    let entry = APPS
        .iter()
        .filter_map(|(_, names, parts)| {
            let len = names.iter().filter(|n| spoken.starts_with(**n)).map(|n| n.len()).max()?;
            Some((len, *parts))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, parts)| parts);
    if let Some(parts) = entry {
        // Parts of other apps: «…youtube.tvmusic» contains «…youtube.tv» but is YouTube Music.
        // A full package name («com.google.android.youtube.tv») only matches itself.
        let full = |p: &str| p.matches('.').count() >= 2;
        let others: Vec<&str> =
            APPS.iter().flat_map(|(_, _, p)| p.iter().copied()).filter(|p| !parts.contains(p) && !full(p)).collect();
        for part in parts {
            if let Some(p) = packages.iter().find(|p| p.as_str() == *part) {
                return Some(p.clone());
            }
            if full(part) {
                continue;
            }
            let found = packages.iter().find(|p| {
                // …and not a package that another app's name explains better.
                p.contains(part) && !others.iter().any(|o| p.contains(o) && !part.contains(o))
            });
            if let Some(p) = found {
                return Some(p.clone());
            }
        }
        // A known app that is not installed: not some other app with a similar name.
        return None;
    }
    // Anything else: the name, in Latin letters, inside the package name.
    // Within one part of the package name: «com.amazon.avod» joined up would
    // contain «zona».
    for t in spellings(&spoken) {
        if let Some(p) = packages.iter().find(|p| p.split(['.', '_', '-']).any(|part| part.contains(&t))) {
            return Some(p.clone());
        }
    }
    None
}

/// How a spoken app name may appear in a package name, best first: as said
/// («zona»), back from the case Alice heard («зону» → zona), with
/// other spellings of the sounds (кс → x, ю → iu/u, й → y…), and the stem.
fn spellings(spoken: &str) -> Vec<String> {
    let words: String = spoken.split_whitespace().collect::<Vec<_>>().join("");
    let mut bases = vec![words.clone()];
    // «Открой зону / кинопоиск / тиви»: accusative and similar endings back to the name.
    for (end, base) in [("у", "а"), ("ю", "я"), ("ом", ""), ("е", "а")] {
        if let Some(stem) = words.strip_suffix(end) {
            bases.push(format!("{stem}{base}"));
        }
    }
    let mut out: Vec<String> = Vec::new();
    let mut add = |s: String| {
        if s.len() >= 3 && !out.contains(&s) {
            out.push(s);
        }
    };
    for b in &bases {
        let t = translit(b);
        add(t.clone());
        add(t.replace("ks", "x").replace("yu", "u").replace("ya", "ia").replace("ts", "z").replace("kh", "h"));
        add(t.replace("yu", "iu").replace("zh", "j").replace('c', "ts"));
        add(t.replace("i", "y"));
    }
    // The stem without its last vowel («zon» for «зона»), only when long enough
    // not to match everything.
    let t = translit(&words);
    let stem = t.trim_end_matches(['a', 'e', 'i', 'o', 'u', 'y']);
    if stem.len() >= 4 {
        add(stem.to_owned());
    }
    out
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else if b == b' ' { "+".into() } else { format!("%{b:02X}") })
        .collect()
}

fn speech(e: &adb::Error) -> String {
    match e {
        adb::Error::Unreachable(_) => {
            "Телевизор не отвечает. Он включён, в той же сети и с включённой отладкой по сети?".into()
        }
        adb::Error::NeedsConfirm => {
            "На экране телевизора появился запрос на отладку. Нажмите «Разрешить» и повторите".into()
        }
        adb::Error::Other(e) => format!("Телевизор не выполнил команду: {e}"),
    }
}

/// Runs the action; the answer is a short sentence for Alice to say.
/// The ADB key, read from disk once (it was read and parsed on every key press).
fn key(dir: &Path) -> Result<std::sync::Arc<adb::Key>, String> {
    static KEY: std::sync::Mutex<Option<(std::path::PathBuf, std::sync::Arc<adb::Key>)>> = std::sync::Mutex::new(None);
    let mut g = KEY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((d, k)) = &*g {
        if d == dir {
            return Ok(k.clone());
        }
    }
    let k = std::sync::Arc::new(adb::Key::load_or_create(dir).map_err(|e| format!("нет ключа для телевизора: {e}"))?);
    *g = Some((dir.to_owned(), k.clone()));
    Ok(k)
}

pub fn run(host: &str, dir: &Path, action: &TvAction) -> Result<String, String> {
    let key = key(dir)?;
    // Waiting for «Разрешить» makes sense only for the check from the settings page.
    let wait = if matches!(action, TvAction::Ping) { Duration::from_secs(40) } else { Duration::from_secs(8) };
    // One connection kept between presses: the remote answers without a new handshake.
    let sh = |cmd: &str| adb::shell_kept(host, &key, cmd, wait).map_err(|e| speech(&e));
    match action {
        TvAction::Ping => {
            let out = sh("getprop ro.product.manufacturer; getprop ro.product.model")?;
            let name = out.split_whitespace().collect::<Vec<_>>().join(" ");
            Ok(format!("Телевизор на связи: {}", if name.is_empty() { "Android" } else { &name }))
        }
        TvAction::Wake => sh("input keyevent KEYCODE_WAKEUP").map(|_| "Включаю".into()),
        TvAction::Sleep => sh("input keyevent KEYCODE_SLEEP").map(|_| "Выключаю".into()),
        TvAction::Key(code, n) => {
            let keys = vec![*code; usize::from(*n)].join(" ");
            sh(&format!("input keyevent {keys}")).map(|_| "Готово".into())
        }
        TvAction::Search(q) => {
            // «Алиса, найди фильм Колобок»: the TV on first, then the search (#SY-F351993A).
            wake_if_asleep(&sh);
            let app = SEARCH_APPS.lock().unwrap().get(host).cloned().unwrap_or_default();
            if !app.is_empty() && search_in_app(&sh, &app, q) {
                return Ok(format!("Ищу {q} в {}", label(&app)));
            }
            sh(&global_search(q)).map(|_| format!("Ищу {q}"))
        }
        TvAction::SearchIn(app, q) => {
            wake_if_asleep(&sh);
            let list = sh("pm list packages")?;
            let packages: Vec<String> =
                list.lines().filter_map(|l| l.trim().strip_prefix("package:")).map(str::to_owned).collect();
            match find_package(app, &packages) {
                Some(pkg) if search_in_app(&sh, &pkg, q) => Ok(format!("Ищу {q} в {}", label(&pkg))),
                Some(_) => sh(&global_search(q)).map(|_| format!("Ищу {q}")),
                None => Err(format!("Не нашла на телевизоре приложение {app}")),
            }
        }
        TvAction::SearchInPkg(pkg, q) => {
            wake_if_asleep(&sh);
            if search_in_app(&sh, &pkg, q) {
                Ok(format!("Ищу {q} в {}", label(&pkg)))
            } else {
                sh(&global_search(q)).map(|_| format!("Ищу {q}"))
            }
        }
        TvAction::YouTube(q) => {
            let url = format!("https://www.youtube.com/results?search_query={}", percent_encode(q));
            let out = sh(&format!("am start -a android.intent.action.VIEW -d {}", adb::quote(&url)))?;
            if out.contains("Error") {
                // No app takes the link: the TV's own search, which includes YouTube.
                sh(&format!("am start -a android.search.action.GLOBAL_SEARCH --es query {}", adb::quote(q)))?;
            }
            Ok(format!("Ищу на ютубе {q}"))
        }
        TvAction::Apps => {
            // Apps with a TV launcher entry (what the TV's home screen shows).
            // `query-activities` stalls on some TVs (the read comes back as EAGAIN,
            // «os error 11»): then the plain package list, which is enough to pick an app.
            let queried = sh("cmd package query-activities --brief -a android.intent.action.MAIN -c android.intent.category.LEANBACK_LAUNCHER 2>/dev/null");
            let mut apps = queried.as_ref().map(|out| packages_of(out)).unwrap_or_default();
            if apps.is_empty() {
                if let Err(e) = &queried {
                    eprintln!("tv: launcher list skipped: {e}");
                }
                let list = match sh("pm list packages -3") {
                    Ok(list) => list,
                    Err(e) => return Err(e),
                };
                apps = list.lines().filter_map(|l| l.trim().strip_prefix("package:")).map(str::to_owned).collect();
            }
            apps.sort();
            apps.dedup();
            let names: serde_json::Map<String, serde_json::Value> = apps.iter().map(|a| (a.clone(), label(a).into())).collect();
            Ok(serde_json::json!({ "apps": apps, "names": names }).to_string())
        }
        TvAction::ImeList => {
            let enabled = sh("settings get secure enabled_input_methods").unwrap_or_default();
            let current = sh("settings get secure default_input_method").unwrap_or_default();
            let ids: Vec<String> = enabled
                .trim()
                .split(':')
                .map(str::to_owned)
                .filter(|s| s.contains('/'))
                .collect();
            Ok(serde_json::json!({ "imes": ids, "active": current.trim() }).to_string())
        }
        TvAction::ImeEnable(id) => {
            if !valid_ime(&id) {
                return Err("неверный id клавиатуры".into());
            }
            sh(&format!("ime enable {}", adb::quote(&id)))?;
            Ok(format!("Клавиатура включена: {id}"))
        }
        TvAction::ImeDisable(id) => {
            if !valid_ime(&id) {
                return Err("неверный id клавиатуры".into());
            }
            sh(&format!("ime disable {}", adb::quote(&id)))?;
            Ok(format!("Клавиатура отключена: {id}"))
        }
        TvAction::ImeSet(id) => {
            if !valid_ime(&id) {
                return Err("неверный id клавиатуры".into());
            }
            sh(&format!("ime set {}", adb::quote(&id)))?;
            Ok(format!("Клавиатура выбрана: {id}"))
        }
        TvAction::KeyboardPrepare => {
            // ADBKeyBoard types Cyrillic over ADB; the phone-keyboard bridge
            // overrides every `ime set`, so it must be disabled for our
            // search path to work on this TV.
            let out = sh("pm list packages | grep -i adbkey").unwrap_or_default();
            if !out.contains("com.android.adbkeyboard") {
                return Err("ADBKeyBoard не установлен на этом ТВ — см. инструкцию в панели".into());
            }
            let _ = sh("settings put secure enabled_input_methods \"com.google.android.katniss/com.google.android.voicesearch.ime.VoiceInputMethodService:com.google.android.inputmethod.latin/com.android.inputmethod.latin.LatinIME:com.android.adbkeyboard/.AdbIME\"");
            let bridge = "com.google.android.tv.remote.service";
            if sh(&format!("pm list packages | grep -i {}", bridge))
                .unwrap_or_default()
                .contains(bridge)
            {
                let _ = sh(&format!("pm uninstall -k --user 0 {}", bridge));
            }
            Ok("Клавиатура подготовлена: ADBKeyBoard включён, ввод с телефона отключён".into())
        }
        TvAction::OpenPackage(pkg) | TvAction::OpenOn(pkg, _) => {
            if !valid_package(pkg) {
                return Err("неверное имя пакета".into());
            }
            wake_if_asleep(&sh);
            let out = sh(&launch_script(pkg))?;
            if out.contains("Error") || out.contains("No activities") || out.contains("Exception") {
                return Err(format!("Телевизор не открыл {pkg}"));
            }
            Ok("Открываю".into())
        }
        TvAction::Close(name) => {
            let pkg = if valid_package(name) && name.contains('.') {
                name.clone()
            } else {
                let list = sh("pm list packages")?;
                let packages: Vec<String> =
                    list.lines().filter_map(|l| l.trim().strip_prefix("package:")).map(str::to_owned).collect();
                find_package(name, &packages).ok_or_else(|| format!("Не нашла на телевизоре приложение {name}"))?
            };
            sh(&format!("am force-stop {pkg}")).map(|_| format!("Закрыла {name}"))
        }
        // Handled by the controller (no TV involved).
        TvAction::RemoteMode(_) => Ok(String::new()),
        TvAction::Unclear => Err("Не поняла, что сделать на проекторе. Скажите, например: включи ютуб на проекторе, тише на пять, пауза".into()),
        TvAction::Tap(what) => {
            let nodes = dump_screen(&sh)?;
            match find_on_screen(&nodes, what) {
                Some(n) => {
                    tap(&sh, n)?;
                    Ok(format!("Нажала {}", n.label))
                }
                None => {
                    eprintln!("tv: «{what}» not on the screen; seen: {}", seen(&nodes));
                    Err(format!("Не вижу на экране «{what}»"))
                }
            }
        }
        TvAction::OpenIn(app, section) => {
            open_app(&sh, app)?;
            // The app's first screen takes a few seconds.
            for attempt in 0..4 {
                std::thread::sleep(Duration::from_millis(if attempt == 0 { 3500 } else { 1500 }));
                let Ok(nodes) = dump_screen(&sh) else { continue };
                if let Some(n) = find_on_screen(&nodes, section) {
                    tap(&sh, n)?;
                    return Ok(format!("Открываю {section}"));
                }
                if attempt == 3 {
                    eprintln!("tv: section «{section}» not found in {app}; seen: {}", seen(&nodes));
                }
            }
            Err(format!("Открыла {app}, но не нашла там «{section}»"))
        }
        TvAction::PlayFirst => {
            let nodes = dump_screen(&sh)?;
            match first_item(&nodes) {
                Some(n) => {
                    tap(&sh, n)?;
                    Ok(format!("Включаю {}", n.label.chars().take(60).collect::<String>()))
                }
                None => {
                    // Nothing recognisable: the focused item, as the remote's OK would.
                    eprintln!("tv: no first item; seen: {}", seen(&nodes));
                    sh("input keyevent KEYCODE_DPAD_CENTER").map(|_| "Включаю".into())
                }
            }
        }
        TvAction::OpenAndStart(app) => {
            open_app(&sh, app)?;
            std::thread::sleep(Duration::from_millis(3500));
            let nodes = dump_screen(&sh).unwrap_or_default();
            if let Some(n) = START_WORDS.iter().find_map(|w| find_on_screen(&nodes, w)) {
                tap(&sh, n)?;
                return Ok(format!("Открыла {app} и нажала {}", n.label));
            }
            // Most TV VPN apps open with the big button in focus.
            eprintln!("tv: no start button in {app}; seen: {}", seen(&nodes));
            sh("input keyevent KEYCODE_DPAD_CENTER").map(|_| format!("Открыла {app} и нажала главную кнопку"))
        }
        TvAction::Open(name) => {
            wake_if_asleep(&sh);
            let list = sh("pm list packages")?;
            let packages: Vec<String> =
                list.lines().filter_map(|l| l.trim().strip_prefix("package:")).map(str::to_owned).collect();
            let Some(pkg) = find_package(name, &packages) else {
                eprintln!("tv: no app for {name:?} among {} packages", packages.len());
                return Err(format!("Не нашла на телевизоре приложение {name}"));
            };
            let out = sh(&launch_script(&pkg))?;
            eprintln!("tv: open {name:?} -> {pkg}: {}", out.trim().replace('\n', " | "));
            if out.contains("Error") || out.contains("No activities") || out.contains("Exception") {
                return Err(format!("Телевизор не открыл {name}"));
            }
            Ok(format!("Открываю {name}"))
        }
    }
}

/// «com.google.android.youtube.tv/…» lines → packages.
fn packages_of(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| l.trim().split_once('/').map(|(p, _)| p.trim().to_owned()))
        .filter(|p| valid_package(p))
        .collect()
}

/// «Искать фильмы в», per TV (address → package); a TV not here: its own search.
static SEARCH_APPS: std::sync::Mutex<std::collections::BTreeMap<String, String>> = std::sync::Mutex::new(std::collections::BTreeMap::new());

pub fn set_search_apps(apps: std::collections::BTreeMap<String, String>) {
    *SEARCH_APPS.lock().unwrap() = apps;
}

/// Whether the panel chose an app for «найди фильм …».
pub fn search_app_configured() -> bool {
    SEARCH_APPS.lock().unwrap().values().any(|a| !a.is_empty())
}

/// The search inside the chosen app (its SEARCH screen with the query), or the TV's
/// own search when none is chosen or the app does not take it.
fn search_script(app: &str, q: &str) -> Option<String> {
    (!app.is_empty() && valid_package(app))
        .then(|| format!("am start -a android.intent.action.SEARCH -p {app} --es query {}", adb::quote(q)))
}

fn global_search(q: &str) -> String {
    format!("am start -a android.search.action.GLOBAL_SEARCH --es query {}", adb::quote(q))
}

/// Asleep TVs are woken first, and given a moment. A screen that is already on is
/// left alone: on some boxes KEYCODE_WAKEUP then starts the screensaver.
fn wake_if_asleep(sh: &dyn Fn(&str) -> Result<String, String>) {
    let power = sh("dumpsys power | grep -E 'mWakefulness=|Display Power'").unwrap_or_default();
    let awake = power.lines().any(|l| l.contains("mWakefulness=") && l.contains("Awake"));
    let screen_off = power.contains("state=OFF");
    if !power.is_empty() && (!awake || screen_off) {
        let _ = sh("input keyevent KEYCODE_WAKEUP");
        std::thread::sleep(Duration::from_millis(1500));
    }
}

/// The search inside `pkg`: Android's search screen, else the app's own search page as a link
/// (RUTUBE has no search screen: «найди маму в 16» opened the TV's general search). false: neither.
fn search_in_app(sh: &dyn Fn(&str) -> Result<String, String>, pkg: &str, q: &str) -> bool {
    // Some apps search in their own screen (LazyMedia Deluxe): the standard SEARCH
    // intent goes to a different one that only opens an article. Start theirs with
    // the query — and from a fresh start, or the running one keeps the old query.
    if let Some((_, activity)) = OWN_SEARCH.iter().find(|(p, _)| *p == pkg) {
        let cmd = format!("am force-stop {pkg}; sleep 1; am start -n {pkg}/{activity} --es query {}", adb::quote(q));
        // `am start` prints the refusal to stderr, which the ADB shell does not
        // return: a successful-looking answer is not proof that the search
        // opened. Check the window focus instead.
        let opened = match sh(&cmd) {
            Ok(out) => out.contains("Starting: Intent"),
            Err(e) => {
                eprintln!("tv: {pkg} search: {e}");
                false
            }
        };
        let focused = opened && sh("dumpsys window | grep mCurrentFocus")
            .map(|f| f.contains("ActivityTvSearch"))
            .unwrap_or(false);
        if focused {
            return true;
        }
        // On a TV where adb runs as `shell` (not root), the app's own search
        // activity is not exported: `am start -n` is refused with
        // «Permission Denial … not exported». Open the app with `monkey`,
        // press its search key, focus the field and type via ADBKeyBoard.
        // `input text` cannot type Cyrillic, but ADBKeyBoard's B64 broadcast
        // can — so the query keeps the exact Russian letters Alice heard.
        if let Some(payload) = keyboard_payload(q) {
            let open = format!("monkey -p {pkg} 1 >/dev/null 2>&1");
            // Save the IME the TV was using, switch to ADBKeyBoard for the
            // Unicode broadcast, then restore it. If ADBKeyBoard was already
            // active (e.g. a previous run left it), fall back to the TV's
            // standard ImeBridgeService instead of leaving our helper on.
            // Pick the TV's own keyboard dynamically: B866 uses
            // com.google.android.inputmethod.latin/.LatinIME, X3 uses the
            // older com.android.inputmethod.latin/.LatinIME. Never hardcode.
            // The first non-helper IME in the enabled list is the stock one.
            let script = format!(
                "OLD=$(settings get secure default_input_method 2>/dev/null); \
                 STOCK=$(ime list -s 2>/dev/null | head -1); \
                 case \"$OLD\" in *adbkeyboard*|*ImeBridgeService*|*ugoos*) OLD=\"$STOCK\" ;; esac; \
                 ime enable com.android.adbkeyboard/.AdbIME >/dev/null 2>&1; \
                 ime set com.android.adbkeyboard/.AdbIME >/dev/null 2>&1; \
                 {open}; sleep 3; input tap 980 113; sleep 2; \
                 am broadcast -a ADB_INPUT_B64 --es msg {}; \
                 sleep 2; \
                 input keyevent 4; \
                 sleep 1; \
                 if [ -n \"$OLD\" ] && [ \"$OLD\" != \"null\" ]; then \
                   settings put secure default_input_method \"$OLD\" >/dev/null 2>&1; \
                   ime set \"$OLD\" >/dev/null 2>&1; \
                 fi",
                adb::quote(&payload)
            );
            match sh(&script) {
                Ok(_) => {
                    let focused = sh("dumpsys window | grep mCurrentFocus")
                        .map(|f| f.contains("ActivityTvSearch"))
                        .unwrap_or(false);
                    if focused {
                        eprintln!("tv: {pkg}: search opened via ADBKeyBoard ({q})");
                        return true;
                    }
                    eprintln!("tv: {pkg}: keyboard search did not open the search screen");
                }
                Err(e) => eprintln!("tv: {pkg}: keyboard search failed: {e}"),
            }
        }
    }
    if let Some(cmd) = search_script(pkg, q) {
        match sh(&cmd) {
            Ok(out) if !out.contains("Error") => return true,
            Ok(out) => eprintln!("tv: {pkg} has no search screen ({})", out.trim().replace('\n', " | ")),
            Err(e) => eprintln!("tv: {pkg} search: {e}"),
        }
    }
    if let Some(url) = label_of_package(pkg).and_then(|l| search_url(l, q)) {
        match sh(&format!("am start -a android.intent.action.VIEW -d {} -p {pkg}", adb::quote(&url))) {
            Ok(out) if !out.contains("Error") => {
                eprintln!("tv: {pkg}: its search page {url}");
                return true;
            }
            Ok(out) => eprintln!("tv: {pkg} does not open its search page ({}); the TV's own search", out.trim().replace('\n', " | ")),
            Err(e) => eprintln!("tv: {pkg} search page: {e}"),
        }
    }
    false
}

/// ADBKeyBoard's B64 broadcast carries the query in the exact letters Alice
/// heard — no transliteration. Standard base64 of the query's UTF-8 bytes.
fn keyboard_payload(q: &str) -> Option<String> {
    let b = crate::b64::base64(q.as_bytes());
    (!b.is_empty()).then_some(b)
}

/// A package name safe to put into a shell command.
pub fn valid_package(p: &str) -> bool {
    !p.is_empty() && p.len() < 200 && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
}

/// An IME id like `com.pkg/.Service`: only safe characters in a shell arg.
fn valid_ime(id: &str) -> bool {
    !id.is_empty()
        && id.len() < 200
        && id.chars().all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c))
}

/// Starts an app's main screen: the TV launcher entry (Android TV apps), else the
/// phone launcher entry, else whatever monkey finds.
fn launch_script(pkg: &str) -> String {
    let resolve = |cat: &str| {
        format!("cmd package resolve-activity --brief -a android.intent.action.MAIN -c android.intent.category.{cat} {pkg} 2>/dev/null | tail -n 1")
    };
    format!(
        "c=$({}); case \"$c\" in */*) ;; *) c=$({});; esac; \
         case \"$c\" in */*) am start -n \"$c\" 2>&1 ;; *) monkey -p {pkg} 1 2>&1 ;; esac; \
         f=$(dumpsys window 2>/dev/null | grep mCurrentFocus); \
         case \"$f\" in *:dream*) service call dreams 3 >/dev/null 2>&1 ;; esac",
        resolve("LEANBACK_LAUNCHER"),
        resolve("LAUNCHER")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // A SmartTube-like screen: the menu on the left, a grid of videos.
    const SCREEN: &str = r#"<?xml version='1.0'?><hierarchy rotation="0">
<node index="0" text="" class="android.widget.FrameLayout" content-desc="" clickable="false" focusable="false" bounds="[0,0][1920,1080]">
<node text="Главная" content-desc="" clickable="true" focusable="true" bounds="[20,200][300,260]" />
<node text="История" content-desc="" clickable="true" focusable="true" bounds="[20,280][300,340]" />
<node text="" content-desc="Поиск" clickable="true" focusable="true" bounds="[20,120][300,180]" />
<node text="Как приготовить плов &amp; не сжечь" content-desc="" clickable="true" focusable="true" bounds="[420,200][900,520]" />
<node text="Обзор проектора" content-desc="" clickable="true" focusable="true" bounds="[940,200][1420,520]" />
<node text="Вторая строка" content-desc="" clickable="true" focusable="true" bounds="[420,560][900,880]" />
</node></hierarchy>"#;

    #[test]
    fn reads_the_screen_and_finds_what_was_said() {
        let nodes = ui_nodes(SCREEN);
        assert_eq!(nodes.len(), 7);
        assert_eq!(nodes[4].label, "Как приготовить плов & не сжечь");
        assert_eq!(nodes[3].label, "Поиск", "content-desc when there is no text");
        assert_eq!(find_on_screen(&nodes, "историю").unwrap().label, "История", "the case Alice heard");
        assert_eq!(find_on_screen(&nodes, "история просмотров").unwrap().label, "История");
        assert_eq!(find_on_screen(&nodes, "поиск").unwrap().center(), (160, 150));
        assert_eq!(find_on_screen(&nodes, "обзор").unwrap().label, "Обзор проектора");
        assert!(find_on_screen(&nodes, "настройки").is_none());
        assert_eq!(first_item(&nodes).unwrap().label, "Как приготовить плов & не сжечь", "top left of the grid, not the menu");
    }

    #[test]
    fn smarttube_and_vpn_by_name() {
        let pk = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let installed = pk(&["com.google.android.youtube.tv", "com.liskovsoft.smarttubetv.beta", "org.amnezia.vpn"]);
        assert_eq!(find_package("смарт тьюб", &installed).as_deref(), Some("com.liskovsoft.smarttubetv.beta"));
        assert_eq!(find_package("впн", &installed).as_deref(), Some("org.amnezia.vpn"));
        assert!(START_WORDS.iter().any(|w| find_on_screen(&ui_nodes(r#"<node text="Подключиться" clickable="true" bounds="[800,500][1100,600]" />"#), w).is_some()));
    }

    #[test]
    fn packages_get_names_people_know() {
        // The scenario editor listed bare package names; nobody could tell what opens.
        assert_eq!(label("com.google.android.youtube.tv"), "YouTube");
        assert_eq!(label("com.google.android.youtube.tvmusic"), "YouTube Music");
        assert_eq!(label("ru.kinopoisk.tv"), "Кинопоиск");
        assert_eq!(label("com.android.vending"), "Google Play");
        assert_eq!(label("com.example.wink.tv"), "Wink");
        // Unknown: the telling part of the name, the package after it.
        assert_eq!(label("com.teamsmart.videomanager.tv"), "Videomanager (com.teamsmart.videomanager.tv)");
        assert_eq!(label("org.example.app"), "Example (org.example.app)");
        assert_eq!(label("com.lazycatsoftware.lmd"), "LazyMedia Deluxe");
        assert_eq!(label("ru.cn.atx"), "Free TV");
    }

    #[test]
    fn finds_apps_by_spoken_name() {
        let pk: Vec<String> = [
            "com.google.android.youtube.tv",
            "ru.kinopoisk.tv",
            "ru.ivi.client",
            "com.netflix.ninja",
            "org.videolan.vlc",
            "com.android.tv.settings",
            "tv.okko.androidtv",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        assert_eq!(find_package("ютуб", &pk).as_deref(), Some("com.google.android.youtube.tv"));
        assert_eq!(find_package("кинопоиск", &pk).as_deref(), Some("ru.kinopoisk.tv"));
        assert_eq!(find_package("иви", &pk).as_deref(), Some("ru.ivi.client"));
        assert_eq!(find_package("нетфликс", &pk).as_deref(), Some("com.netflix.ninja"));
        assert_eq!(find_package("влц", &pk).as_deref(), Some("org.videolan.vlc"));
        assert_eq!(find_package("настройки", &pk).as_deref(), Some("com.android.tv.settings"));
        assert_eq!(find_package("окко", &pk).as_deref(), Some("tv.okko.androidtv"));
        assert_eq!(find_package("твич", &pk), None);
        // The apps of this TV: their own names.
        let pk2: Vec<String> = ["com.lazycatsoftware.lmd", "ru.cn.atx"].iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(find_package("делюкс", &pk2).as_deref(), Some("com.lazycatsoftware.lmd"));
        assert_eq!(find_package("лмд", &pk2).as_deref(), Some("com.lazycatsoftware.lmd"));
        assert_eq!(find_package("фри ти ви", &pk2).as_deref(), Some("ru.cn.atx"));
        // Xiaomi L1 Pro lists YouTube Music before YouTube.
        let pk: Vec<String> =
            ["com.google.android.youtube.tvmusic", "com.google.android.youtube.tv"].iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(find_package("youtube", &pk).as_deref(), Some("com.google.android.youtube.tv"));
        assert_eq!(find_package("ютуб", &pk).as_deref(), Some("com.google.android.youtube.tv"));
        assert_eq!(find_package("ютуб музыку", &pk).as_deref(), Some("com.google.android.youtube.tvmusic"));
        let only_music: Vec<String> = vec!["com.google.android.youtube.tvmusic".into()];
        assert_eq!(find_package("ютуб", &only_music), None);
    }


    #[test]
    fn the_app_named_at_the_end_of_the_phrase() {
        assert_eq!(split_app_suffix("найди фильм колобок на делюкс"),
                   Some(("найди фильм колобок".into(), "делюкс".into())));
        assert_eq!(split_app_suffix("найди фильм колобок на делюкс на телевизоре"),
                   Some(("найди фильм колобок на делюкс".into(), "телевизоре".into())));
        assert_eq!(split_app_suffix("найди фильм колобок"), None);
    }

    #[test]
    fn keyboard_payload_is_utf8_base64() {
        // «колобок» in UTF-8 = 0LrQvtC70L7QsdC+0Lo= — the exact broadcast
        // ADBKeyBoard accepted on the non-root MTS box.
        assert_eq!(keyboard_payload("колобок"), Some("0LrQvtC70L7QsdC+0Lo=".into()));
        assert_eq!(keyboard_payload(""), None);
    }

    #[test]
    fn a_search_in_a_named_video_app() {
        assert_eq!(search_in_phrase("найди маму в 16 на рутубе"), Some(("рутубе".into(), "маму в 16".into())));
        assert_eq!(search_in_phrase("найди фильм дюна в кинопоиске"), Some(("кинопоиске".into(), "дюна".into())));
        assert_eq!(search_in_phrase("поищи стрим на твиче"), Some(("твиче".into(), "стрим".into())));
        assert_eq!(search_in_phrase("найди маму в 16 на пикабу"), None, "not an app it knows");
        // Music stays Alice's; places and numbers are not apps.
        assert_eq!(search_in_phrase("включи bonobo в яндекс музыке"), None);
        assert_eq!(search_in_phrase("найди кафе в москве"), None);
        assert_eq!(search_in_phrase("найди маму в 16"), None);
        assert_eq!(search_url("RUTUBE", "маму в 16").as_deref(), Some("https://rutube.ru/search/?query=%D0%BC%D0%B0%D0%BC%D1%83+%D0%B2+16"));
        assert_eq!(label_of_package("ru.rutube.app"), Some("RUTUBE"));
    }

    #[test]
    fn searches_in_the_chosen_app() {
        let script = search_script("ru.kinopoisk.tv", "колобок 2").unwrap();
        assert_eq!(script, "am start -a android.intent.action.SEARCH -p ru.kinopoisk.tv --es query 'колобок 2'");
        assert!(search_script("", "колобок").is_none(), "no app: the TV's own search");
        assert!(search_script("x; reboot", "колобок").is_none(), "only a package name goes into the shell");
        for s in [script, global_search("it's")] {
            let ok = std::process::Command::new("sh").arg("-n").arg("-c").arg(&s).status().unwrap().success();
            assert!(ok, "{s}");
        }
    }

    #[test]
    fn launch_script_is_valid_shell() {
        let script = launch_script("com.google.android.youtube.tv");
        assert!(script.contains("LEANBACK_LAUNCHER com.google.android.youtube.tv"));
        let ok = std::process::Command::new("sh").arg("-n").arg("-c").arg(&script).status().unwrap().success();
        assert!(ok, "{script}");
    }

    #[test]
    fn latin_names_said_in_russian() {
        let pk = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let installed = pk(&["ru.zona.tv", "com.mxtech.videoplayer.ad", "tv.twitch.android.app", "com.amazon.avod"]);
        assert_eq!(find_package("зону", &installed).as_deref(), Some("ru.zona.tv"));
        assert_eq!(find_package("zona", &installed).as_deref(), Some("ru.zona.tv"));
        assert_eq!(find_package("зона", &installed).as_deref(), Some("ru.zona.tv"));
        assert_eq!(find_package("зону", &pk(&["com.amazon.avod"])), None, "«zon» must not match amazon");
    }

    #[test]
    fn reads_launcher_apps_safely() {
        let out = "priority=0 preferredOrder=0\ncom.google.android.youtube.tv/com.google.android.apps.youtube.tv.activity.ShellActivity\nru.kinopoisk.tv/.MainActivity\nbad;rm -rf/x\n";
        assert_eq!(packages_of(out), vec!["com.google.android.youtube.tv", "ru.kinopoisk.tv"]);
        assert!(!valid_package("a;b") && !valid_package("") && valid_package("tv.okko.androidtv"));
    }

    #[test]
    fn transliterates_and_encodes() {
        assert_eq!(translit("лампа"), "lampa");
        assert_eq!(percent_encode("котики 2"), "%D0%BA%D0%BE%D1%82%D0%B8%D0%BA%D0%B8+2");
    }
}
