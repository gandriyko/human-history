//! Key normalization, configurable key bindings and keyboard-layout independence: shortcuts
//! typed on a Cyrillic (ЙЦУКЕН) layout act like the Latin key in the same physical position.

use std::collections::BTreeMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    /// Only for non-character keys and ctrl combos: a plain character carries its case instead.
    pub shift: bool,
}

impl Key {
    pub fn plain(code: KeyCode) -> Key {
        Key { code, ctrl: false, alt: false, shift: false }
    }

    pub fn char(&self) -> Option<char> {
        match self.code {
            KeyCode::Char(c) if !self.ctrl && !self.alt => Some(c),
            _ => None,
        }
    }

    pub fn is(&self, code: KeyCode) -> bool {
        *self == Key::plain(code)
    }

    pub fn ctrl(&self, c: char) -> bool {
        self.code == KeyCode::Char(c) && self.ctrl && !self.alt && !self.shift
    }
}

/// Ukrainian ЙЦУКЕН rows against QWERTY, plus the Russian-only letters (ы ъ э ё).
const CYRILLIC: &str = "йцукенгшщзхїфівапролджєячсмитьбюыъэё";
const LATIN: &str = "qwertyuiop[]asdfghjkl;'zxcvbnm,.s]'`";

fn latin(c: char) -> Option<char> {
    match c {
        // On ЙЦУКЕН the "/" key types "." and Shift+/ types ",".
        '.' => return Some('/'),
        ',' => return Some('?'),
        _ => {}
    }
    let lower = c.to_lowercase().next()?;
    let i = CYRILLIC.chars().position(|x| x == lower)?;
    let l = LATIN.chars().nth(i)?;
    if c.is_uppercase() {
        l.is_ascii_alphabetic().then(|| l.to_ascii_uppercase())
    } else {
        Some(l)
    }
}

/// Normalized key; `typing` (a text input has focus) leaves plain characters alone so Cyrillic
/// text can be typed, but modifier combos such as `ctrl+г` still act as `ctrl+u`.
pub fn normalize(event: KeyEvent, typing: bool) -> Key {
    let m = event.modifiers;
    let (ctrl, alt, mut shift) =
        (m.contains(KeyModifiers::CONTROL), m.contains(KeyModifiers::ALT), m.contains(KeyModifiers::SHIFT));
    let code = match event.code {
        KeyCode::BackTab => {
            shift = true;
            KeyCode::Tab
        }
        KeyCode::Char(c) if ctrl || alt => {
            shift |= c.is_uppercase();
            let c = latin(c).unwrap_or(c);
            KeyCode::Char(c.to_lowercase().next().unwrap_or(c))
        }
        KeyCode::Char(c) => {
            shift = false;
            KeyCode::Char(if typing { c } else { latin(c).unwrap_or(c) })
        }
        other => other,
    };
    Key { code, ctrl, alt, shift }
}

const NAMES: [(&str, char); 30] = [
    ("slash", '/'),
    ("question_mark", '?'),
    ("space", ' '),
    ("comma", ','),
    ("full_stop", '.'),
    ("minus", '-'),
    ("plus", '+'),
    ("equals_sign", '='),
    ("colon", ':'),
    ("semicolon", ';'),
    ("apostrophe", '\''),
    ("quotation_mark", '"'),
    ("exclamation_mark", '!'),
    ("at", '@'),
    ("number_sign", '#'),
    ("dollar_sign", '$'),
    ("percent_sign", '%'),
    ("ampersand", '&'),
    ("asterisk", '*'),
    ("left_parenthesis", '('),
    ("right_parenthesis", ')'),
    ("underscore", '_'),
    ("left_square_bracket", '['),
    ("right_square_bracket", ']'),
    ("left_curly_bracket", '{'),
    ("right_curly_bracket", '}'),
    ("vertical_line", '|'),
    ("backslash", '\\'),
    ("grave_accent", '`'),
    ("tilde", '~'),
];

/// A Textual key name (`ctrl+shift+a`, `slash`, `N`, `pageup`) -> `Key`.
pub fn parse(spec: &str) -> Option<Key> {
    let mut parts: Vec<&str> = spec.trim().split('+').collect();
    let base = parts.pop()?;
    let (mut ctrl, mut alt, mut shift) = (false, false, false);
    for m in parts {
        match m {
            "ctrl" => ctrl = true,
            "alt" | "meta" => alt = true,
            "shift" => shift = true,
            _ => return None,
        }
    }
    let code = match base {
        "tab" => KeyCode::Tab,
        "enter" => KeyCode::Enter,
        "escape" => KeyCode::Esc,
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        f if f.len() > 1 && f.starts_with('f') && f[1..].parse::<u8>().is_ok() => KeyCode::F(f[1..].parse().ok()?),
        name => {
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => KeyCode::Char(c),
                _ => KeyCode::Char(NAMES.iter().find(|(n, _)| *n == name)?.1),
            }
        }
    };
    if let KeyCode::Char(c) = code {
        if ctrl || alt {
            return Some(Key { code: KeyCode::Char(c.to_ascii_lowercase()), ctrl, alt, shift: shift || c.is_uppercase() });
        }
        let c = if shift { c.to_ascii_uppercase() } else { c };
        return Some(Key { code: KeyCode::Char(c), ctrl, alt, shift: false });
    }
    Some(Key { code, ctrl, alt, shift })
}

/// Short display form for the footer and help: `slash` -> `/`.
pub fn display(spec: &str) -> String {
    spec.split(',')
        .map(|k| match k.trim() {
            "question_mark" => "?".to_string(),
            "slash" => "/".to_string(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

pub struct Keymap {
    bindings: Vec<(Key, String)>,
    pub specs: BTreeMap<String, String>,
}

impl Keymap {
    pub fn new(specs: BTreeMap<String, String>) -> Keymap {
        let bindings = specs
            .iter()
            .flat_map(|(action, keys)| keys.split(',').filter_map(parse).map(move |k| (k, action.clone())))
            .collect();
        Keymap { bindings, specs }
    }

    pub fn action(&self, key: &Key) -> Option<&str> {
        self.bindings.iter().find(|(k, _)| k == key).map(|(_, a)| a.as_str())
    }

    pub fn label(&self, action: &str) -> String {
        self.specs.get(action).map(|s| display(s)).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn cyrillic_keys_map_to_latin_positions() {
        assert_eq!(normalize(ev('й'), false).code, KeyCode::Char('q'));
        assert_eq!(normalize(ev('.'), false).code, KeyCode::Char('/'));
        assert_eq!(normalize(ev('Т'), false).code, KeyCode::Char('N'));
        assert_eq!(normalize(ev('т'), false).code, KeyCode::Char('n'));
        let ctrl_g = KeyEvent::new(KeyCode::Char('г'), KeyModifiers::CONTROL);
        assert!(normalize(ctrl_g, true).ctrl('u'));
        assert_eq!(normalize(ev('й'), true).code, KeyCode::Char('й')); // typing keeps Cyrillic
    }

    #[test]
    fn binding_specs() {
        assert_eq!(parse("slash"), Some(Key::plain(KeyCode::Char('/'))));
        assert_eq!(parse("N"), Some(Key::plain(KeyCode::Char('N'))));
        let ctrl_shift_a = parse("ctrl+shift+a").unwrap();
        let typed = normalize(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::CONTROL | KeyModifiers::SHIFT), false);
        assert_eq!(ctrl_shift_a, typed);
        assert_ne!(parse("ctrl+a").unwrap(), typed);
        let shift_n = normalize(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT), false);
        assert_eq!(parse("N"), Some(shift_n));
    }
}
