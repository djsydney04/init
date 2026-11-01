use anyhow::{Result, ensure};
use ratatui::{
    Frame,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Cell, Clear, Gauge, Paragraph, Row, Table, TableState, Wrap},
};
use self_host_init::{
    config::Limits,
    control::{AllocationRequest, Controller},
};
use serde_json::{Value, json};
use std::{
    io::{self, IsTerminal},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Mutex, mpsc};

const ACCENT: Color = Color::Rgb(151, 182, 122);

struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

enum Operation {
    Service(String, &'static str),
    Scale(String, Limits),
    Budget(Limits),
    Preview(bool),
    Allocate(bool),
    Logs(String),
}

enum Update {
    Snapshot(Value),
    Completed(Result<Option<Value>>),
    Logs(Result<String>),
}

enum Modal {
    Confirm {
        message: String,
        operation: Operation,
    },
    Resource {
        id: Option<String>,
        cpus: f64,
        memory: u64,
        field: usize,
    },
    Allocation {
        enabled: bool,
        preview: Value,
    },
}

struct App {
    state: Value,
    selection: TableState,
    modal: Option<Modal>,
    logs: Option<String>,
    message: String,
    busy: bool,
    log_scroll: u16,
}

impl App {
    fn new() -> Self {
        Self {
            state: json!({"services":[],"budget":{},"runtime":{}}),
            selection: TableState::default().with_selected(0),
            modal: None,
            logs: None,
            message: "Loading services…".into(),
            busy: false,
            log_scroll: 0,
        }
    }
    fn selected(&self) -> Option<&Value> {
        self.state["services"]
            .as_array()?
            .get(self.selection.selected().unwrap_or(0))
    }
    fn update_snapshot(&mut self, state: Value) {
        if let Some(error) = state["manifest_error"].as_str() {
            self.message = format!("Configuration error: {error}");
        } else if self.message == "Loading services…"
            || self.message.starts_with("Configuration error: ")
        {
            self.message = "Ready".into();
        }
        self.state = state;
    }
    fn select(&mut self, delta: isize) {
        let n = self.state["services"].as_array().map_or(0, Vec::len);
        if n > 0 {
            let index = self.selection.selected().unwrap_or(0).min(n - 1);
            self.selection.select(Some(
                (index as isize + delta).rem_euclid(n as isize) as usize
            ));
        }
    }
    fn key(&mut self, key: KeyEvent) -> Option<Operation> {
        if let Some(modal) = self.modal.take() {
            match modal {
                Modal::Confirm { message, operation } => match key.code {
                    KeyCode::Enter => return Some(operation),
                    KeyCode::Esc => {}
                    _ => self.modal = Some(Modal::Confirm { message, operation }),
                },
                Modal::Allocation { enabled, preview } => match key.code {
                    KeyCode::Enter => return Some(Operation::Allocate(enabled)),
                    KeyCode::Esc => {}
                    _ => self.modal = Some(Modal::Allocation { enabled, preview }),
                },
                Modal::Resource {
                    id,
                    mut cpus,
                    mut memory,
                    mut field,
                } => {
                    match key.code {
                        KeyCode::Esc => return None,
                        KeyCode::Tab | KeyCode::Up | KeyCode::Down => field = 1 - field,
                        KeyCode::Left | KeyCode::Char('-') => {
                            if field == 0 {
                                cpus = (cpus - 0.25).max(0.01)
                            } else {
                                memory = memory.saturating_sub(64).max(6)
                            }
                        }
                        KeyCode::Right | KeyCode::Char('+') => {
                            if field == 0 {
                                cpus = (cpus + 0.25).min(65536.)
                            } else {
                                memory = memory.saturating_add(64).min(1 << 40)
                            }
                        }
                        KeyCode::Enter => {
                            return Some(match id {
                                Some(id) => Operation::Scale(
                                    id,
                                    Limits {
                                        cpus,
                                        memory_mb: memory,
                                    },
                                ),
                                None => Operation::Budget(Limits {
                                    cpus,
                                    memory_mb: memory,
                                }),
                            });
                        }
                        _ => {}
                    }
                    self.modal = Some(Modal::Resource {
                        id,
                        cpus,
                        memory,
                        field,
                    });
                }
            }
            return None;
        }
        if self.logs.is_some() {
            match key.code {
                KeyCode::Esc | KeyCode::Char('l') => self.logs = None,
                KeyCode::Down | KeyCode::Char('j') => {
                    self.log_scroll = self.log_scroll.saturating_add(1)
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.log_scroll = self.log_scroll.saturating_sub(1)
                }
                _ => {}
            }
            return None;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.select(-1),
            KeyCode::Down | KeyCode::Char('j') => self.select(1),
            KeyCode::Char('b') => {
                let b = &self.state["budget"];
                self.modal = Some(Modal::Resource {
                    id: None,
                    cpus: b["cpus"].as_f64().unwrap_or(1.),
                    memory: b["memory_mb"].as_u64().unwrap_or(1024),
                    field: 0,
                });
            }
            KeyCode::Char('a') => {
                return Some(Operation::Preview(
                    !self.state["allocation"]["enabled"]
                        .as_bool()
                        .unwrap_or(false),
                ));
            }
            KeyCode::Char('l') | KeyCode::Enter => {
                if let Some(s) = self.selected() {
                    return Some(Operation::Logs(text(s, "id")));
                }
            }
            KeyCode::Char('e') => {
                if let Some(s) = self
                    .selected()
                    .filter(|s| s["capabilities"]["scale"] == true && s["enabled"] != false)
                {
                    self.modal = Some(Modal::Resource {
                        id: Some(text(s, "id")),
                        cpus: s["limits"]["cpus"].as_f64().unwrap_or(1.),
                        memory: s["limits"]["memory_mb"].as_u64().unwrap_or(256),
                        field: 0,
                    });
                }
            }
            KeyCode::Char(c @ ('s' | 'x' | 'r' | 'u')) => {
                if let Some(s) = self.selected() {
                    if s["observe_only"] == true {
                        self.message="This service is observed. Enable management in its manifest before changing it.".into();
                        return None;
                    }
                    let action = match c {
                        's' => "start",
                        'x' => "stop",
                        'r' => "restart",
                        _ => "run",
                    };
                    let id = text(s, "id");
                    self.modal = Some(Modal::Confirm {
                        message: format!("{action} {id}?"),
                        operation: Operation::Service(id, action),
                    });
                }
            }
            _ => {}
        }
        None
    }

    fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        if area.width < 65 || area.height < 15 {
            f.render_widget(
                Paragraph::new("Resize the terminal to at least 65 × 15. Press q to quit."),
                area,
            );
            return;
        }
        let zones = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(2),
        ])
        .split(area);
        let b = &self.state["budget"];
        let mode = if self.state["allocation"]["enabled"] == true {
            "Automatic"
        } else {
            "Manual"
        };
        f.render_widget(
            Paragraph::new(format!(
                "Self Host  ·  {} CPUs / {} MiB  ·  {mode}\n{}",
                b["cpus"].as_f64().unwrap_or(0.),
                b["memory_mb"].as_u64().unwrap_or(0),
                self.state["workspace"]
                    .as_str()
                    .unwrap_or("Loading workspace")
            ))
            .style(Style::default().fg(ACCENT)),
            zones[0],
        );
        if let Some(logs) = &self.logs {
            f.render_widget(
                Paragraph::new(logs.as_str())
                    .block(Block::bordered().title("Logs · Esc to close"))
                    .scroll((self.log_scroll, 0)),
                zones[1],
            );
        } else {
            let rows = self.state["services"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|s| {
                    let cap = if s["effective_limits"].is_object() {
                        &s["effective_limits"]
                    } else {
                        &s["limits"]
                    };
                    let cpu = s["cpu_percent"]
                        .as_f64()
                        .map(|v| format!("{v:.1}%"))
                        .unwrap_or_else(|| "—".into());
                    let memory = s["memory_mb"]
                        .as_f64()
                        .map(|v| format!("{v:.0}"))
                        .unwrap_or_else(|| "—".into());
                    let limits = cap["cpus"]
                        .as_f64()
                        .map(|v| format!("{v:.2}/{}", cap["memory_mb"]))
                        .unwrap_or_else(|| "Observed".into());
                    Row::new([
                        Cell::from(text(s, "name")),
                        Cell::from(text(s, "status")),
                        Cell::from(text(s, "runtime")),
                        Cell::from(cpu),
                        Cell::from(memory),
                        Cell::from(limits),
                    ])
                });
            let header = Row::new(["Service", "State", "Runtime", "CPU", "MiB", "CPU/MiB cap"])
                .style(Style::default().add_modifier(Modifier::BOLD));
            let table = Table::new(
                rows,
                [
                    Constraint::Min(18),
                    Constraint::Length(12),
                    Constraint::Length(8),
                    Constraint::Length(7),
                    Constraint::Length(7),
                    Constraint::Length(16),
                ],
            )
            .header(header)
            .row_highlight_style(Style::default().bg(Color::DarkGray))
            .block(Block::default().borders(Borders::TOP | Borders::BOTTOM));
            f.render_stateful_widget(table, zones[1], &mut self.selection);
        }
        f.render_widget(
            Paragraph::new(self.message.as_str())
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(if self.busy {
                    Color::Yellow
                } else {
                    Color::Gray
                })),
            zones[2],
        );
        f.render_widget(Paragraph::new("↑↓ Select  l Logs  s Start  x Stop  r Restart  u Run  e Limits  b Budget  a Auto allocation  q Quit").wrap(Wrap{trim:true}),zones[3]);
        if let Some(modal) = &self.modal {
            let popup = center(area, 76, 60);
            f.render_widget(Clear, popup);
            match modal {
                Modal::Confirm { message, .. } => f.render_widget(
                    Paragraph::new(format!("{message}\n\nEnter to confirm · Esc to cancel"))
                        .wrap(Wrap { trim: true })
                        .block(Block::bordered().title("Confirm action")),
                    popup,
                ),
                Modal::Resource {
                    id,
                    cpus,
                    memory,
                    field,
                } => {
                    let title = id.as_deref().unwrap_or("Workspace budget");
                    let regions = Layout::vertical([
                        Constraint::Length(3),
                        Constraint::Length(3),
                        Constraint::Min(2),
                    ])
                    .margin(1)
                    .split(popup);
                    f.render_widget(Block::bordered().title(title), popup);
                    let max_cpu = self.state["runtime"]["cpus"]
                        .as_f64()
                        .unwrap_or(8.)
                        .max(*cpus);
                    let max_memory = self.state["runtime"]["memory_mb"]
                        .as_f64()
                        .unwrap_or(8192.)
                        .max(*memory as f64);
                    f.render_widget(
                        Gauge::default()
                            .ratio((*cpus / max_cpu).clamp(0., 1.))
                            .label(format!("CPU: {cpus:.2}"))
                            .gauge_style(Style::default().fg(if *field == 0 {
                                ACCENT
                            } else {
                                Color::Gray
                            }))
                            .block(Block::default().title(if *field == 0 {
                                "CPU · selected"
                            } else {
                                "CPU"
                            })),
                        regions[0],
                    );
                    f.render_widget(
                        Gauge::default()
                            .ratio((*memory as f64 / max_memory).clamp(0., 1.))
                            .label(format!("Memory: {memory} MiB"))
                            .gauge_style(Style::default().fg(if *field == 1 {
                                ACCENT
                            } else {
                                Color::Gray
                            }))
                            .block(Block::default().title(if *field == 1 {
                                "Memory · selected"
                            } else {
                                "Memory"
                            })),
                        regions[1],
                    );
                    f.render_widget(Paragraph::new("Tab selects a resource. ←/→ or −/+ adjusts it.\nEnter applies · Esc cancels\nChanges must fit the workspace budget." ).wrap(Wrap{trim:true}),regions[2]);
                }
                Modal::Allocation { enabled, preview } => {
                    let mut lines = vec![
                        Line::from(if *enabled {
                            "Enable automatic allocation"
                        } else {
                            "Return to manual allocation"
                        }),
                        Line::from(""),
                    ];
                    for row in preview["allocations"].as_array().into_iter().flatten() {
                        lines.push(Line::from(format!(
                            "{}: {:.2} CPU / {} MiB",
                            text(row, "id"),
                            row["cpus"].as_f64().unwrap_or(0.),
                            row["memory_mb"]
                        )));
                    }
                    lines.push(Line::from(""));
                    for warning in preview["warnings"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                    {
                        lines.push(Line::styled(warning, Style::default().fg(Color::Yellow)));
                    }
                    lines.push(Line::from("Enter applies · Esc cancels"));
                    f.render_widget(
                        Paragraph::new(lines)
                            .wrap(Wrap { trim: true })
                            .block(Block::bordered().title("Allocation preview")),
                        popup,
                    );
                }
            }
        }
    }
}

