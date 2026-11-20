//! YATV's own voice parser. Adapted from yatv's `alice::tv` and
//! `app_phrase_intent`, but with no dependency on music, PC or Alice's state:
//! every TV phrase is handled here, with or without yatv installed.

use crate::{action::{normalize, normalize_phrase, TvAction}, tv};

const UP: &str = "KEYCODE_DPAD_UP";
const DOWN: &str = "KEYCODE_DPAD_DOWN";
const LEFT: &str = "KEYCODE_DPAD_LEFT";
const RIGHT: &str = "KEYCODE_DPAD_RIGHT";
const CENTER: &str = "KEYCODE_DPAD_CENTER";
const OK_WORDS: [&str; 13] = [
    "ок", "окей", "ok", "нажми", "нажми ок", "выбери", "выбрать", "подтверди",
    "войди", "открой это", "жми", "выбери это", "нажми на это",
];

/// «на проекторе», «на телевизоре» — or the device as the object («включи
/// проектор»). Returns the phrase without it and whether it was the object.
fn strip_tv(text: &str) -> Option<(String, bool)> {
    const PLACES: [&str; 13] = [
        " на телевизоре ", " на телеке ", " на телике ", " на тв ", " на проекторе ", " на приставке ",
        " в телевизоре ", " в проекторе ", " на телевизор ", " на проектор ", " на тиви ", " на android тв ", " на смарт тв ",
    ];
    const NOUNS: [&str; 14] = [
        "телевизор", "телевизора", "телевизору", "телек", "телик", "тв", "тиви", "проектор", "проектора",
        "проектору", "приставку", "приставка", "приставке", "приставки",
    ];
    let mut rest = format!(" {text} ");
    for place in PLACES {
        if rest.contains(place) {
            rest = rest.replacen(place, " ", 1);
            return Some((rest.split_whitespace().collect::<Vec<_>>().join(" "), false));
        }
    }
    let words: Vec<&str> = rest.split_whitespace().collect();
    let kept: Vec<&str> = words.iter().copied().filter(|w| !NOUNS.contains(w)).collect();
    (kept.len() < words.len()).then(|| (kept.join(" "), true))
}

fn number_word(w: &str) -> Option<u8> {
    if let Ok(v) = w.parse::<u8>() {
        return Some(v);
    }
    const WORDS: [(&str, u8); 17] = [
        ("один", 1), ("одно", 1), ("два", 2), ("две", 2), ("три", 3), ("четыре", 4), ("пять", 5),
        ("шесть", 6), ("семь", 7), ("восемь", 8), ("девять", 9), ("десять", 10), ("пятнадцать", 15),
        ("двадцать", 20), ("тридцать", 30), ("пару", 2), ("парочку", 2),
    ];
    WORDS.iter().find(|(k, _)| *k == w).map(|(_, v)| *v)
}

/// «выше», «ещё ниже», «наведись левее», «вниз на три».
fn nav_key(words: &[&str]) -> Option<(&'static str, u8)> {
    const FILL: [&str; 25] = [
        "наведись", "наведи", "перейди", "сдвинься", "сдвинь", "подвинь", "подвинься", "иди", "двигайся",
        "курсор", "фокус", "еще", "на", "чуть", "немного", "раза", "раз", "повыше", "кнопку", "стрелка",
        "нажми", "жми", "стрелку", "клавишу", "листай",
    ];
    let mut code = None;
    let mut n = 1u8;
    for w in words {
        match *w {
            "выше" | "вверх" | "наверх" | "повыше" if code.is_none() => code = Some(UP),
            "ниже" | "вниз" | "пониже" => code = Some(DOWN),
            "левее" | "влево" | "налево" => code = Some(LEFT),
            "правее" | "вправо" | "направо" => code = Some(RIGHT),
            "два" | "2" | "дважды" => n = 2,
            "три" | "3" | "трижды" => n = 3,
            "четыре" | "4" => n = 4,
            "пять" | "5" => n = 5,
            w if FILL.contains(&w) => {}
            _ => return None,
        }
    }
    code.map(|c| (c, n))
}

/// «тише на 10», «сделай громче на пять» → (key, presses).
fn volume_steps(words: &[&str]) -> Option<(&'static str, u8)> {
    let mut code = None;
    let mut n: Option<u8> = None;
    for w in words {
        match *w {
            "тише" | "потише" | "убавь" | "уменьши" | "ниже" => code = Some("KEYCODE_VOLUME_DOWN"),
            "громче" | "погромче" | "прибавь" | "увеличь" | "добавь" => code = Some("KEYCODE_VOLUME_UP"),
            "сделай" | "на" | "звук" | "громкость" | "деления" | "делений" | "деление" | "пунктов"
            | "пункта" | "единиц" | "шагов" | "шага" | "еще" => {}
            w => n = Some(number_word(w)?),
        }
    }
    Some((code?, n.unwrap_or(3).clamp(1, 30)))
}

