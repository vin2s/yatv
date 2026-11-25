//! Google Calendar and Google Tasks, both ways, with the user's own OAuth
//! client: YATV ships no Google keys (nothing secret in the sources).
//!
//! Setup, once, in the panel («Планы» → «Google»): the user makes an OAuth
//! client of type «Desktop app» in their Google Cloud project (Calendar API
//! and Tasks API on), pastes its ID and secret, presses «Войти в Google»,
//! signs in, and pastes back the address the browser ended on
//! (http://127.0.0.1:53682/yatv?code=… — the page itself does not open, that
//! is expected). PKCE (RFC 7636) protects the code. The refresh token is a
//! secret like any token: config.json only, masked on the page.

use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::planner::{civil, days_from_civil, local};

pub const REDIRECT: &str = "http://127.0.0.1:53682/yatv";
const SCOPES: &str =
    "https://www.googleapis.com/auth/calendar.readonly https://www.googleapis.com/auth/calendar.events https://www.googleapis.com/auth/tasks";

fn auth_base() -> String {
    std::env::var("YATV_GOOGLE_AUTH").unwrap_or_else(|_| "https://accounts.google.com".into())
}
fn token_url() -> String {
    std::env::var("YATV_GOOGLE_OAUTH").map(|b| format!("{b}/token")).unwrap_or_else(|_| "https://oauth2.googleapis.com/token".into())
}
fn api_base() -> String {
    std::env::var("YATV_GOOGLE_API").unwrap_or_else(|_| "https://www.googleapis.com".into())
}

/// The speaker has no proxy settings panel: a simple agent with ureq's
/// bundled webpki roots. `YATV_PROXY` is optional (http://… or socks5://…).
pub fn agent() -> Result<ureq::Agent, String> {
    let mut builder = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30));
    if let Ok(proxy) = std::env::var("YATV_PROXY") {
        if !proxy.trim().is_empty() {
            builder = builder.proxy(ureq::Proxy::new(proxy.trim()).map_err(|e| e.to_string())?);
        }
    }
    Ok(builder.build())
}

#[derive(Clone, Default, PartialEq)]
pub struct Config {
    pub client_id: String,
    pub secret: String,
    pub refresh: String,
}

static CFG: Mutex<Option<Config>> = Mutex::new(None);
static TOKEN: Mutex<Option<(String, Instant)>> = Mutex::new(None);
static PENDING: Mutex<Option<(String, String, Instant)>> = Mutex::new(None);
/// Device flow: (device_code, interval, expires_at, client_id, secret).
static DEVICE: Mutex<Option<(String, u64, Instant, String, String)>> = Mutex::new(None);

pub fn configure(c: Config) {
    let mut g = CFG.lock().unwrap();
    if g.as_ref() != Some(&c) {
        *TOKEN.lock().unwrap() = None;
    }
    *g = Some(c);
}

fn cfg() -> Config {
    CFG.lock().unwrap().clone().unwrap_or_default()
}

pub fn connected() -> bool {
    let c = cfg();
    !c.refresh.is_empty() && !c.client_id.is_empty()
}

// ------------------------------------------------------------ device flow

/// RFC 8628: the endpoint where a browserless device asks for a user code.
pub fn device_code_url() -> String {
    std::env::var("YATV_GOOGLE_DEVICE").unwrap_or_else(|_| "https://oauth2.googleapis.com/device/code".into())
}

/// What the panel shows: the short code and the URL to open on the phone/PC.
#[derive(Debug, Clone)]
pub struct DevicePrompt {
    pub user_code: String,
    pub verification_url: String,
}

/// Start the device flow. `client_id` must be a «TV and Limited Input» or
/// «Desktop app» OAuth client of the user's own project. Returns what to say.
pub fn device_start(a: &ureq::Agent, client_id: &str, secret: &str) -> Result<DevicePrompt, String> {
    // Device code request carries only client_id + scope (RFC 8628). Adding a
    // client_secret here makes Google answer invalid_client for Desktop apps.
    let v: Value = a.post(&device_code_url()).timeout(Duration::from_secs(20))
        .send_form(&[("client_id", client_id.trim()), ("scope", SCOPES)])
        .map_err(google_error)?.into_json().map_err(|e| e.to_string())?;
    let code = v["device_code"].as_str().ok_or("Google не выдал код устройства")?.to_owned();
    let user_code = v["user_code"].as_str().unwrap_or("").to_owned();
    let url = v["verification_url"].as_str().unwrap_or("https://www.google.com/device").to_owned();
    let interval = v["interval"].as_u64().unwrap_or(5).clamp(1, 30);
    let expires = v["expires_in"].as_u64().unwrap_or(1800);
    *DEVICE.lock().unwrap() = Some((code, interval, Instant::now() + Duration::from_secs(expires), client_id.trim().to_owned(), secret.trim().to_owned()));
    Ok(DevicePrompt { user_code, verification_url: url })
}