fn text(s: &Value, key: &str) -> String {
    s[key].as_str().unwrap_or("—").to_string()
}
fn center(area: Rect, width: u16, height: u16) -> Rect {
    let w = area.width.saturating_mul(width) / 100;
    let h = (area.height.saturating_mul(height) / 100)
        .max(8)
        .min(area.height);
    Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    )
}

pub async fn run(controller: Controller) -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "The TUI needs an interactive terminal. Use selfhost status --json in scripts."
    );
    let mut terminal = ratatui::init();
    let _restore = Restore;
    let controller = Arc::new(Mutex::new(controller));
    let (tx, mut updates) = mpsc::unbounded_channel();
    let snapshot_controller = controller.clone();
    let snapshot_tx = tx.clone();
    let refresher = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let snapshot = snapshot_controller.lock().await.snapshot().await;
            if snapshot_tx.send(Update::Snapshot(snapshot)).is_err() {
                break;
            }
        }
    });
    let mut app = App::new();
    let mut dirty = true;
    let mut pending: Option<tokio::task::JoinHandle<()>> = None;
    let result = async {
        loop {
            if dirty {
                terminal.draw(|f| app.draw(f))?;
                dirty = false;
            }
            while let Ok(update) = updates.try_recv() {
                dirty = true;
                match update {
                    Update::Snapshot(state) => {
                        app.update_snapshot(state);
                    }
                    Update::Completed(result) => {
                        app.busy = false;
                        match result {
                            Ok(Some(preview)) => {
                                let enabled = preview["enabled"].as_bool().unwrap_or(false);
                                app.modal = Some(Modal::Allocation { enabled, preview });
                                app.message =
                                    "Review the proposed allocation before applying it.".into();
                            }
                            Ok(None) => app.message = "Applied".into(),
                            Err(error) => app.message = format!("{error:#}"),
                        }
                    }
                    Update::Logs(result) => {
                        app.busy = false;
                        match result {
                            Ok(logs) => {
                                app.logs = Some(logs);
                                app.log_scroll = 0;
                                app.message = "Esc returns to services.".into();
                            }
                            Err(e) => app.message = format!("{e:#}"),
                        }
                    }
                }
            }
            if event::poll(Duration::from_millis(50))? {
                let input = event::read()?;
                if matches!(input, Event::Resize(..)) {
                    dirty = true;
                }
                let Event::Key(key) = input else {
                    continue;
                };
                dirty = true;
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if key.code == KeyCode::Char('q')
                    || (key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL))
                {
                    break;
                }
                if app.busy {
                    continue;
                }
                if let Some(operation) = app.key(key) {
                    app.busy = true;
                    app.message = "Working…".into();
                    let worker = controller.clone();
                    let worker_tx = tx.clone();
                    pending = Some(tokio::spawn(async move {
                        let mut controller = worker.lock().await;
                        if let Operation::Logs(id) = operation {
                            let _ = worker_tx.send(Update::Logs(controller.logs(&id, 200).await));
                            return;
                        }
                        let result = match operation {
                            Operation::Service(id, action) => {
                                controller.action(&id, action, None).await.map(|_| None)
                            }
                            Operation::Scale(id, limits) => controller
                                .action(&id, "apply", Some(limits))
                                .await
                                .map(|_| None),
                            Operation::Budget(limits) => {
                                controller.set_budget(limits).await.map(|_| None)
                            }
                            Operation::Preview(enabled) => controller
                                .configure_allocation(AllocationRequest {
                                    enabled: Some(enabled),
                                    ..Default::default()
                                })
                                .await
                                .map(Some),
                            Operation::Allocate(enabled) => controller
                                .configure_allocation(AllocationRequest {
                                    enabled: Some(enabled),
                                    apply: true,
                                    ..Default::default()
                                })
                                .await
                                .map(|_| None),
                            Operation::Logs(_) => unreachable!(),
                        };
                        let _ = worker_tx.send(Update::Completed(result));
                        let _ = worker_tx.send(Update::Snapshot(controller.snapshot().await));
                    }));
                }
            }
            tokio::task::yield_now().await;
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    refresher.abort();
    // Finish a submitted mutation before exiting; cancellation could leave only
    // part of a resource change applied. The terminal is restored while waiting.
    ratatui::restore();
    if let Some(pending) = pending {
        pending.await?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    #[test]
    fn resources_cancel_without_mutation_and_observers_cannot_start() {
        let mut app = App::new();
        app.state = json!({"services":[{"id":"external","observe_only":true}],"budget":{"cpus":2,"memory_mb":1024}});
        assert!(
            app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE))
                .is_none()
        );
        assert!(app.modal.is_none());
        app.key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        assert!(app.modal.is_some());
        assert!(
            app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
                .is_none()
        );
        assert!(app.modal.is_none());
    }
    #[test]
    fn renders_empty_and_narrow_terminal_without_panicking() {
        for (w, h) in [(100, 24), (40, 10)] {
            let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
            let mut app = App::new();
            t.draw(|f| app.draw(f)).unwrap();
        }
    }
    #[test]
    fn configuration_errors_are_visible_until_the_manifest_is_fixed() {
        let mut app = App::new();
        app.update_snapshot(json!({"services":[],"manifest_error":null}));
        assert_eq!(app.message, "Ready");
        app.update_snapshot(json!({"services":[],"manifest_error":"Invalid YAML at line 3"}));
        assert_eq!(app.message, "Configuration error: Invalid YAML at line 3");
        app.update_snapshot(json!({"services":[],"manifest_error":null}));
        assert_eq!(app.message, "Ready");
    }
}
