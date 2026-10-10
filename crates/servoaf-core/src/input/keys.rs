// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! Key names and characters to DOM key/code values (US layout).

use std::str::FromStr;

use servo::{Code, Key, Location, Modifiers, NamedKey};

#[derive(Clone, Debug, PartialEq)]
pub struct KeySpec {
    pub key: Key,
    pub code: Code,
    pub location: Location,
    /// Modifier needed to produce the key, e.g. Shift for 'A'.
    pub implied: Modifiers,
}

impl KeySpec {
    /// The modifier this key itself is (Shift, Control, ...), if any.
    pub fn modifier(&self) -> Modifiers {
        match &self.key {
            Key::Named(NamedKey::Shift) => Modifiers::SHIFT,
            Key::Named(NamedKey::Control) => Modifiers::CONTROL,
            Key::Named(NamedKey::Alt) => Modifiers::ALT,
            Key::Named(NamedKey::Meta) => Modifiers::META,
            _ => Modifiers::empty(),
        }
    }
}

const SHIFTED_DIGITS: &str = ")!@#$%^&*(";
const PUNCTUATION: &[(char, char, Code)] = &[
    ('-', '_', Code::Minus),
    ('=', '+', Code::Equal),
    ('[', '{', Code::BracketLeft),
    (']', '}', Code::BracketRight),
    ('\\', '|', Code::Backslash),
    (';', ':', Code::Semicolon),
    ('\'', '"', Code::Quote),
    (',', '<', Code::Comma),
    ('.', '>', Code::Period),
    ('/', '?', Code::Slash),
    ('`', '~', Code::Backquote),
];

fn spec(key: Key, code: Code, implied: Modifiers) -> KeySpec {
    KeySpec { key, code, location: Location::Standard, implied }
}

/// The key a person presses to type `character`.
pub fn for_character(character: char) -> KeySpec {
    match character {
        '\n' | '\r' => return for_name("Enter").expect("Enter"),
        '\t' => return for_name("Tab").expect("Tab"),
        _ => {},
    }
    let key = Key::Character(character.to_string());
    let none = Modifiers::empty();
    if character.is_ascii_alphabetic() {
        let code =
            Code::from_str(&format!("Key{}", character.to_ascii_uppercase())).unwrap_or(Code::Unidentified);
        let implied = if character.is_ascii_uppercase() { Modifiers::SHIFT } else { none };
        return spec(key, code, implied);
    }
    if let Some(digit) = character.to_digit(10) {
        return spec(key, Code::from_str(&format!("Digit{digit}")).unwrap_or(Code::Unidentified), none);
    }
    if let Some(index) = SHIFTED_DIGITS.find(character) {
        return spec(
            key,
            Code::from_str(&format!("Digit{index}")).unwrap_or(Code::Unidentified),
            Modifiers::SHIFT,
        );
    }
    if character == ' ' {
        return spec(key, Code::Space, none);
    }
    for (plain, shifted, code) in PUNCTUATION {
        if character == *plain {
            return spec(key, *code, none);
        }
        if character == *shifted {
            return spec(key, *code, Modifiers::SHIFT);
        }
    }
    spec(key, Code::Unidentified, none)
}

