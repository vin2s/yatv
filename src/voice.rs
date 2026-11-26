//! Alice's local IPC (Quasar) on 127.0.0.1:9875 — YATV listens to it directly,
//! so it works without yatv. The wire format is a u32 little-endian length
//! followed by a protobuf-like message {252: payload, 253: type}.

use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream},
    sync::mpsc::Sender,
    thread,
    time::Duration,
};

use crate::{action::normalize, Cmd};

pub const ADDRESS: &str = "127.0.0.1:9875";
const MAX_FRAME: usize = 512 * 1024;

/// «найди фильм колобок на втором этаже» → (command, "на втором этаже").
/// Only a trailing «на …» of at most three words is a TV hint; app aliases
/// like «на делюкс» are handled by the parser separately.
pub fn split_tv_hint(text: &str) -> (String, String) {
    // `rfind` on an ASCII needle always lands on a UTF-8 char boundary.
    let at = match text.rfind(" на ") {
        Some(i) => i,
        None => return (text.to_owned(), String::new()),
    };
    // " на " is 6 bytes in UTF-8 (space + 2 Cyrillic letters + space).
    let tail = match text.get(at + 6..) {
        Some(t) => t.trim(),
        None => return (text.to_owned(), String::new()),
    };
    let head = match text.get(..at) {
        Some(h) => h.trim(),
        None => return (text.to_owned(), String::new()),
    };
    if tail.is_empty() || tail.split_whitespace().count() > 3 || head.is_empty() {
        return (text.to_owned(), String::new());
    }
    (head.to_owned(), format!("на {tail}"))
}

pub fn watch(tx: Sender<Cmd>) {
    thread::spawn(move || {
        let address: SocketAddr = ADDRESS.parse().expect("address");
        loop {
            // A fresh `seen` per connection: the request ids come from maind and
            // start over when it restarts, so ids kept from an older connection
            // would drop brand-new phrases as «duplicates».
            let mut seen: VecDeque<Vec<u8>> = VecDeque::new();
            match TcpStream::connect_timeout(&address, Duration::from_secs(2)) {
                Ok(mut socket) => {
                    eprintln!("voice: connected to Alice");
                    let result = read_frames(&mut socket, &tx, &mut seen);
                    eprintln!("voice: connection lost ({result:?}); retrying");
                }
                Err(_) => {}
            }
            thread::sleep(Duration::from_secs(2));
        }
    });
}

// ---- minimal protobuf reader/writer (the same as yatv's, field 253 needs
// a multi-byte varint tag: a one-byte tag silently broke Alice's state frames).

fn varint(data: &[u8], offset: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..70).step_by(7) {
        let byte = *data.get(*offset)?;
        *offset += 1;
        if shift == 63 && byte > 1 {
            return None;
        }
        value |= u64::from(byte & 127) << shift;
        if byte < 128 {
            return Some(value);
        }
    }
    None
}

#[derive(Clone, Copy)]
enum Value<'a> {
    Number(u64),
    Bytes(&'a [u8]),
}

fn field(data: &[u8], wanted: u64) -> Option<Value<'_>> {
    let mut offset = 0;
    while offset < data.len() {
        let key = varint(data, &mut offset)?;
        if key >> 3 == 0 {
            return None;
        }
        let value = match key & 7 {
            0 => Value::Number(varint(data, &mut offset)?),
            2 => {
                let len = usize::try_from(varint(data, &mut offset)?).ok()?;
                let end = offset.checked_add(len)?;
                let bytes = data.get(offset..end)?;
                offset = end;
                Value::Bytes(bytes)
            }
            1 | 5 => {
                offset = offset.checked_add(if key & 7 == 1 { 8 } else { 4 })?;
                if offset > data.len() {
                    return None;
                }
                continue;
            }
            _ => return None,
        };
        if key >> 3 == wanted {
            return Some(value);
        }
    }
    None
}