/// Poll once. `Ok(None)` = the user has not approved yet (call again after
/// `interval` seconds); `Ok(Some(refresh))` = signed in; `Err` = stop.
pub fn device_poll(a: &ureq::Agent) -> Result<Option<String>, String> {
    let (device_code, interval, until, client_id, secret) = DEVICE.lock().unwrap().clone()
        .ok_or("сначала нажмите «Войти через код»")?;
    if Instant::now() >= until {
        *DEVICE.lock().unwrap() = None;
        return Err("код устарел — нажмите «Войти через код» ещё раз".into());
    }
    // Honour Google's interval: an early poll gets `slow_down`.
    thread::sleep(Duration::from_secs(interval));
    let v: Value = match a.post(&token_url()).timeout(Duration::from_secs(20)).send_form(&[
        ("client_id", client_id.trim()), ("device_code", device_code.as_str()),
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ("client_secret", secret.trim()),
    ]) {
        Ok(r) => r.into_json().map_err(|e| e.to_string())?,
        Err(ureq::Error::Status(code, resp)) => {
            let e: Value = resp.into_json().unwrap_or(Value::Null);
            let kind = e["error"].as_str().unwrap_or("");
            return match kind {
                "authorization_pending" => Ok(None),
                "slow_down" => {
                    let mut d = DEVICE.lock().unwrap();
                    if let Some(x) = d.as_mut() { x.1 += 5; }
                    Ok(None)
                }
                "expired_token" => {
                    *DEVICE.lock().unwrap() = None;
                    Err("код устарел — нажмите «Войти через код» ещё раз".into())
                }
                "access_denied" => {
                    *DEVICE.lock().unwrap() = None;
                    Err("вход отменён в Google".into())
                }
                _ => Err(format!("Google ответил {code}: {kind}")),
            };
        }
        Err(e) => return Err(google_error(e)),
    };
    let refresh = v["refresh_token"].as_str().ok_or("Google не выдал постоянный вход")?.to_owned();
    if let Some(t) = v["access_token"].as_str() {
        let ttl = v["expires_in"].as_u64().unwrap_or(3600).saturating_sub(120);
        *TOKEN.lock().unwrap() = Some((t.to_owned(), Instant::now() + Duration::from_secs(ttl)));
    }
    *DEVICE.lock().unwrap() = None;
    Ok(Some(refresh))
}

pub fn device_cancel() {
    *DEVICE.lock().unwrap() = None;
}

pub fn device_active() -> bool {
    DEVICE.lock().unwrap().is_some()
}

// ---------------------------------------------------------------- encoding

pub fn b64url(data: &[u8]) -> String {
    crate::b64::base64(data).replace('+', "-").replace('/', "_").trim_end_matches('=').to_owned()
}

/// RFC 7636: BASE64URL(SHA256(verifier)).
pub fn challenge(verifier: &str) -> String {
    b64url(&crate::sha256::sha256(verifier.as_bytes()))
}

fn random(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut b))
        .is_err()
    {
        let seed = format!("{:?}{:p}", std::time::SystemTime::now(), &b);
        let h = crate::sha256::sha256(seed.as_bytes());
        for (i, x) in b.iter_mut().enumerate() {
            *x = h[i % 32];
        }
    }
    b
}

