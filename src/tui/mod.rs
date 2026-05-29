use std::{io, path::PathBuf, time::Duration};

use anyhow::Result;
use crossterm::{
    event::{self, Event as CrosstermEvent, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{ipc::IpcClient, model::Event};

const EVENT_LIMIT: usize = 100;

#[derive(Debug)]
pub struct TuiState {
    pub events: Vec<Event>,
    pub selected: usize,
    pub unread_only: bool,
    pub source_filter: Option<String>,
    pub available_sources: Vec<String>,
}

impl Default for TuiState {
    fn default() -> Self {
        Self {
            events: Vec::new(),
            selected: 0,
            unread_only: true,
            source_filter: None,
            available_sources: Vec::new(),
        }
    }
}

impl TuiState {
    pub fn set_events(&mut self, events: Vec<Event>) {
        let previous_id = self.selected_event_id();
        let previous_index = self.selected;
        self.events = events;
        if self.events.is_empty() {
            self.selected = 0;
        } else if let Some(selected) =
            previous_id.and_then(|id| self.events.iter().position(|event| event.id == id))
        {
            self.selected = selected;
        } else if previous_index >= self.events.len() {
            self.selected = self.events.len() - 1;
        } else {
            self.selected = previous_index;
        }
    }

    pub fn set_available_sources(&mut self, sources: Vec<String>) {
        let mut available_sources = Vec::new();
        for source in sources {
            if !source.is_empty() && !available_sources.contains(&source) {
                available_sources.push(source);
            }
        }
        self.available_sources = available_sources;
        if self
            .source_filter
            .as_ref()
            .is_some_and(|source| !self.available_sources.contains(source))
        {
            self.source_filter = None;
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

    pub fn selected_event(&self) -> Option<&Event> {
        self.events.get(self.selected)
    }

    pub fn cycle_source_filter(&mut self) {
        if self.available_sources.is_empty() {
            self.source_filter = None;
            return;
        }

        self.source_filter = match self.source_filter.as_deref() {
            None => self.available_sources.first().cloned(),
            Some(current) => self
                .available_sources
                .iter()
                .position(|source| source == current)
                .and_then(|index| self.available_sources.get(index + 1).cloned()),
        };
    }

    pub fn toggle_unread_only(&mut self) {
        self.unread_only = !self.unread_only;
    }

    fn source_filter_label(&self) -> &str {
        self.source_filter.as_deref().unwrap_or("all")
    }

    fn read_filter_label(&self) -> &str {
        if self.unread_only { "unread" } else { "all" }
    }

    fn source_filter_for_request(&self) -> Option<String> {
        self.source_filter.clone()
    }
}

pub fn run(socket_path: PathBuf) -> Result<()> {
    let client = IpcClient::new(socket_path);
    let mut state = TuiState::default();
    refresh_sources(&client, &mut state)?;
    refresh_events(&client, &mut state)?;

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
        terminal.draw(|frame| render_ui(frame, state))?;

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
                    refresh_sources(client, state)?;
                    refresh_events(client, state)?;
                }
                CrosstermEvent::Key(key) if key.code == KeyCode::Char('m') => {
                    if let Some(id) = state.selected_event_id() {
                        client.mark_read(id)?;
                        refresh_events(client, state)?;
                    }
                }
                CrosstermEvent::Key(key) if key.code == KeyCode::Char('u') => {
                    state.toggle_unread_only();
                    refresh_events(client, state)?;
                }
                CrosstermEvent::Key(key) if key.code == KeyCode::Char('s') => {
                    state.cycle_source_filter();
                    refresh_events(client, state)?;
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn refresh_sources(client: &IpcClient, state: &mut TuiState) -> Result<()> {
    let sources = client
        .sources()?
        .into_iter()
        .map(|source| source.source)
        .collect();
    state.set_available_sources(sources);
    Ok(())
}

fn refresh_events(client: &IpcClient, state: &mut TuiState) -> Result<()> {
    let events = client.list_events(
        state.unread_only,
        state.source_filter_for_request(),
        EVENT_LIMIT,
    )?;
    state.set_events(events);
    Ok(())
}

fn render_ui(frame: &mut Frame<'_>, state: &TuiState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(frame.area());
    let header_text = format!(
        "{} / {} / {} | q quit  j/k move  r refresh  m read  u all  s source",
        state.read_filter_label(),
        state.source_filter_label(),
        state.events.len()
    );
    let header = Paragraph::new(header_text)
        .block(Block::default().borders(Borders::ALL).title("signalpane"));
    frame.render_widget(header, chunks[0]);

    let panes = if chunks[1].width >= 100 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(chunks[1])
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(chunks[1])
    };
    render_event_list(frame, state, panes[0]);
    render_event_detail(frame, state, panes[1]);
}

fn render_event_list(frame: &mut Frame<'_>, state: &TuiState, area: Rect) {
    let items: Vec<ListItem<'_>> = if state.events.is_empty() {
        vec![ListItem::new(Line::raw("No events match current filters"))]
    } else {
        state.events.iter().map(event_list_item).collect()
    };
    let mut list_state = ListState::default();
    if !state.events.is_empty() {
        list_state.select(Some(state.selected));
    }
    let title = format!(
        "Events: {} / {}",
        state.read_filter_label(),
        state.source_filter_label()
    );
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn event_list_item(event: &Event) -> ListItem<'static> {
    let read_marker = if event.read_at.is_some() { " " } else { "*" };
    ListItem::new(Line::from(vec![
        Span::styled(
            format!("{read_marker} "),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("[{}] ", event.source),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} ", format_short_dt(&event.occurred_at)),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(event.title.clone()),
    ]))
}

fn render_event_detail(frame: &mut Frame<'_>, state: &TuiState, area: Rect) {
    let detail = Paragraph::new(Text::from(detail_lines(state.selected_event())))
        .block(Block::default().borders(Borders::ALL).title("Detail"))
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, area);
}

fn detail_lines(event: Option<&Event>) -> Vec<Line<'static>> {
    let Some(event) = event else {
        return vec![Line::raw("No event selected")];
    };

    let mut lines = vec![
        field_line("Title", &event.title),
        field_line("Body", format_optional_text(event.body.as_deref()).as_ref()),
    ];
    lines.extend([
        field_line("Source", &event.source),
        field_line(
            "Reason",
            format_optional_text(event.reason.as_deref()).as_ref(),
        ),
        field_line(
            "Actor",
            format_optional_text(event.actor.as_deref()).as_ref(),
        ),
        field_line("URL", format_optional_text(event.url.as_deref()).as_ref()),
        field_line("Occurred", &format_full_dt(&event.occurred_at)),
        field_line("Read state", &format_read_state(event)),
    ]);
    lines
}

fn field_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label}: "),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(value.to_string()),
    ])
}

