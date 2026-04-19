use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame, Terminal,
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::{io, path::PathBuf, time::Duration};

use crow_workspace::ledger::{EventLedger, LedgerEvent};

/// Dashboard state
struct App {
    ledger_events: Vec<LedgerEvent>,
    memories: Vec<String>,
    workspace: PathBuf,
}

impl App {
    fn new(workspace: PathBuf) -> Result<Self> {
        let mut app = App {
            ledger_events: Vec::new(),
            memories: Vec::new(),
            workspace,
        };
        app.refresh();
        Ok(app)
    }

    fn refresh(&mut self) {
        let mut hasher = DefaultHasher::new();
        self.workspace.to_string_lossy().hash(&mut hasher);
        let hash = format!("{:x}", hasher.finish());

        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."));

        let ledger_path = home
            .join(".crow")
            .join("ledger")
            .join(format!("{hash}.jsonl"));
        let memory_dir = home.join(".crow").join("memory").join(&hash);

        self.ledger_events.clear();
        if ledger_path.exists() {
            if let Ok(ledger) = EventLedger::open(&ledger_path) {
                self.ledger_events = ledger.history().to_vec();
            }
        }

        self.memories.clear();
        if memory_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&memory_dir) {
                for entry in entries.flatten() {
                    if let Ok(content) = std::fs::read_to_string(entry.path()) {
                        self.memories.push(content);
                    }
                }
            }
        }
    }
}

pub async fn run_dashboard(workspace: PathBuf) -> Result<()> {
    println!("Initializing Ratatui Dashboard...");

    // setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // create app and run it
    let app = App::new(workspace)?;
    let res = run_app(&mut terminal, app);

    // restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{err:?}")
    }

    Ok(())
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, mut app: App) -> io::Result<()> {
    loop {
        app.refresh(); // Live View update!
        terminal.draw(|f| ui(f, &app))?;

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if let KeyCode::Char('q') = key.code {
                    return Ok(());
                }
            }
        }
    }
}

fn ui(f: &mut Frame, app: &App) {
    let size = f.size();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(size);

    let header = Paragraph::new(format!(
        "🦅 Crow Code Dashboard | Workspace: {} | Press 'q' to quit",
        app.workspace.display()
    ))
    .style(
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )
    .block(Block::default().borders(Borders::ALL).title("Status"));
    f.render_widget(header, chunks[0]);

    let body_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[1]);

    // Format Ledger Events
    let events: Vec<ListItem> = app
        .ledger_events
        .iter()
        .rev() // Show newest first
        .take(50)
        .map(|e| {
            let (label, color, ts) = match e {
                LedgerEvent::SnapshotCreated { timestamp, .. } => {
                    ("SnapshotCreated", Color::Blue, timestamp)
                }
                LedgerEvent::PlanHydrated { timestamp, .. } => {
                    ("PlanHydrated", Color::Magenta, timestamp)
                }
                LedgerEvent::PreflightStarted { timestamp, .. } => {
                    ("PreflightStarted", Color::Yellow, timestamp)
                }
                LedgerEvent::PreflightTested {
                    passed, timestamp, ..
                } => {
                    if *passed {
                        ("PreflightTested (Pass)", Color::Green, timestamp)
                    } else {
                        ("PreflightTested (Fail)", Color::Red, timestamp)
                    }
                }
                LedgerEvent::PlanApplied { timestamp, .. } => {
                    ("PlanApplied", Color::Green, timestamp)
                }
                LedgerEvent::PlanRolledBack { timestamp, .. } => {
                    ("PlanRolledBack", Color::Red, timestamp)
                }
            };

            let time_str = ts.format("%H:%M:%S").to_string();
            let content = Line::from(vec![
                Span::styled(
                    format!("[{time_str}] "),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    label,
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
            ]);
            ListItem::new(content)
        })
        .collect();

    let events_list = List::new(events).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Event Ledger (Decisions & Rollbacks)"),
    );
    f.render_widget(events_list, body_chunks[0]);

    // Format Memories
    let mut memory_text = String::new();
    if app.memories.is_empty() {
        memory_text.push_str("No deep dreams found. Try running `crow dream`.");
    } else {
        for mem in app.memories.iter().take(5) {
            memory_text.push_str(mem);
            memory_text.push_str("\n\n---\n\n");
        }
    }

    let memory_widget = Paragraph::new(memory_text).block(
        Block::default()
            .borders(Borders::ALL)
            .title("AutoDream (Subconscious Fragments)"),
    );
    f.render_widget(memory_widget, body_chunks[1]);
}