pub fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn dec(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 <= b.len() - 1 && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() {
            let hex = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
            out.push(hex(b[i + 1]) * 16 + hex(b[i + 2]));
            i += 3;
            continue;
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// RFC 3339 → unix time.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let s = s.trim();
    let n = |a: usize, b: usize| s.get(a..b).and_then(|x| x.parse::<i64>().ok());
    let (y, mo, d) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    if s.as_bytes().get(10).map(u8::to_ascii_uppercase) != Some(b'T') {
        return None;
    }
    let (h, mi, se) = (n(11, 13)?, n(14, 16)?, n(17, 19)?);
    let rest = &s[19..];
    let rest = rest.strip_prefix('.').map_or(rest, |r| r.trim_start_matches(|c: char| c.is_ascii_digit()));
    let off = match rest.as_bytes().first() {
        Some(b'Z' | b'z') | None => 0,
        Some(b'+' | b'-') => {
            let sign = if rest.starts_with('-') { -1 } else { 1 };
            let oh = rest.get(1..3)?.parse::<i64>().ok()?;
            let om = rest.get(4..6).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
            sign * (oh * 3600 + om * 60)
        }
        _ => return None,
    };
    Some(days_from_civil(y, mo as u32, d as u32) * 86_400 + h * 3600 + mi * 60 + se - off)
}

pub fn rfc3339(t: i64, off: i64) -> String {
    let (day, secs) = local(t, off);
    let (y, m, d) = civil(day);
    let sign = if off < 0 { '-' } else { '+' };
    let a = off.abs();
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60, a / 3600, a / 60 % 60)
}

pub fn date(day: i64) -> String {
    let (y, m, d) = civil(day);
    format!("{y:04}-{m:02}-{d:02}")
}

fn parse_date(s: &str) -> Option<i64> {
    let n = |a: usize, b: usize| s.get(a..b).and_then(|x| x.parse::<i64>().ok());
    Some(days_from_civil(n(0, 4)?, n(5, 7)? as u32, n(8, 10)? as u32))
}

// ---------------------------------------------------------------- sign-in

pub fn auth_url(client_id: &str) -> String {
    let verifier = b64url(&random(48));
    let state = b64url(&random(12));
    let url = format!(
        "{}/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent&code_challenge={}&code_challenge_method=S256&state={}",
        auth_base(),
        enc(client_id.trim()),
        enc(REDIRECT),
        enc(SCOPES),
        challenge(&verifier),
        state
    );
    *PENDING.lock().unwrap() = Some((verifier, state, Instant::now()));
    url
}

pub fn code_from(pasted: &str) -> Option<(String, Option<String>)> {
    let p = pasted.trim();
    if p.is_empty() {
        return None;
    }
    if let Some(q) = p.split_once('?').map(|x| x.1).or_else(|| p.contains("code=").then_some(p)) {
        let mut code = None;
        let mut state = None;
        for kv in q.split(['&', '#']) {
            match kv.split_once('=') {
                Some(("code", v)) => code = Some(dec(v)),
                Some(("state", v)) => state = Some(dec(v)),
                Some(("error", v)) => return Some((format!("error:{}", dec(v)), None)),
                _ => {}
            }
        }
        return code.filter(|c| !c.is_empty()).map(|c| (c, state));
    }
    (!p.contains(char::is_whitespace) && !p.contains("://") && p.len() > 10).then(|| (p.to_owned(), None))
}

fn disabled_api(msg: &str) -> String {
    let m = msg.to_ascii_lowercase();
    let (name, id) = if m.contains("tasks") {
        ("Google Tasks API", "tasks.googleapis.com")
    } else if m.contains("calendar") {
        ("Google Calendar API", "calendar-json.googleapis.com")
    } else {
        return "в проекте Google Cloud не включён Calendar API или Tasks API".into();
    };
    format!("в проекте Google Cloud выключен {name} — включите его: https://console.cloud.google.com/apis/library/{id}")
}

fn google_error(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let v: Value = resp.into_json().unwrap_or(Value::Null);
            let err = v["error"].as_str().map(str::to_owned).or_else(|| v["error"]["status"].as_str().map(str::to_owned)).unwrap_or_default();
            let msg = v["error_description"].as_str().or_else(|| v["error"]["message"].as_str()).unwrap_or("").chars().take(200).collect::<String>();
            match err.as_str() {
                "invalid_grant" => "вход в Google устарел или отозван — войдите заново (в режиме «Тестирование» Google отзывает вход через 7 дней)".into(),
                "invalid_client" | "unauthorized_client" => {
                    if msg.contains("Invalid client type") || msg.contains("client type") {
                        "Google не принял этот ключ для входа по коду: нужен OAuth-клиент типа «TV and Limited Input devices». Либо создайте такой клиент в проекте Google Cloud, либо войдите обычным способом через браузер (кнопка «Войти в Google»)".into()
                    } else {
                        "Google не принял ID или секрет клиента — проверьте, что клиент типа «Приложение для ПК»".into()
                    }
                },
                "PERMISSION_DENIED" if msg.contains("has not been used") || msg.contains("disabled") => disabled_api(&msg),
                _ => format!("Google ответил {code}{}{}", if err.is_empty() { "" } else { ": " }, if msg.is_empty() { err } else { msg }),
            }
        }
        ureq::Error::Transport(t) => format!("нет связи с Google ({})", t.kind()),
    }
}

