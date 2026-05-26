use std::io::{self, Stdout};

use crossterm::{
    cursor::{Hide, Show},
    execute,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
        is_raw_mode_enabled,
    },
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::config::TerminalConfig;

pub type AppTerminal = Terminal<CrosstermBackend<Stdout>>;

#[derive(Debug)]
pub struct TerminalSession {
    terminal: AppTerminal,
    alternate_screen: bool,
    raw_mode: bool,
    active: bool,
}

impl TerminalSession {
    pub fn enter(config: &TerminalConfig) -> io::Result<Self> {
        let mut session = Self {
            terminal: Terminal::new(CrosstermBackend::new(io::stdout()))?,
            alternate_screen: false,
            raw_mode: false,
            active: false,
        };

        if config.raw_mode && !is_raw_mode_enabled()? {
            enable_raw_mode()?;
            session.raw_mode = true;
        }

        if config.alternate_screen {
            execute!(io::stdout(), EnterAlternateScreen, Hide)?;
            session.alternate_screen = true;
        }

        session.active = true;
        Ok(session)
    }

    pub fn terminal_mut(&mut self) -> &mut AppTerminal {
        &mut self.terminal
    }

    pub fn shutdown(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }

        let mut first_error = None;

        if self.alternate_screen {
            if let Err(err) = execute!(self.terminal.backend_mut(), Show, LeaveAlternateScreen) {
                first_error.get_or_insert(err);
            }
            self.alternate_screen = false;
        }

        if self.raw_mode {
            if let Err(err) = disable_raw_mode() {
                first_error.get_or_insert(err);
            }
            self.raw_mode = false;
        }

        self.active = false;

        match first_error {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
