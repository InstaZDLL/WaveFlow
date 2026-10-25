//! The combination a global shortcut is bound to, as stored and as each
//! backend wants it.
//!
//! Stored as `Ctrl+Alt+Shift+Super+<key>`: the modifiers in that fixed
//! order, then one key token from [`KEYS`]. The frontend builds the same
//! string from a key press, so the stored value, the Settings row and the
//! reset dialog all read one spelling.
//!
//! A letter token names the key that *types* that letter, not a physical
//! position: `RegisterHotKey` takes `VK_A`..`VK_Z`, which Windows maps
//! through the active layout, and the X11 grab looks the keysym up the same
//! way. So on AZERTY `Ctrl+Alt+A` is the key labelled A, and the frontend
//! takes a letter from `KeyboardEvent.key` to match. macOS hot keys are
//! physical (`kVK_ANSI_A`), the one place this differs.

use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut};

/// One key a global shortcut can end on: the stored token, the code the
/// native backends register, and the xkb keysym name the Wayland portal
/// reads in a trigger.
struct KeyDef {
    token: &'static str,
    code: Code,
    keysym: &'static str,
}

macro_rules! keys {
    ($( $token:literal => $code:ident, $keysym:literal; )*) => {
        &[ $( KeyDef { token: $token, code: Code::$code, keysym: $keysym }, )* ]
    };
}

/// Every key a global shortcut may use. Punctuation is left out on
/// purpose: what `;` or `,` sits on changes with the layout in ways the
/// letter rule above does not cover, and a combination that silently
/// moves is worse than one that is refused.
const KEYS: &[KeyDef] = keys! {
    "A" => KeyA, "a"; "B" => KeyB, "b"; "C" => KeyC, "c"; "D" => KeyD, "d";
    "E" => KeyE, "e"; "F" => KeyF, "f"; "G" => KeyG, "g"; "H" => KeyH, "h";
    "I" => KeyI, "i"; "J" => KeyJ, "j"; "K" => KeyK, "k"; "L" => KeyL, "l";
    "M" => KeyM, "m"; "N" => KeyN, "n"; "O" => KeyO, "o"; "P" => KeyP, "p";
    "Q" => KeyQ, "q"; "R" => KeyR, "r"; "S" => KeyS, "s"; "T" => KeyT, "t";
    "U" => KeyU, "u"; "V" => KeyV, "v"; "W" => KeyW, "w"; "X" => KeyX, "x";
    "Y" => KeyY, "y"; "Z" => KeyZ, "z";
    "0" => Digit0, "0"; "1" => Digit1, "1"; "2" => Digit2, "2"; "3" => Digit3, "3";
    "4" => Digit4, "4"; "5" => Digit5, "5"; "6" => Digit6, "6"; "7" => Digit7, "7";
    "8" => Digit8, "8"; "9" => Digit9, "9";
    "F1" => F1, "F1"; "F2" => F2, "F2"; "F3" => F3, "F3"; "F4" => F4, "F4";
    "F5" => F5, "F5"; "F6" => F6, "F6"; "F7" => F7, "F7"; "F8" => F8, "F8";
    "F9" => F9, "F9"; "F10" => F10, "F10"; "F11" => F11, "F11"; "F12" => F12, "F12";
    "F13" => F13, "F13"; "F14" => F14, "F14"; "F15" => F15, "F15"; "F16" => F16, "F16";
    "F17" => F17, "F17"; "F18" => F18, "F18"; "F19" => F19, "F19"; "F20" => F20, "F20";
    "F21" => F21, "F21"; "F22" => F22, "F22"; "F23" => F23, "F23"; "F24" => F24, "F24";
    "Space" => Space, "space";
    "ArrowUp" => ArrowUp, "Up"; "ArrowDown" => ArrowDown, "Down";
    "ArrowLeft" => ArrowLeft, "Left"; "ArrowRight" => ArrowRight, "Right";
    "Home" => Home, "Home"; "End" => End, "End";
    "PageUp" => PageUp, "Page_Up"; "PageDown" => PageDown, "Page_Down";
    "Insert" => Insert, "Insert"; "Delete" => Delete, "Delete";
    "Numpad0" => Numpad0, "KP_0"; "Numpad1" => Numpad1, "KP_1";
    "Numpad2" => Numpad2, "KP_2"; "Numpad3" => Numpad3, "KP_3";
    "Numpad4" => Numpad4, "KP_4"; "Numpad5" => Numpad5, "KP_5";
    "Numpad6" => Numpad6, "KP_6"; "Numpad7" => Numpad7, "KP_7";
    "Numpad8" => Numpad8, "KP_8"; "Numpad9" => Numpad9, "KP_9";
    "NumpadAdd" => NumpadAdd, "KP_Add"; "NumpadSubtract" => NumpadSubtract, "KP_Subtract";
    "NumpadMultiply" => NumpadMultiply, "KP_Multiply"; "NumpadDivide" => NumpadDivide, "KP_Divide";
};

