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
            let data = std::fs::read_to_string(file).unwrap_or_else(|_| "{\"apps\":[],\"names\":{}}".into());
            ("200 OK", "application/json", data)
        }
        ("POST", "/api/apps") => {
            let host = body.lines().next().unwrap_or("").trim().to_owned();
            let json = body.lines().skip(1).collect::<Vec<_>>().join("\n");
            let file = crate::state_dir().join(format!("apps-{}.json", host.replace(['.', ':'], "_")));
            let _ = std::fs::write(&file, &json);
            ("200 OK", "text/plain; charset=utf-8", "Сохранено".into())
        }
        ("POST", "/api/cmd") => {
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            let command = body.trim().to_owned();
            let (phrase, hint) = crate::voice::split_tv_hint(&command);
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
                
                "journal": crate::log::latest(&crate::state_dir()),
            });
            ("200 OK", "application/json", json.to_string())
        }
        ("GET", "/") | ("GET", "/index.html") => {
            ("200 OK", "text/html; charset=utf-8", include_str!("web.html").to_owned())
        }
        _ => ("404 Not Found", "text/plain", "not found".to_owned()),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.as_bytes().len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(payload.as_bytes());
}
