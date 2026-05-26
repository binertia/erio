use std::time::Duration;

use crossterm::event::{self, Event as CrosstermEvent, KeyCode, KeyEvent, KeyModifiers};

use crate::state::MainContext;
use super::panel::PanelId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    Key(KeyStroke),
    Resize { width: u16, height: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyStroke {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    None,
    Quit,
    FocusNext,
    FocusPrevious,
    FocusPanel(PanelId),
    MoveUp,
    MoveDown,
    SetMainContext(MainContext),
    Redraw,
}

impl From<KeyEvent> for KeyStroke {
    fn from(value: KeyEvent) -> Self {
        Self {
            code: value.code,
            modifiers: value.modifiers,
        }
    }
}

pub fn map_key(key: KeyStroke) -> InputAction {
    match (key.code, key.modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => InputAction::Quit,
        (KeyCode::Char('q'), _) => InputAction::Quit,
        (KeyCode::Right, _) | (KeyCode::Tab, _) | (KeyCode::Char(']'), _) => InputAction::FocusNext,
        (KeyCode::Left, _) | (KeyCode::BackTab, _) | (KeyCode::Char('['), _) => {
            InputAction::FocusPrevious
        }
        (KeyCode::Char('j'), _) | (KeyCode::Down, _) => InputAction::MoveDown,
        (KeyCode::Char('k'), _) | (KeyCode::Up, _) => InputAction::MoveUp,
        (KeyCode::Char('l'), _) => InputAction::SetMainContext(MainContext::Logs),
        (KeyCode::Char('s'), _) => InputAction::SetMainContext(MainContext::Stats),
        (KeyCode::Char('c'), _) => InputAction::SetMainContext(MainContext::Config),
        (KeyCode::Char('e'), _) => InputAction::SetMainContext(MainContext::Env),
        (KeyCode::Char('1'), _) => InputAction::FocusPanel(PanelId::Projects),
        (KeyCode::Char('2'), _) => InputAction::FocusPanel(PanelId::Services),
        (KeyCode::Char('3'), _) => InputAction::FocusPanel(PanelId::Containers),
        (KeyCode::Char('4'), _) => InputAction::FocusPanel(PanelId::Images),
        (KeyCode::Char('5'), _) => InputAction::FocusPanel(PanelId::Volumes),
        (KeyCode::Char('6'), _) => InputAction::FocusPanel(PanelId::Networks),
        (KeyCode::F(5), _) => InputAction::Redraw,
        _ => InputAction::None,
    }
}

pub fn read_terminal_input(timeout: Duration) -> std::io::Result<Option<InputEvent>> {
    if !event::poll(timeout)? {
        return Ok(None);
    }

    loop {
        match event::read()? {
            CrosstermEvent::Key(key) => return Ok(Some(InputEvent::Key(KeyStroke::from(key)))),
            CrosstermEvent::Resize(width, height) => {
                return Ok(Some(InputEvent::Resize { width, height }));
            }
            _ => {
                if !event::poll(Duration::from_millis(0))? {
                    return Ok(None);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_navigation_keys() {
        assert_eq!(
            map_key(KeyStroke {
                code: KeyCode::Char(']'),
                modifiers: KeyModifiers::NONE,
            }),
            InputAction::FocusNext
        );
        assert_eq!(
            map_key(KeyStroke {
                code: KeyCode::Char('['),
                modifiers: KeyModifiers::NONE,
            }),
            InputAction::FocusPrevious
        );
    }

    #[test]
    fn maps_number_keys_to_panels() {
        assert_eq!(
            map_key(KeyStroke {
                code: KeyCode::Char('3'),
                modifiers: KeyModifiers::NONE,
            }),
            InputAction::FocusPanel(PanelId::Containers)
        );
    }

    #[test]
    fn maps_quit_keys() {
        assert_eq!(
            map_key(KeyStroke {
                code: KeyCode::Char('q'),
                modifiers: KeyModifiers::NONE,
            }),
            InputAction::Quit
        );
        assert_eq!(
            map_key(KeyStroke {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
            }),
            InputAction::Quit
        );
    }
}