/// The owner's own app names: «открой делюкс» and «найди колобок на делюкс».
/// The exact package comes from the settings, never guessed from the words.

fn first_word(text: &str) -> &str {
    text.split_whitespace().next().unwrap_or("")
}

/// Unambiguous remote commands that are TV actions even without a TV word.
/// Generic «включи/выключи <entity>» is NOT here: Alice owns many other
/// entities (kettle, lights, garland) and those phrases must reach her.
fn is_tv_command(text: &str) -> bool {
    let first = first_word(text);
    const VERBS: &[&str] = &[
        "назад", "дальше", "пауза", "стоп", "продолжи", "воспроизведи",
        "перемотай", "домой", "выше", "ниже", "левее", "правее", "ок", "окей",
        "громче", "тише",
    ];
    VERBS.contains(&first) || nav_key(&text.split_whitespace().collect::<Vec<_>>()).is_some()
}

/// Parses a phrase. Strict whitelist — everything else is left to stock Alice:
///   1. «найди фильм X» (optionally «на <TV>») → TV search in the panel's app.
///   2. A phrase listed in the panel's «Свои фразы» (e.g. «free tv») → open app.
/// Remote keys, power, volume by voice are Alice's now; the panel still has
/// all those buttons for direct control.
pub fn parse(text: &str) -> Option<TvAction> {
    let n = normalize(text);

    // 2. Owned phrases from the panel. With a leading verb or bare; an
    // optional TV hint at the end can override the phrase's own TV.
    let said = crate::action::normalize_phrase(&n);
    let body = ["включи ", "включить ", "запусти ", "открой ", "поставь "]
        .iter()
        .find_map(|v| said.strip_prefix(v))
        .unwrap_or(&said);
    let (body, hint) = crate::voice::split_tv_hint(body);
    let hint = if hint.is_empty() { String::new() } else { hint };
    if let Some((pkg, phrase_tv)) = tv::phrase_for(&body) {
        return Some(TvAction::OpenOn(pkg, if hint.is_empty() { phrase_tv } else { hint }));
    }

    // 1. A film search: the noun is required, exactly like yatv.
    let after = ["найди ", "поищи ", "покажи "]
        .iter()
        .find_map(|v| n.strip_prefix(v))?;
    let nouns = ["фильм ", "сериал ", "мультфильм ", "мультик ", "видео ", "ролик ", "кино "];
    let mut q = after.to_owned();
    if let Some(rest) = nouns.iter().find_map(|noun| q.strip_prefix(noun)) {
        q = rest.to_owned();
    } else {
        // No film noun — Alice's own search, not ours.
        return None;
    }
    // An optional TV hint at the end: «… на 2 этаже».
    let (q, _hint) = crate::voice::split_tv_hint(&q);
    let q = crate::action::normalize_phrase(&q);
    if q.is_empty() {
        return None;
    }
    Some(TvAction::Search(q))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_film_search_is_ours() {
        // Our whitelist: a film search, optionally with a TV hint.
        assert_eq!(parse("найди фильм дюна на телевизоре"), Some(TvAction::Search("дюна".into())));
        assert_eq!(parse("найди фильм колобок"), Some(TvAction::Search("колобок".into())));
        assert_eq!(parse("найди фильм терминатор на 2 этаже"), Some(TvAction::Search("терминатор".into())));
        assert_eq!(parse("найди фильм"), None);

        // Everything else belongs to stock Alice — we stay silent.
        assert_eq!(parse("найди колобок"), None);
        assert_eq!(parse("доброе утро"), None);
        assert_eq!(parse("выключи телевизор"), None);
        assert_eq!(parse("открой ютуб на проекторе"), None);
        assert_eq!(parse("найди маму в 16 на рутубе"), None);
        assert_eq!(parse("наведись выше на проекторе"), None);
        assert_eq!(parse("сделай громче на телевизоре"), None);
        assert_eq!(parse("пауза на проекторе"), None);
        assert_eq!(parse("стоп"), None);
        assert_eq!(parse("выключи гирлянду"), None);
        assert_eq!(parse("включи чайник"), None);
        assert_eq!(parse("включи на ютубе котиков на проекторе"), None);
    }

    #[test]
    fn phrase_from_the_panel_opens_the_app() {
        tv::set_phrases(vec![("free tv".into(), "ru.cn.atx".into(), String::new())]);
        assert_eq!(parse("включи free tv"), Some(TvAction::OpenOn("ru.cn.atx".into(), String::new())));
        assert_eq!(parse("free tv на 2 этаже"), Some(TvAction::OpenOn("ru.cn.atx".into(), "на 2 этаже".into())));
        assert_eq!(parse("другая фраза"), None);
        tv::set_phrases(Vec::new());
    }
}
