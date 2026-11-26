//! A tiny HTTP server for the YATV panel: static HTML + two JSON endpoints.
//! No external crate: just TCP and enough RFC 7230 to serve a local page.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::mpsc::Sender,
    thread,
};

use crate::{settings::Settings, state_dir, Cmd};

pub fn serve(port: u16, tx: Sender<Cmd>) {
    let listener = match TcpListener::bind(("0.0.0.0", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("web: cannot bind port {port}: {e}");
            return;
        }
    };
    eprintln!("web: http://yatv.local:{port}/");
    crate::mark_ready();
    thread::spawn(move || {
        for stream in listener.incoming() {
            if let Ok(stream) = stream {
                let tx = tx.clone();
                thread::spawn(move || handle(stream, tx));
            }
        }
    });
}

fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| &haystack[i..i + needle.len()] == needle)
}

fn handle(mut stream: TcpStream, tx: Sender<Cmd>) {
    let settings = Settings::load(&state_dir());
    // Read the request headers, then exactly Content-Length bytes of the body.
    // `read_to_string` would wait for the client to close the connection — but
    // the client is waiting for our answer, so the panel used to time out.
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        if let Some(at) = find_sub(&buf, b"\r\n\r\n") {
            break at + 4;
        }
        if buf.len() > 64 * 1024 {
            return;
        }
    };
    let head_text = String::from_utf8_lossy(&buf[..header_end]).to_owned();
    let mut lines = head_text.lines();
    let request = lines.next().unwrap_or("");
    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/");
    let mut content_length = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);
    let body = String::from_utf8_lossy(&body).into_owned();

    let (status, kind, payload) = match (method, path.split('?').next().unwrap_or("/")) {
        ("GET", "/api/apps") => {
            // Cached app list for one TV, stored per host.
            let host = path.split_once('?').map_or("", |(_, q)| q);
            let file = crate::state_dir().join(format!("apps-{}.json", host.replace(['.', ':'], "_")));
            let data = crate::fsx::read_string(&file).unwrap_or_else(|| "{\"apps\":[],\"names\":{}}".into());
            ("200 OK", "application/json", data)
        }
        ("POST", "/api/apps") => {
            let host = body.lines().next().unwrap_or("").trim().to_owned();
            let json = body.lines().skip(1).collect::<Vec<_>>().join("\n");
            let file = crate::state_dir().join(format!("apps-{}.json", host.replace(['.', ':'], "_")));
            let _ = std::fs::write(&file, &json);
            ("200 OK", "text/plain; charset=utf-8", "Сохранено".into())
        }
        ("POST", "/api/plan") => {
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            let command = body.trim().to_owned();
            if tx.send(Cmd::Plan(command, reply_tx)).is_err() {
                ("500 Internal Server Error", "text/plain", "controller stopped".to_owned())
            } else {
                match reply_rx.recv_timeout(std::time::Duration::from_secs(30)) {
                    Ok(answer) => ("200 OK", "text/plain; charset=utf-8", answer),
                    Err(_) => ("504 Gateway Timeout", "text/plain", "planner did not answer".to_owned()),
                }
            }
        }
        ("GET", "/api/export") => {
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            let _ = tx.send(Cmd::Transfer("export".into(), String::new(), reply_tx));
            let answer = reply_rx.recv_timeout(std::time::Duration::from_secs(10))
                .unwrap_or_else(|_| "error: timeout".into());
            ("200 OK", "application/json; charset=utf-8", answer)
        }
        ("POST", "/api/import") => {
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            let _ = tx.send(Cmd::Transfer("import".into(), body.clone(), reply_tx));
            let answer = reply_rx.recv_timeout(std::time::Duration::from_secs(10))
                .unwrap_or_else(|_| "error: timeout".into());
            ("200 OK", "text/plain; charset=utf-8", answer)
        }
        ("POST", "/api/update") => {
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            let command = body.trim().to_owned();
            if tx.send(Cmd::Update(command, reply_tx)).is_err() {
                ("500 Internal Server Error", "text/plain", "controller stopped".to_owned())
            } else {
                match reply_rx.recv_timeout(std::time::Duration::from_secs(40)) {
                    Ok(answer) => ("200 OK", "text/plain; charset=utf-8", answer),
                    Err(_) => ("504 Gateway Timeout", "text/plain", "update did not answer".to_owned()),
                }
            }
        }
        ("POST", "/api/debug/say") => {
            // Test panel: say a phrase aloud. If YATV owns it, run the same
            // pipeline as a spoken command and speak the answer; otherwise
            // just repeat the phrase (demonstration of the handover).
            let text = body.trim();
            let json = crate::classify_phrase(text);
            let owner = json["owner"].as_str().unwrap_or("alice").to_owned();
            let mut spoken = text.to_owned();
            let mut answer = String::new();
            if owner == "yatv" {
                let (reply_tx, reply_rx) = std::sync::mpsc::channel();
                let (phrase, hint) = if text.to_lowercase().contains("план")
                    || text.to_lowercase().contains("задач") || text.to_lowercase().contains("напомни") {
                    (text.to_owned(), String::new())
                } else {
                    crate::voice::split_tv_hint(text)
                };
                if tx.send(crate::Cmd::Phrase(phrase, hint, reply_tx)).is_ok() {
                    if let Ok(a) = reply_rx.recv_timeout(std::time::Duration::from_secs(20)) {
                        spoken = a.clone();
                        answer = a;
                    }
                }
            }
            crate::log::add(&crate::state_dir(), "debug", &format!(
                "сказал фразу {text:?} → {}, {}; ответ {answer:?}", json["owner"], json["route"]
            ));
            // The controller already spoke the answer for YATV phrases. Only
            // repeat a phrase ourselves when it belongs to Alice (demo mode).
            if owner != "yatv" {
                crate::voice::say(spoken.clone());
            }
            let result = serde_json::json!({
                "phrase": text,
                "spoken": spoken,
                "answer": answer,
                "owner": json["owner"],
                "route": json["route"],
                "detail": json["detail"],
            });
            ("200 OK", "application/json", result.to_string())
        }
        ("POST", "/api/debug/phrase") => {
            // Dry-run of the voice pipeline: shows which engine owns a phrase
            // without cancelling Alice or starting a brief.
            let text = body.trim();
            let json = crate::classify_phrase(&text.to_lowercase().replace('ё', "е"));
            crate::log::add(&crate::state_dir(), "debug", &format!(
                "проверка фразы {text:?} → {}, {}", json["owner"], json["route"]
            ));
            ("200 OK", "application/json", json.to_string())
        }
        ("POST", "/api/cmd") => {
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            let command = body.trim().to_owned();
            let (phrase, hint) = if command.to_lowercase().contains("план")
                || command.to_lowercase().contains("задач") || command.to_lowercase().contains("напомни") {
                (body.clone(), String::new())
            } else {
                crate::voice::split_tv_hint(&command)
            };
            let cmd = Cmd::Phrase(phrase, hint, reply_tx);
            if tx.send(cmd).is_err() {
                ("500 Internal Server Error", "text/plain", "controller stopped".to_owned())
            } else {
                match reply_rx.recv_timeout(std::time::Duration::from_secs(60)) {
                    Ok(answer) => ("200 OK", "text/plain; charset=utf-8", answer),
                    Err(_) => ("504 Gateway Timeout", "text/plain", "TV did not answer".to_owned()),
                }
            }
        }
        ("POST", "/api/settings") => {
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            let patch = serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
            let _ = tx.send(Cmd::Settings(patch, reply_tx));
            let answer = reply_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap_or_else(|_| "error: timeout".into());
            ("200 OK", "text/plain; charset=utf-8", answer)
        }
        ("POST", "/api/journal/clear") => {
            crate::log::clear(&crate::state_dir());
            ("200 OK", "text/plain; charset=utf-8", "Журнал очищен".into())
        }
        ("GET", "/api/status") => {
            let json = serde_json::json!({
                "name": "yatv",
                "version": crate::VERSION,
                "tv_host": settings.tv_host,
                "tv_name": settings.tv_name,
                "tvs": settings.tv_list(),
                "phrases": settings.phrases,
                "g_client_id": settings.extra.get("g_client_id").and_then(|v| v.as_str()).unwrap_or(""),
                "g_key_saved": settings.extra.get("g_client_secret").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()),
                "tz_offset": settings.extra.get("tz_offset").and_then(|v| v.as_i64()),
                "planner": crate::planner::json(),
                "update": crate::updates().json(),
                "update_auto": settings.extra.get("update_auto").and_then(|v| v.as_bool()).unwrap_or(false),
                
                "journal": crate::log::latest(&crate::state_dir()),
            });
            ("200 OK", "application/json", json.to_string())
        }
        ("GET", "/") | ("GET", "/index.html") => {
            ("200 OK", "text/html; charset=utf-8", include_str!("web.html").to_owned())
        }
        _ => ("404 Not Found", "text/plain", "not found".to_owned()),
    };
    let content_type = if kind.contains("charset=") { kind.to_owned() } else { format!("{kind}; charset=utf-8") };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.as_bytes().len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(payload.as_bytes());
}
