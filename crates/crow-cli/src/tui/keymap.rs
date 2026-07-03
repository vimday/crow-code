use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── KeyAction ───────────────────────────────────────────────────────────────

/// A semantic TUI action that can be bound to one or more key combinations.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyAction {
    Submit,
    Cancel,
    Quit,
    NewLine,
    ClearInput,
    ScrollUp,
    ScrollDown,
    ScrollTop,
    ScrollBottom,
    HistoryPrev,
    HistoryNext,
    FocusNext,
    ToggleViewMode,
    AcceptSuggestion,
    DismissPopup,
    CopyLastMessage,
}

impl KeyAction {
    /// Human-readable label for display in the shortcut overlay.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Submit => "Submit",
            Self::Cancel => "Cancel / Interrupt",
            Self::Quit => "Quit",
            Self::NewLine => "New Line",
            Self::ClearInput => "Clear Input",
            Self::ScrollUp => "Scroll Up",
            Self::ScrollDown => "Scroll Down",
            Self::ScrollTop => "Scroll to Top",
            Self::ScrollBottom => "Scroll to Bottom",
            Self::HistoryPrev => "Previous History",
            Self::HistoryNext => "Next History",
            Self::FocusNext => "Focus Next Pane",
            Self::ToggleViewMode => "Toggle View Mode",
            Self::AcceptSuggestion => "Accept Suggestion",
            Self::DismissPopup => "Dismiss",
            Self::CopyLastMessage => "Copy Last Message",
        }
    }
}

// ─── KeyBinding ──────────────────────────────────────────────────────────────

/// A serialisable key binding specification.
///
/// `code` is a lowercase string representation of the key:
/// `"enter"`, `"esc"`, `"tab"`, `"up"`, `"down"`, `"left"`, `"right"`,
/// `"home"`, `"end"`, `"pageup"`, `"pagedown"`, `"backspace"`, `"delete"`,
/// or a single character like `"c"`, `"u"`, `"y"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyBinding {
    pub code: String,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub alt: bool,
}

impl KeyBinding {
    /// Create a plain key binding (no modifiers).
    fn plain(code: &str) -> Self {
        Self {
            code: code.to_string(),
            ctrl: false,
            shift: false,
            alt: false,
        }
    }

    /// Create a Ctrl+<key> binding.
    fn ctrl(code: &str) -> Self {
        Self {
            code: code.to_string(),
            ctrl: true,
            shift: false,
            alt: false,
        }
    }

    /// Create a Shift+<key> binding.
    fn shift(code: &str) -> Self {
        Self {
            code: code.to_string(),
            ctrl: false,
            shift: true,
            alt: false,
        }
    }

    /// Check whether this binding matches a `crossterm` [`KeyEvent`].
    pub fn matches(&self, event: &KeyEvent) -> bool {
        let code_matches = match self.code.as_str() {
            "enter" => event.code == KeyCode::Enter,
            "esc" => event.code == KeyCode::Esc,
            "tab" => event.code == KeyCode::Tab,
            "backtab" => event.code == KeyCode::BackTab,
            "backspace" => event.code == KeyCode::Backspace,
            "delete" => event.code == KeyCode::Delete,
            "up" => event.code == KeyCode::Up,
            "down" => event.code == KeyCode::Down,
            "left" => event.code == KeyCode::Left,
            "right" => event.code == KeyCode::Right,
            "home" => event.code == KeyCode::Home,
            "end" => event.code == KeyCode::End,
            "pageup" => event.code == KeyCode::PageUp,
            "pagedown" => event.code == KeyCode::PageDown,
            single if single.len() == 1 => {
                // Safety: we just checked len() == 1
                let ch = single.chars().next().unwrap_or('\0');
                event.code == KeyCode::Char(ch)
            }
            _ => false,
        };
        if !code_matches {
            return false;
        }

        let mods = event.modifiers;
        self.ctrl == mods.contains(KeyModifiers::CONTROL)
            && self.shift == mods.contains(KeyModifiers::SHIFT)
            && self.alt == mods.contains(KeyModifiers::ALT)
    }

    /// Return a human-readable string such as `"Ctrl+C"` or `"Shift+Enter"`.
    pub fn display(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push("Alt");
        }
        if self.shift {
            parts.push("Shift");
        }
        let key_label = match self.code.as_str() {
            "enter" => "Enter",
            "esc" => "Esc",
            "tab" => "Tab",
            "backtab" => "BackTab",
            "backspace" => "Backspace",
            "delete" => "Delete",
            "up" => "↑",
            "down" => "↓",
            "left" => "←",
            "right" => "→",
            "home" => "Home",
            "end" => "End",
            "pageup" => "PgUp",
            "pagedown" => "PgDn",
            other => other,
        };
        parts.push(key_label);
        parts.join("+")
    }
}

// ─── KeymapConfig ────────────────────────────────────────────────────────────

