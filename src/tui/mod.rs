use std::{io, path::PathBuf, time::Duration};

use anyhow::Result;
use crossterm::{
    event::{self, Event as CrosstermEvent, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

use crate::{ipc::IpcClient, model::Event};

#[derive(Debug, Default)]
pub struct TuiState {
    pub events: Vec<Event>,
    pub selected: usize,
}

impl TuiState {
    pub fn set_events(&mut self, events: Vec<Event>) {
        self.events = events;
        if self.events.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.events.len() {
            self.selected = self.events.len() - 1;
        }
    }

    pub fn next(&mut self) {
        if !self.events.is_empty() {
            self.selected = (self.selected + 1).min(self.events.len() - 1);
        }
    }

    pub fn previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn selected_event_id(&self) -> Option<i64> {
        self.events.get(self.selected).map(|event| event.id)
    }
}

pub fn run(socket_path: PathBuf) -> Result<()> {
    let client = IpcClient::new(socket_path);
    let mut state = TuiState::default();
    state.set_events(client.list_events(true, 100)?);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run_loop(&mut terminal, &client, &mut state);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    client: &IpcClient,
    state: &mut TuiState,
) -> Result<()> {
    loop {
        terminal.draw(|frame| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Min(1)])
                .split(frame.area());
            let header =
                Paragraph::new("signalpane unread mentions - q: quit  r: refresh  m: mark read")
                    .block(Block::default().borders(Borders::ALL).title("signalpane"));
            frame.render_widget(header, chunks[0]);

            let items = state.events.iter().map(|event| {
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("[{}] ", event.source),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(event.title.clone()),
                ]))
            });
            let mut list_state = ListState::default();
            if !state.events.is_empty() {
                list_state.select(Some(state.selected));
            }
            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title("Unread"))
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
            frame.render_stateful_widget(list, chunks[1], &mut list_state);
        })?;

        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                CrosstermEvent::Key(key) if key.code == KeyCode::Char('q') => break,
                CrosstermEvent::Key(key)
                    if key.code == KeyCode::Char('j') || key.code == KeyCode::Down =>
                {
                    state.next();
                }
                CrosstermEvent::Key(key)
                    if key.code == KeyCode::Char('k') || key.code == KeyCode::Up =>
                {
                    state.previous();
                }
                CrosstermEvent::Key(key) if key.code == KeyCode::Char('r') => {
                    state.set_events(client.list_events(true, 100)?);
                }
                CrosstermEvent::Key(key) if key.code == KeyCode::Char('m') => {
                    if let Some(id) = state.selected_event_id() {
                        client.mark_read(id)?;
                        state.set_events(client.list_events(true, 100)?);
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;

    #[test]
    fn selection_stays_within_bounds() {
        let mut state = TuiState::default();
        state.set_events(vec![Event {
            id: 1,
            source: "github".to_string(),
            account_id: 1,
            external_id: "gh-1".to_string(),
            title: "title".to_string(),
            body: None,
            url: None,
            actor: None,
            reason: None,
            occurred_at: Utc.with_ymd_and_hms(2026, 5, 26, 1, 2, 3).single().unwrap(),
            received_at: Utc.with_ymd_and_hms(2026, 5, 26, 1, 2, 4).single().unwrap(),
            read_at: None,
            raw_json: serde_json::json!({}),
        }]);
        state.next();
        assert_eq!(state.selected, 0);
        assert_eq!(state.selected_event_id(), Some(1));
        state.set_events(Vec::new());
        assert_eq!(state.selected_event_id(), None);
    }
}