pub fn exchange(a: &ureq::Agent, client_id: &str, secret: &str, pasted: &str) -> Result<String, String> {
    let (code, state) = code_from(pasted).ok_or("вставьте адрес из строки браузера целиком (он начинается с http://127.0.0.1)")?;
    if let Some(e) = code.strip_prefix("error:") {
        return Err(if e == "access_denied" { "вход отменён в Google".into() } else { format!("Google: {e}") });
    }
    let (verifier, expect) = match PENDING.lock().unwrap().clone() {
        Some((v, s, at)) if at.elapsed() < Duration::from_secs(1800) => (v, s),
        _ => return Err("сначала нажмите «Войти в Google» (ссылка действует 30 минут)".into()),
    };
    if state.as_deref().is_some_and(|s| s != expect) {
        return Err("это адрес от другой попытки входа — нажмите «Войти в Google» ещё раз".into());
    }
    let v: Value = a.post(&token_url())
        .timeout(Duration::from_secs(20))
        .send_form(&[
            ("code", code.as_str()), ("client_id", client_id.trim()), ("client_secret", secret.trim()),
            ("redirect_uri", REDIRECT), ("grant_type", "authorization_code"), ("code_verifier", verifier.as_str()),
        ])
        .map_err(google_error)?.into_json().map_err(|e| e.to_string())?;
    let refresh = v["refresh_token"].as_str().ok_or("Google не выдал постоянный вход — удалите доступ YATV в myaccount.google.com и войдите снова")?;
    if let Some(t) = v["access_token"].as_str() {
        let ttl = v["expires_in"].as_u64().unwrap_or(3600).saturating_sub(120);
        *TOKEN.lock().unwrap() = Some((t.to_owned(), Instant::now() + Duration::from_secs(ttl)));
    }
    *PENDING.lock().unwrap() = None;
    Ok(refresh.to_owned())
}

fn access(a: &ureq::Agent) -> Result<String, String> {
    if let Some((t, until)) = TOKEN.lock().unwrap().clone() {
        if Instant::now() < until {
            return Ok(t);
        }
    }
    let c = cfg();
    if c.refresh.is_empty() {
        return Err("не выполнен вход в Google".into());
    }
    let v: Value = a.post(&token_url()).timeout(Duration::from_secs(20))
        .send_form(&[
            ("client_id", c.client_id.trim()), ("client_secret", c.secret.trim()),
            ("refresh_token", c.refresh.as_str()), ("grant_type", "refresh_token"),
        ])
        .map_err(google_error)?.into_json().map_err(|e| e.to_string())?;
    let t = v["access_token"].as_str().ok_or("Google не выдал ключ доступа")?.to_owned();
    let ttl = v["expires_in"].as_u64().unwrap_or(3600).saturating_sub(120);
    *TOKEN.lock().unwrap() = Some((t.clone(), Instant::now() + Duration::from_secs(ttl)));
    Ok(t)
}

fn call(a: &ureq::Agent, method: &str, path: &str, query: &[(&str, String)], body: Option<&Value>) -> Result<Value, String> {
    for attempt in 0..2 {
        let token = access(a)?;
        let mut req = a.request(method, &format!("{}{path}", api_base())).timeout(Duration::from_secs(25))
            .set("Authorization", &format!("Bearer {token}"));
        for (k, v) in query {
            req = req.query(k, v);
        }
        let r = match body {
            Some(b) => req.send_json(b.clone()),
            None => req.call(),
        };
        match r {
            Ok(resp) => {
                let text = resp.into_string().map_err(|e| e.to_string())?;
                return Ok(if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text).map_err(|e| e.to_string())? });
            }
            Err(ureq::Error::Status(401, _)) if attempt == 0 => *TOKEN.lock().unwrap() = None,
            Err(e) => return Err(google_error(e)),
        }
    }
    Err("Google не принял вход".into())
}

// ---------------------------------------------------------------- calendars

#[derive(Debug, Clone, PartialEq)]
pub struct Cal {
    pub id: String,
    pub name: String,
    pub primary: bool,
    pub writable: bool,
    pub selected: bool,
}