/// Configurable keymap loaded from the `tui.keymap` section of `.crow/config.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeymapConfig {
    #[serde(default = "default_bindings")]
    pub bindings: HashMap<KeyAction, Vec<KeyBinding>>,
}

impl Default for KeymapConfig {
    fn default() -> Self {
        Self {
            bindings: default_bindings(),
        }
    }
}

/// Construct the default key bindings that match Crow's existing hardcoded behaviour.
fn default_bindings() -> HashMap<KeyAction, Vec<KeyBinding>> {
    let mut m = HashMap::new();

    m.insert(KeyAction::Submit, vec![KeyBinding::plain("enter")]);
    m.insert(KeyAction::Cancel, vec![KeyBinding::ctrl("c")]);
    m.insert(KeyAction::Quit, vec![KeyBinding::ctrl("d")]);
    m.insert(KeyAction::NewLine, vec![KeyBinding::shift("enter")]);
    m.insert(KeyAction::ClearInput, vec![KeyBinding::ctrl("u")]);

    m.insert(
        KeyAction::ScrollUp,
        vec![KeyBinding::plain("up"), KeyBinding::plain("pageup")],
    );
    m.insert(
        KeyAction::ScrollDown,
        vec![KeyBinding::plain("down"), KeyBinding::plain("pagedown")],
    );
    m.insert(KeyAction::ScrollTop, vec![KeyBinding::plain("home")]);
    m.insert(KeyAction::ScrollBottom, vec![KeyBinding::plain("end")]);

    m.insert(
        KeyAction::HistoryPrev,
        vec![KeyBinding::ctrl("p")],
    );
    m.insert(
        KeyAction::HistoryNext,
        vec![KeyBinding::ctrl("n")],
    );

    m.insert(KeyAction::FocusNext, vec![KeyBinding::plain("tab")]);
    m.insert(KeyAction::ToggleViewMode, vec![KeyBinding::ctrl("v")]);
    m.insert(KeyAction::DismissPopup, vec![KeyBinding::plain("esc")]);
    m.insert(KeyAction::CopyLastMessage, vec![KeyBinding::ctrl("y")]);
    m.insert(KeyAction::AcceptSuggestion, vec![KeyBinding::plain("tab")]);

    m
}

impl KeymapConfig {
    /// Resolve a [`KeyEvent`] to a [`KeyAction`] using the configured bindings.
    ///
    /// Returns the first matching action found. If no bindings match, returns `None`.
    pub fn resolve(&self, event: &KeyEvent) -> Option<KeyAction> {
        for (action, bindings) in &self.bindings {
            if bindings.iter().any(|b| b.matches(event)) {
                return Some(action.clone());
            }
        }
        None
    }

    /// Collect human-readable `(key_combo, action_label)` pairs for all bindings.
    ///
    /// Useful for rendering the shortcut overlay in the TUI.
    pub fn describe_bindings(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .bindings
            .iter()
            .flat_map(|(action, bindings)| {
                bindings
                    .iter()
                    .map(move |b| (b.display(), action.label().to_string()))
            })
            .collect();
        out.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        out
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn make_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn plain_enter_matches_submit() {
        let config = KeymapConfig::default();
        let event = make_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(config.resolve(&event), Some(KeyAction::Submit));
    }

    #[test]
    fn ctrl_c_matches_cancel() {
        let config = KeymapConfig::default();
        let event = make_key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(config.resolve(&event), Some(KeyAction::Cancel));
    }

    #[test]
    fn shift_enter_matches_new_line() {
        let config = KeymapConfig::default();
        let event = make_key(KeyCode::Enter, KeyModifiers::SHIFT);
        assert_eq!(config.resolve(&event), Some(KeyAction::NewLine));
    }

    #[test]
    fn unbound_key_returns_none() {
        let config = KeymapConfig::default();
        let event = make_key(KeyCode::Char('z'), KeyModifiers::NONE);
        assert_eq!(config.resolve(&event), None);
    }

    #[test]
    fn describe_bindings_is_non_empty() {
        let config = KeymapConfig::default();
        let descs = config.describe_bindings();
        assert!(!descs.is_empty());
        // Every entry has a non-empty key combo and label
        for (combo, label) in &descs {
            assert!(!combo.is_empty());
            assert!(!label.is_empty());
        }
    }

    #[test]
    fn serde_roundtrip() {
        let config = KeymapConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let restored: KeymapConfig = serde_json::from_str(&json).unwrap();
        // Every default action should still resolve identically
        let event = make_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(config.resolve(&event), restored.resolve(&event));
    }

    #[test]
    fn display_format() {
        assert_eq!(KeyBinding::ctrl("c").display(), "Ctrl+c");
        assert_eq!(KeyBinding::shift("enter").display(), "Shift+Enter");
        assert_eq!(KeyBinding::plain("pageup").display(), "PgUp");
    }
}
