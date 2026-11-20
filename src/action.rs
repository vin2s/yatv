//! The actions YATV can perform on the TV. One enum, shared by the voice
//! parser, the panel and the ADB runner. No dependency on soundya's Alice.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TvAction {
    /// A phrase about the TV that was not understood.
    Unclear,
    Wake,
    Sleep,
    /// Open an app by its spoken name: «открой ютуб на проекторе».
    Open(String),
    /// Search films across the TV's apps: «найди фильм дюна на телевизоре».
    Search(String),
    /// Search inside an app identified by its spoken name («найди … на рутубе»).
    SearchIn(String, String),
    /// Search inside the app the user named with their own alias and exact
    /// package («найди фильм колобок на делюкс»).
    SearchInPkg(String, String),
    /// Search YouTube: «включи на ютубе котиков».
    YouTube(String),
    /// Remote keys: the Android key code and how many presses.
    Key(&'static str, u8),
    /// Connection check from the panel.
    Ping,
    /// List the apps installed on the TV (for the panel's dropdowns).
    Apps,
    /// Open exactly this package (picked from Apps).
    OpenPackage(String),
    /// Open this package on a named TV (empty = default).
    OpenOn(String, String),
    /// Close an app: spoken name or package.
    Close(String),
    /// Press what is on the screen by its words: «нажми историю».
    Tap(String),
    /// Open an app, then a section of it.
    OpenIn(String, String),
    /// Input methods on a TV: list ids / enable / disable / set active.
    ImeList,
    ImeEnable(String),
    ImeDisable(String),
    ImeSet(String),
    /// Install ADBKeyBoard and disable the phone-keyboard bridge on this TV.
    KeyboardPrepare,
    /// Play the first video of the list on the screen.
    PlayFirst,
    /// Open an app and press its main button.
    OpenAndStart(String),
    /// «Режим пульта» on / off: short commands go to the TV.
    RemoteMode(bool),
}

/// Alice writes phrases in lower case, without punctuation, and may spell a
/// name differently. One shared normalizer for the parser and the app aliases.
pub fn normalize(text: &str) -> String {
    let text = text
        .trim()
        .trim_matches(['.', '!', '?'])
        .to_lowercase()
        .replace('ё', "е");
    text.strip_prefix("алиса, ")
        .or_else(|| text.strip_prefix("алиса "))
        .unwrap_or(&text)
        .trim()
        .to_owned()
}

/// The words of a phrase, without small talk: for comparing whole app names.
pub fn normalize_phrase(text: &str) -> String {
    normalize(text)
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|w| !w.is_empty() && *w != "пожалуйста")
        .collect::<Vec<_>>()
        .join(" ")
}