pub fn parse_calendars(v: &Value) -> Vec<Cal> {
    v["items"].as_array().map(|a| a.iter().filter_map(|c| {
        Some(Cal {
            id: c["id"].as_str()?.to_owned(),
            name: c["summaryOverride"].as_str().or_else(|| c["summary"].as_str()).unwrap_or("Календарь").to_owned(),
            primary: c["primary"].as_bool().unwrap_or(false),
            writable: matches!(c["accessRole"].as_str(), Some("owner" | "writer")),
            selected: c["selected"].as_bool().unwrap_or(false) || c["primary"].as_bool().unwrap_or(false),
        })
    }).collect()).unwrap_or_default()
}

pub fn calendars(a: &ureq::Agent) -> Result<Vec<Cal>, String> {
    call(a, "GET", "/calendar/v3/users/me/calendarList", &[("maxResults", TASKS_PAGE.to_string())], None).map(|v| parse_calendars(&v))
}

pub type GEvent = (String, String, i64, i64, bool);

pub fn parse_events(v: &Value, off: i64) -> Vec<GEvent> {
    v["items"].as_array().map(|a| a.iter()
        .filter(|e| e["status"].as_str() != Some("cancelled"))
        .filter_map(|e| {
            let title = e["summary"].as_str().unwrap_or("Событие").trim().to_owned();
            let id = e["id"].as_str()?.to_owned();
            if let Some(s) = e["start"]["dateTime"].as_str() {
                let start = parse_rfc3339(s)?;
                let end = e["end"]["dateTime"].as_str().and_then(parse_rfc3339).unwrap_or(start + 3600);
                Some((id, title, start, end, false))
            } else {
                let d = parse_date(e["start"]["date"].as_str()?)?;
                let end = e["end"]["date"].as_str().and_then(parse_date).unwrap_or(d + 1);
                Some((id, title, d * 86_400 - off, end * 86_400 - off, true))
            }
        }).collect()).unwrap_or_default()
}

pub fn events(a: &ureq::Agent, cal: &str, from: i64, to: i64, off: i64) -> Result<Vec<GEvent>, String> {
    let v = call(a, "GET", &format!("/calendar/v3/calendars/{}/events", enc(cal)), &[
        ("timeMin", rfc3339(from, 0).replace("+00:00", "Z")),
        ("timeMax", rfc3339(to, 0).replace("+00:00", "Z")),
        ("singleEvents", "true".into()), ("orderBy", "startTime".into()), ("maxResults", "250".into()),
    ], None)?;
    Ok(parse_events(&v, off))
}

pub fn add_event(a: &ureq::Agent, cal: &str, title: &str, start: i64, end: i64, all_day: bool, off: i64) -> Result<String, String> {
    let body = if all_day {
        let (d, _) = local(start, off);
        json!({ "summary": title, "start": { "date": date(d) }, "end": { "date": date(d + 1) } })
    } else {
        json!({ "summary": title, "start": { "dateTime": rfc3339(start, off) }, "end": { "dateTime": rfc3339(end, off) } })
    };
    let v = call(a, "POST", &format!("/calendar/v3/calendars/{}/events", enc(cal)), &[], Some(&body))?;
    Ok(v["id"].as_str().unwrap_or("").to_owned())
}

// ---------------------------------------------------------------- tasks

#[derive(Debug, Clone, PartialEq)]
pub struct GTask {
    pub id: String,
    pub list: String,
    pub title: String,
    pub notes: String,
    /// Google keeps no time for tasks: the due date as a local day number.
    pub due_day: Option<i64>,
}

pub const TASKS_PAGE: usize = 100;

pub fn parse_lists(v: &Value) -> Vec<(String, String)> {
    v["items"].as_array().map(|a| a.iter().filter_map(|l| {
        Some((l["id"].as_str()?.to_owned(), l["title"].as_str().unwrap_or("Задачи").to_owned()))
    }).collect()).unwrap_or_default()
}

pub fn parse_tasks(v: &Value, list: &str) -> Vec<GTask> {
    v["items"].as_array().map(|a| a.iter()
        .filter(|t| t["status"].as_str() != Some("completed") && !t["deleted"].as_bool().unwrap_or(false))
        .filter_map(|t| {
            let title = t["title"].as_str()?.trim().to_owned();
            (!title.is_empty()).then(|| GTask {
                id: t["id"].as_str().unwrap_or("").to_owned(), list: list.to_owned(), title,
                notes: t["notes"].as_str().unwrap_or("").chars().take(300).collect(),
                due_day: t["due"].as_str().and_then(parse_date),
            })
        }).collect()).unwrap_or_default()
}