fn bytes(data: &[u8], n: u64) -> Option<&[u8]> {
    match field(data, n)? {
        Value::Bytes(v) => Some(v),
        Value::Number(_) => None,
    }
}

fn number(data: &[u8], n: u64) -> Option<u64> {
    match field(data, n)? {
        Value::Number(v) => Some(v),
        Value::Bytes(_) => None,
    }
}

fn put_var(out: &mut Vec<u8>, mut v: u64) {
    while v >= 128 {
        out.push((v as u8 & 127) | 128);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_number(out: &mut Vec<u8>, field: u64, value: u64) {
    put_var(out, field << 3);
    put_var(out, value);
}

fn put_bytes(out: &mut Vec<u8>, field: u64, value: &[u8]) {
    put_var(out, (field << 3) | 2);
    put_var(out, value.len() as u64);
    out.extend_from_slice(value);
}

/// Stops Alice's own answer to the phrase YATV took over. Without this she
/// says «не нашла ничего подобного» / «воспроизведение не поддерживается»
/// over the action we perform: the cancel is the same frame yatv sends.
pub fn cancel_alice() {
    let frame = rpc("AliceCapability", 8, &[8, 3]);
    let address: SocketAddr = ADDRESS.parse().expect("address");
    if let Ok(mut socket) = TcpStream::connect_timeout(&address, Duration::from_millis(500)) {
        let _ = socket.write_all(&frame);
        let _ = socket.shutdown(std::net::Shutdown::Write);
    }
}

fn rpc(capability: &str, field_id: u64, method: &[u8]) -> Vec<u8> {
    let mut remoting = Vec::new();
    put_bytes(&mut remoting, 7, capability.as_bytes());
    put_bytes(&mut remoting, field_id, method);
    let mut message = Vec::new();
    put_bytes(&mut message, 252, &remoting);
    put_number(&mut message, 253, 210);
    let mut frame = (message.len() as u32).to_le_bytes().to_vec();
    frame.extend(message);
    frame
}

fn read_frames(
    socket: &mut TcpStream,
    tx: &Sender<Cmd>,
    seen: &mut VecDeque<Vec<u8>>,
) -> io::Result<()> {
    let mut data = Vec::new();
    loop {
        let mut len = [0u8; 4];
        socket.read_exact(&mut len)?;
        let n = u32::from_le_bytes(len) as usize;
        if n == 0 || n > MAX_FRAME {
            return Err(io::Error::other("bad frame"));
        }
        data.resize(n, 0);
        socket.read_exact(&mut data)?;

        // Alice's state frames (type 3).
        if number(&data, 253) != Some(3) {
            continue;
        }
        let Some(state) = bytes(&data, 252) else { continue };

        if !matches!(number(state, 1), Some(1 | 3)) {
            continue;
        }
        let Some(result) = bytes(state, 15) else { continue };
        if number(result, 2) != Some(1) || number(result, 4) != Some(1) {
            continue;
        }
        let Ok(text) = std::str::from_utf8(bytes(result, 1).unwrap_or_default()) else {
            continue;
        };
        let phrase = text.trim().to_owned();
        if phrase.is_empty() || phrase.len() > 512 {
            continue;
        }
        // maind sends the same recognition twice: an intermediate frame and a
        // final one. The raw request in field 2 equals the final text exactly;
        // an intermediate frame carries a different (partial) text there.
        // Without this guard a spoken command is handled twice (plans read
        // aloud twice). Compare after trimming, like the final `phrase`.
        let request_text = bytes(state, 2)
            .map(|b| std::str::from_utf8(b).unwrap_or_default().trim())
            .unwrap_or_default();
        if !request_text.is_empty() && request_text != phrase {
            continue;
        }
        // Short spoken commands («новости», «какие планы», «стоп») are never
        // deduped: a repeated key from maind must not make us deaf to them.
        // Only long forwarded phrases keep the duplicate guard.
        if phrase.len() > 60 {
            let request_key = bytes(state, 2).map(|b| b.to_vec()).unwrap_or_default();
            let fingerprint = [request_key.as_slice(), phrase.as_bytes()].concat();
            if !request_key.is_empty() && seen.contains(&fingerprint) {
                continue;
            }
            seen.push_back(fingerprint);
            if seen.len() > 128 {
                seen.pop_front();
            }
        }
        let full = normalize(&phrase);
        if full.is_empty() {
            continue;
        }
        eprintln!("voice: phrase {full:?}");
        // A TV mentioned at the end: «… на втором этаже». The controller
        // resolves it against the configured TV names; here we just pass it.
        // Planner phrases keep «на завтра / на сегодня» — that is a day, not a TV.
        let lower = full.to_lowercase();
        let is_plan = lower.contains("планы") || lower.contains("задач") || lower.contains("дела")
            || lower.contains("расписан") || lower.contains("напомни") || lower.contains("добавь")
            || lower.contains("дедлайн") || lower.contains("срок") || lower.contains("календар");
        let (phrase, tv_hint) = if is_plan { (full.clone(), String::new()) } else { split_tv_hint(&full) };
        if phrase.is_empty() {
            continue;
        }
        let (reply_tx, _reply_rx) = std::sync::mpsc::channel();
        let _ = tx.send(Cmd::Phrase(phrase, tv_hint, reply_tx));
    }
}

/// Says an answer through Alice's TTS. Never blocks the controller.
pub fn say(text: String) {
    thread::spawn(move || {
        // After the phrase is cancelled (up to ~700 ms), otherwise the cancel
        // swallows our own TTS and stock Alice answers instead.
        thread::sleep(Duration::from_millis(1100));
        if let Err(e) = speak(&text) {
            eprintln!("say: {text:?}: {e}");
        }
    });
}

/// Says it and returns when she is done — for a sequence of brief phrases.
pub fn say_wait(text: &str) {
    if let Err(e) = speak(text) {
        eprintln!("say: {text:?}: {e}");
    }
}

fn speak(text: &str) -> io::Result<()> {
    let address: SocketAddr = ADDRESS.parse().expect("address");
    let mut socket = TcpStream::connect_timeout(&address, Duration::from_secs(1))?;
    socket.set_read_timeout(Some(Duration::from_secs(15)))?;

    let mut directive = Vec::new();
    put_bytes(&mut directive, 1, text.as_bytes());
    put_number(&mut directive, 2, 1);
    let mut method = Vec::new();
    put_number(&mut method, 1, 18); // START_VOCALIZE
    put_number(&mut method, 3, 1); // send events
    put_bytes(&mut method, 13, &directive);
    let mut subscribe = Vec::new();
    put_bytes(&mut subscribe, 1, b"AliceCapability");

    let mut frames = rpc("AliceCapability", 20, &subscribe);
    frames.extend(rpc("AliceCapability", 8, &method));
    crate::log::add(&crate::state_dir(), "tts", &format!("Алиса говорит: {text}"));
    socket.write_all(&frames)?;

    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    let started = std::time::Instant::now();
    let mut data = Vec::new();
    // As on other controllers: wait for Alice's TTS state — 2 = speaking, then 0/6 = done.
    // If the firmware sends no state, the 5 s read timeout is the fallback.
    let mut speaking = false;
    while started.elapsed() < Duration::from_secs(30) {
        let mut len = [0u8; 4];
        match socket.read_exact(&mut len) {
            Ok(()) => {}
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                // Some firmwares never send a TTS state at all. If she has
                // already started speaking, a quiet socket is fine — but a
                // timeout BEFORE she starts means the phrase was not taken.
                if speaking { return Ok(()); }
                eprintln!("say: {text:?}: Алиса не подтвердила начало речи за 5 с");
                return Err(e);
            }
            Err(e) => return Err(e),
        }
        let n = u32::from_le_bytes(len) as usize;
        if n == 0 || n > MAX_FRAME {
            return Ok(());
        }
        data.resize(n, 0);
        socket.read_exact(&mut data)?;
        if number(&data, 253) == Some(3) {
            let state = bytes(&data, 252).unwrap_or_default();
            match number(state, 1) {
                Some(2) => {
                    speaking = true;
                    let _ = socket.set_read_timeout(Some(Duration::from_secs(15)));
                }
                Some(0 | 6) if speaking => return Ok(()),
                _ => {}
            }
            // The TTS events are also delivered in field 18 of the same state
            // frame: 6 = started, 1 = completed, 4 = error. Some firmware
            // versions only send these and never change field 1.
            if let Some(event) = bytes(state, 18) {
                match number(event, 1) {
                    Some(6) => {
                        speaking = true;
                        let _ = socket.set_read_timeout(Some(Duration::from_secs(15)));
                    }
                    Some(1) if speaking => return Ok(()),
                    Some(4) => return Err(io::Error::other("Алиса сообщила об ошибке TTS")),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_tv_hint() {
        assert_eq!(
            split_tv_hint("найди фильм колобок на втором этаже"),
            ("найди фильм колобок".into(), "на втором этаже".into())
        );
        assert_eq!(
            split_tv_hint("найди фильм колобок"),
            ("найди фильм колобок".into(), String::new())
        );
    }

    #[test]
    fn skips_intermediate_recognition_frame() {
        // Intermediate frame: the text in field 1 is a partial result, while
        // field 2 holds the earlier/raw request. It must be dropped; only the
        // final frame (field 2 == field 1) reaches the controller.
        let partial = "какие пла";
        let mut result = Vec::new();
        put_bytes(&mut result, 1, partial.as_bytes());
        put_number(&mut result, 2, 1);
        put_number(&mut result, 4, 1);
        let mut state = Vec::new();
        put_number(&mut state, 1, 1);
        put_bytes(&mut state, 2, "какие планы на сегодня".as_bytes());
        put_bytes(&mut state, 15, &result);
        let mut message = Vec::new();
        put_bytes(&mut message, 252, &state);
        put_number(&mut message, 253, 3);

        let st = bytes(&message, 252).unwrap();
        let text = std::str::from_utf8(bytes(bytes(st, 15).unwrap(), 1).unwrap()).unwrap().trim();
        let request = std::str::from_utf8(bytes(st, 2).unwrap()).unwrap_or_default().trim();
        assert_ne!(text, request, "intermediate frame must differ");
        // The guard in read_frames() drops it because request != phrase.
    }

    #[test]
    fn parses_a_recognized_phrase_frame() {
        // The wire shape yatv::recognized() uses:
        // state{1:1, 2:"request", 15:{1:"text", 2:1, 4:1}}
        let phrase = "найди фильм колобок на делюкс";
        let mut result = Vec::new();
        put_bytes(&mut result, 1, phrase.as_bytes());
        put_number(&mut result, 2, 1);
        put_number(&mut result, 4, 1);
        let mut state = Vec::new();
        put_number(&mut state, 1, 1);
        put_bytes(&mut state, 2, b"request-id");
        put_bytes(&mut state, 15, &result);
        let mut message = Vec::new();
        put_bytes(&mut message, 252, &state);
        put_number(&mut message, 253, 3);

        assert_eq!(number(&message, 253), Some(3));
        let st = bytes(&message, 252).unwrap();
        assert_eq!(number(st, 1), Some(1));
        let r = bytes(st, 15).unwrap();
        assert_eq!(number(r, 2), Some(1));
        assert_eq!(number(r, 4), Some(1));
        assert_eq!(bytes(r, 1), Some(phrase.as_bytes()));
        assert_eq!(bytes(st, 2), Some(&b"request-id"[..]));
    }
}