fn format_optional_text(value: Option<&str>) -> String {
    value
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("-")
        .to_string()
}

fn format_read_state(event: &Event) -> String {
    event.read_at.as_ref().map_or_else(
        || "unread".to_string(),
        |read_at| format!("read at {}", format_full_dt(read_at)),
    )
}

fn format_short_dt(dt: &chrono::DateTime<chrono::Utc>) -> String {
    dt.format("%m-%d %H:%M").to_string()
}

fn format_full_dt(dt: &chrono::DateTime<chrono::Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;

    #[test]
    fn selection_stays_within_bounds() {
        let mut state = TuiState::default();
        state.set_events(vec![event(1, "github", false)]);
        state.next();
        assert_eq!(state.selected, 0);
        assert_eq!(state.selected_event_id(), Some(1));
        state.set_events(Vec::new());
        assert_eq!(state.selected_event_id(), None);
    }

    #[test]
    fn refresh_preserves_selected_event_when_it_still_exists() {
        let mut state = TuiState::default();
        state.set_events(vec![event(1, "github", false), event(2, "slack", false)]);
        state.next();

        state.set_events(vec![event(3, "github", false), event(2, "slack", false)]);

        assert_eq!(state.selected, 1);
        assert_eq!(state.selected_event_id(), Some(2));
    }

    #[test]
    fn refresh_clamps_to_same_index_when_selected_event_disappears() {
        let mut state = TuiState::default();
        state.set_events(vec![
            event(1, "github", false),
            event(2, "slack", false),
            event(3, "github", false),
        ]);
        state.next();

        state.set_events(vec![event(1, "github", false), event(3, "github", false)]);

        assert_eq!(state.selected, 1);
        assert_eq!(state.selected_event_id(), Some(3));
    }

    #[test]
    fn cycles_source_filter_through_all_and_available_sources() {
        let mut state = TuiState::default();
        state.set_available_sources(vec![
            "github".to_string(),
            "slack".to_string(),
            "github".to_string(),
        ]);

        assert_eq!(state.source_filter.as_deref(), None);
        state.cycle_source_filter();
        assert_eq!(state.source_filter.as_deref(), Some("github"));
        state.cycle_source_filter();
        assert_eq!(state.source_filter.as_deref(), Some("slack"));
        state.cycle_source_filter();
        assert_eq!(state.source_filter.as_deref(), None);
    }

    #[test]
    fn toggles_unread_only_filter() {
        let mut state = TuiState::default();

        assert!(state.unread_only);
        state.toggle_unread_only();
        assert!(!state.unread_only);
        state.toggle_unread_only();
        assert!(state.unread_only);
    }

    fn event(id: i64, source: &str, read: bool) -> Event {
        Event {
            id,
            source: source.to_string(),
            account_id: 1,
            external_id: format!("{source}-{id}"),
            title: format!("title {id}"),
            body: Some(format!("body {id}")),
            url: Some(format!("https://example.com/{source}/{id}")),
            actor: Some("actor".to_string()),
            reason: Some("mention".to_string()),
            occurred_at: Utc
                .with_ymd_and_hms(2026, 5, 26, 1, 2, id as u32)
                .single()
                .unwrap(),
            received_at: Utc
                .with_ymd_and_hms(2026, 5, 26, 1, 3, id as u32)
                .single()
                .unwrap(),
            read_at: read.then(|| {
                Utc.with_ymd_and_hms(2026, 5, 26, 1, 4, id as u32)
                    .single()
                    .unwrap()
            }),
            raw_json: serde_json::json!({}),
        }
    }
}