/// Parses a DOM `KeyboardEvent.key` value ("Enter", "ArrowDown", "a", "F5")
/// or a common alias ("Ctrl", "Esc", "Space", "Up").
pub fn for_name(name: &str) -> Option<KeySpec> {
    let mut chars = name.chars();
    if let (Some(character), None) = (chars.next(), chars.next()) {
        return Some(for_character(character));
    }
    let canonical = match name.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => "Control",
        "shift" => "Shift",
        "alt" | "option" => "Alt",
        "meta" | "cmd" | "command" | "super" | "win" | "os" => "Meta",
        "esc" | "escape" => "Escape",
        "return" | "enter" => "Enter",
        "space" | "spacebar" => return Some(for_character(' ')),
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "del" | "delete" => "Delete",
        "backspace" => "Backspace",
        "tab" => "Tab",
        "pgup" | "pageup" => "PageUp",
        "pgdn" | "pagedown" => "PageDown",
        "home" => "Home",
        "end" => "End",
        "insert" | "ins" => "Insert",
        "plus" => return Some(for_character('+')),
        _ => name,
    };
    let named = NamedKey::from_str(canonical).ok()?;
    let (code, location) = match named {
        NamedKey::Control => (Code::ControlLeft, Location::Left),
        NamedKey::Shift => (Code::ShiftLeft, Location::Left),
        NamedKey::Alt => (Code::AltLeft, Location::Left),
        NamedKey::Meta => (Code::MetaLeft, Location::Left),
        // Most named keys share their name with the physical key code.
        _ => (Code::from_str(canonical).unwrap_or(Code::Unidentified), Location::Standard),
    };
    Some(KeySpec { key: Key::Named(named), code, location, implied: Modifiers::empty() })
}

/// Splits "Control+Shift+t" into modifier keys and the final key. A lone "+"
/// is the plus key.
pub fn parse_chord(chord: &str) -> Result<(Vec<KeySpec>, KeySpec), String> {
    let chord = chord.trim();
    let parts: Vec<&str> = if chord == "+" {
        vec!["+"]
    } else if let Some(prefix) = chord.strip_suffix("++") {
        prefix.split('+').map(str::trim).filter(|p| !p.is_empty()).chain(["+"]).collect()
    } else {
        chord.split('+').map(str::trim).filter(|p| !p.is_empty()).collect()
    };
    let Some((last, modifiers)) = parts.split_last() else {
        return Err("Empty key".into());
    };
    let modifiers = modifiers
        .iter()
        .map(|name| {
            for_name(name)
                .filter(|spec| !spec.modifier().is_empty())
                .ok_or_else(|| format!("Unknown modifier: {name}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let key = for_name(last).ok_or_else(|| format!("Unknown key: {last}"))?;
    Ok((modifiers, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn characters() {
        assert_eq!(for_character('a').code, Code::KeyA);
        assert_eq!(for_character('Q').implied, Modifiers::SHIFT);
        assert_eq!(for_character('7').code, Code::Digit7);
        assert_eq!(for_character('@').code, Code::Digit2);
        assert_eq!(for_character('@').implied, Modifiers::SHIFT);
        assert_eq!(for_character('?').code, Code::Slash);
        assert_eq!(for_character(' ').code, Code::Space);
        assert_eq!(for_character('\n').key, Key::Named(NamedKey::Enter));
        assert_eq!(for_character('é').code, Code::Unidentified);
    }

    #[test]
    fn names_and_chords() {
        assert_eq!(for_name("Enter").unwrap().code, Code::Enter);
        assert_eq!(for_name("ArrowDown").unwrap().code, Code::ArrowDown);
        assert_eq!(for_name("esc").unwrap().key, Key::Named(NamedKey::Escape));
        assert_eq!(for_name("F5").unwrap().code, Code::F5);
        assert!(for_name("NotAKey").is_none());

        let (modifiers, key) = parse_chord("Control+Shift+t").unwrap();
        assert_eq!(modifiers.len(), 2);
        assert_eq!(modifiers[0].modifier(), Modifiers::CONTROL);
        assert_eq!(key.code, Code::KeyT);
        let (modifiers, key) = parse_chord("+").unwrap();
        assert!(modifiers.is_empty());
        assert_eq!(key.key, Key::Character("+".into()));
        let (modifiers, key) = parse_chord("Ctrl++").unwrap();
        assert_eq!(modifiers.len(), 1);
        assert_eq!(key.key, Key::Character("+".into()));
        assert!(parse_chord("a+b").is_err());
        assert!(parse_chord("").is_err());
        assert!(parse_chord("Control+Nope").is_err());
    }
}
