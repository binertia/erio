use std::io::{self, Stdout, Write};

use crossterm::{
    cursor::{Hide, Show},
    event::{DisableMouseCapture, EnableMouseCapture},
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
    mouse_capture: bool,
    active: bool,
}

impl TerminalSession {
    pub fn enter(config: &TerminalConfig) -> io::Result<Self> {
        let mut session = Self {
            terminal: Terminal::new(CrosstermBackend::new(io::stdout()))?,
            alternate_screen: false,
            raw_mode: false,
            mouse_capture: false,
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

        execute!(io::stdout(), EnableMouseCapture)?;
        session.mouse_capture = true;

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

        if self.mouse_capture {
            if let Err(err) = execute!(self.terminal.backend_mut(), DisableMouseCapture) {
                first_error.get_or_insert(err);
            }
            self.mouse_capture = false;
        }

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

    /// Temporarily leave alternate screen and disable raw mode so an external
    /// interactive process can take over the terminal.
    pub fn suspend(&mut self) -> io::Result<()> {
        if self.mouse_capture {
            execute!(io::stdout(), DisableMouseCapture)?;
            self.mouse_capture = false;
        }
        if self.raw_mode {
            disable_raw_mode()?;
            self.raw_mode = false;
        }
        if self.alternate_screen {
            execute!(io::stdout(), Show, LeaveAlternateScreen)?;
            self.alternate_screen = false;
        }
        // Ensure any buffered backend output is flushed before handing
        // the terminal to the external process.
        self.terminal.backend_mut().flush()?;
        Ok(())
    }

    /// Re-enter alternate screen and re-enable raw mode after an external
    /// process has finished.
    pub fn resume(&mut self, config: &TerminalConfig) -> io::Result<()> {
        // Always explicitly re-apply our terminal state; the external
        // command may have left raw mode or other settings changed.
        if config.raw_mode {
            enable_raw_mode()?;
            self.raw_mode = true;
        }
        if config.alternate_screen {
            execute!(io::stdout(), EnterAlternateScreen, Hide)?;
            self.alternate_screen = true;
        }
        execute!(io::stdout(), EnableMouseCapture)?;
        self.mouse_capture = true;
        self.terminal.clear()?;
        self.terminal.backend_mut().flush()?;
        self.active = true;
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