/// Why a combination was refused before any backend saw it. Serialized
/// for the Settings row, which words each case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ComboError {
    /// Not a combination: nothing, or modifiers with no key.
    Empty,
    /// A key outside [`KEYS`].
    UnsupportedKey,
    /// No Ctrl, Alt or Super, and not F13–F24. A bare `Space` or `Shift+S`
    /// registered system-wide would take that key from every application.
    NeedsModifier,
}

/// A parsed combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Combo {
    ctrl: bool,
    alt: bool,
    shift: bool,
    super_key: bool,
    key: &'static KeyDef,
}

impl PartialEq for KeyDef {
    fn eq(&self, other: &Self) -> bool {
        self.token == other.token
    }
}
impl Eq for KeyDef {}
impl std::fmt::Debug for KeyDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.token)
    }
}

impl Combo {
    /// Parse a stored combination. Modifiers are accepted in any order
    /// and the result is re-spelled canonically by [`Combo::canonical`].
    pub fn parse(raw: &str) -> Result<Self, ComboError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(ComboError::Empty);
        }
        let parts: Vec<&str> = raw.split('+').map(str::trim).collect();
        let (key_token, modifiers) = parts.split_last().ok_or(ComboError::Empty)?;
        let mut combo = Combo {
            ctrl: false,
            alt: false,
            shift: false,
            super_key: false,
            key: &KEYS[0],
        };
        for modifier in modifiers {
            match modifier.to_ascii_lowercase().as_str() {
                "ctrl" => combo.ctrl = true,
                "alt" => combo.alt = true,
                "shift" => combo.shift = true,
                "super" => combo.super_key = true,
                _ => return Err(ComboError::UnsupportedKey),
            }
        }
        if key_token.is_empty()
            || matches!(
                key_token.to_ascii_lowercase().as_str(),
                "ctrl" | "alt" | "shift" | "super"
            )
        {
            return Err(ComboError::Empty);
        }
        combo.key = KEYS
            .iter()
            .find(|k| k.token.eq_ignore_ascii_case(key_token))
            .ok_or(ComboError::UnsupportedKey)?;
        if !(combo.ctrl || combo.alt || combo.super_key) && !combo.is_extra_function_key() {
            return Err(ComboError::NeedsModifier);
        }
        Ok(combo)
    }

    /// F13–F24: keys no application types with, found on macro pads and
    /// some keyboards precisely for this. F1–F12 alone stay refused —
    /// F5 and F11 belong to whatever window has the focus.
    fn is_extra_function_key(&self) -> bool {
        self.key
            .token
            .strip_prefix('F')
            .and_then(|n| n.parse::<u8>().ok())
            .is_some_and(|n| (13..=24).contains(&n))
    }

    /// The stored spelling.
    pub fn canonical(&self) -> String {
        let mut out = String::new();
        for (on, name) in [
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
            (self.super_key, "Super"),
        ] {
            if on {
                out.push_str(name);
                out.push('+');
            }
        }
        out.push_str(self.key.token);
        out
    }

    /// What the native backends (`RegisterHotKey`, Carbon, X11) register.
    pub fn shortcut(&self) -> Shortcut {
        let mut mods = Modifiers::empty();
        mods.set(Modifiers::CONTROL, self.ctrl);
        mods.set(Modifiers::ALT, self.alt);
        mods.set(Modifiers::SHIFT, self.shift);
        mods.set(Modifiers::SUPER, self.super_key);
        Shortcut::new((!mods.is_empty()).then_some(mods), self.key.code)
    }

    /// The `preferred_trigger` the XDG GlobalShortcuts portal reads: the
    /// shortcuts specification's `CTRL+ALT+SHIFT+LOGO+<keysym>`.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn portal_trigger(&self) -> String {
        let mut out = String::new();
        for (on, name) in [
            (self.ctrl, "CTRL"),
            (self.alt, "ALT"),
            (self.shift, "SHIFT"),
            (self.super_key, "LOGO"),
        ] {
            if on {
                out.push_str(name);
                out.push('+');
            }
        }
        out.push_str(self.key.keysym);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_respells_canonically() {
        let combo = Combo::parse("Shift+Ctrl+p").unwrap();
        assert_eq!(combo.canonical(), "Ctrl+Shift+P");
        assert_eq!(
            Combo::parse("Super+Alt+ArrowUp").unwrap().canonical(),
            "Alt+Super+ArrowUp"
        );
    }

    #[test]
    fn refuses_what_would_steal_a_key_from_every_application() {
        assert_eq!(Combo::parse("Space"), Err(ComboError::NeedsModifier));
        assert_eq!(Combo::parse("Shift+S"), Err(ComboError::NeedsModifier));
        assert_eq!(Combo::parse("F5"), Err(ComboError::NeedsModifier));
        assert!(Combo::parse("F13").is_ok());
        assert!(Combo::parse("Shift+F24").is_ok());
        assert!(Combo::parse("Super+M").is_ok());
    }

    #[test]
    fn refuses_unknown_keys_and_empty_combinations() {
        assert_eq!(Combo::parse(""), Err(ComboError::Empty));
        assert_eq!(Combo::parse("Ctrl+"), Err(ComboError::Empty));
        assert_eq!(Combo::parse("Ctrl+Alt"), Err(ComboError::Empty));
        assert_eq!(Combo::parse("Ctrl+;"), Err(ComboError::UnsupportedKey));
        assert_eq!(Combo::parse("Hyper+P"), Err(ComboError::UnsupportedKey));
        assert_eq!(
            Combo::parse("Ctrl+MediaPlayPause"),
            Err(ComboError::UnsupportedKey)
        );
    }

    #[test]
    fn native_shortcut_carries_the_modifiers_and_the_code() {
        let shortcut = Combo::parse("Ctrl+Alt+P").unwrap().shortcut();
        assert_eq!(shortcut.mods, Modifiers::CONTROL | Modifiers::ALT);
        assert_eq!(shortcut.key, Code::KeyP);
        let bare = Combo::parse("F13").unwrap().shortcut();
        assert!(bare.mods.is_empty());
        assert_eq!(bare.key, Code::F13);
    }

    #[test]
    fn portal_trigger_follows_the_shortcuts_specification() {
        assert_eq!(
            Combo::parse("Ctrl+Shift+Super+P").unwrap().portal_trigger(),
            "CTRL+SHIFT+LOGO+p"
        );
        assert_eq!(
            Combo::parse("Alt+PageDown").unwrap().portal_trigger(),
            "ALT+Page_Down"
        );
        assert_eq!(
            Combo::parse("Ctrl+NumpadAdd").unwrap().portal_trigger(),
            "CTRL+KP_Add"
        );
    }

    #[test]
    fn every_token_is_unique() {
        let mut tokens: Vec<_> = KEYS.iter().map(|k| k.token.to_ascii_lowercase()).collect();
        tokens.sort();
        let len = tokens.len();
        tokens.dedup();
        assert_eq!(tokens.len(), len);
    }
}