pub fn task_lists(a: &ureq::Agent) -> Result<Vec<(String, String)>, String> {
    call(a, "GET", "/tasks/v1/users/@me/lists", &[("maxResults", TASKS_PAGE.to_string())], None).map(|v| parse_lists(&v))
}

pub fn tasks(a: &ureq::Agent, list: &str) -> Result<Vec<GTask>, String> {
    let v = call(a, "GET", &format!("/tasks/v1/lists/{}/tasks", enc(list)), &[
        ("showCompleted", "false".into()), ("showHidden", "false".into()), ("maxResults", TASKS_PAGE.to_string()),
    ], None)?;
    Ok(parse_tasks(&v, list))
}

pub fn add_task(a: &ureq::Agent, list: &str, title: &str, due_day: Option<i64>, notes: &str) -> Result<String, String> {
    let mut body = json!({ "title": title });
    if let Some(d) = due_day {
        body["due"] = format!("{}T00:00:00.000Z", date(d)).into();
    }
    if !notes.is_empty() {
        body["notes"] = notes.into();
    }
    let v = call(a, "POST", &format!("/tasks/v1/lists/{}/tasks", enc(list)), &[], Some(&body))?;
    Ok(v["id"].as_str().unwrap_or("").to_owned())
}

pub fn update_task(a: &ureq::Agent, list: &str, id: &str, title: &str, due_day: Option<i64>, notes: &str) -> Result<(), String> {
    let body = json!({ "title": title, "due": due_day.map(|d| format!("{}T00:00:00.000Z", date(d))), "notes": notes });
    call(a, "PATCH", &format!("/tasks/v1/lists/{}/tasks/{}", enc(list), enc(id)), &[], Some(&body)).map(|_| ())
}

pub fn complete_task(a: &ureq::Agent, list: &str, id: &str) -> Result<(), String> {
    call(a, "PATCH", &format!("/tasks/v1/lists/{}/tasks/{}", enc(list), enc(id)), &[], Some(&json!({ "status": "completed" }))).map(|_| ())
}

pub fn delete_task(a: &ureq::Agent, list: &str, id: &str) -> Result<(), String> {
    call(a, "DELETE", &format!("/tasks/v1/lists/{}/tasks/{}", enc(list), enc(id)), &[], None).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_and_encoding() {
        assert_eq!(challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        assert_eq!(enc("me@gmail.com"), "me%40gmail.com");
        let url = auth_url("123-abc.apps.googleusercontent.com");
        assert!(url.contains("code_challenge_method=S256") && url.contains("access_type=offline"));
        assert!(url.contains("calendar.events") && url.contains("%2Fauth%2Ftasks"));
    }

    #[test]
    fn reads_the_pasted_address() {
        let (c, s) = code_from("http://127.0.0.1:53682/yatv?state=xyz&code=4%2F0AQlEd8x-abc_DEF").unwrap();
        assert_eq!(c, "4/0AQlEd8x-abc_DEF");
        assert_eq!(s.as_deref(), Some("xyz"));
        assert!(code_from("http://127.0.0.1:53682/yatv").is_none());
    }

    #[test]
    fn times_and_api_answers() {
        const NOW: i64 = 1_793_966_400;
        assert_eq!(parse_rfc3339("2026-11-06T15:00:00+03:00"), Some(NOW));
        assert_eq!(rfc3339(NOW, 3 * 3600), "2026-11-06T15:00:00+03:00");
        let ev = parse_events(&json!({ "items": [
            { "id": "a", "summary": "Планёрка", "start": { "dateTime": "2026-11-06T10:00:00+03:00" }, "end": { "dateTime": "2026-11-06T10:30:00+03:00" } },
            { "id": "b", "summary": "Отпуск", "start": { "date": "2026-11-07" }, "end": { "date": "2026-11-09" } },
        ]}), 3 * 3600);
        assert_eq!(ev.len(), 2);
        assert!(ev[1].4 && ev[1].3 - ev[1].2 == 2 * 86_400);
        let t = parse_tasks(&json!({ "items": [
            { "id": "t1", "title": "Сдать отчёт", "due": "2026-11-06T00:00:00.000Z", "notes": "до 18:00", "status": "needsAction" },
        ]}), "L");
        assert_eq!(t[0].due_day, Some(days_from_civil(2026, 11, 6)));
    }
}
