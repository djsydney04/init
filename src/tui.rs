use anyhow::{Result, ensure};
use init_core::{
    config::{Limits, Project},
    control::{Controller, DeploymentService},
    machines::Machine,
};
use ratatui::{
    Frame,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap},
};
use serde_json::{Value, json};
use std::{
    cmp::Ordering,
    io::{self, IsTerminal},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Mutex, mpsc};

mod backend;
mod host;
use backend::{Backend, Target};
use host::HostView;

const BACKGROUND: Color = Color::Rgb(27, 28, 30);
const FOREGROUND: Color = Color::Rgb(239, 237, 231);
const MUTED: Color = Color::Rgb(174, 174, 170);
const SELECTED: Color = Color::Rgb(58, 59, 61);

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
    DeployPreview {
        path: PathBuf,
        project: Option<String>,
    },
    Deploy(Box<DeploymentService>),
    CreateProject(Project),
    Machines,
}
enum Update {
    Snapshot(Result<Value>),
    Completed(Result<Option<Value>>),
    Logs {
        service_id: String,
        result: Result<String>,
    },
    Deployment(Result<(Box<DeploymentService>, Value)>),
    ProjectCreated {
        project_id: String,
        result: Result<()>,
    },
    Machines(Result<Vec<Machine>>),
    Switched(Result<Value>),
}
enum Command {
    Execute(Operation),
    Switch(Target),
    Refresh,
}
#[derive(Clone, PartialEq, Eq)]
enum Scope {
    All,
    Default,
    Project(String),
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Services,
    Host,
}
#[derive(Clone, Copy)]
enum Sort {
    State,
    Name,
    Cpu,
    Memory,
}
impl Sort {
    fn next(self) -> Self {
        match self {
            Self::State => Self::Name,
            Self::Name => Self::Cpu,
            Self::Cpu => Self::Memory,
            Self::Memory => Self::State,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Name => "name",
            Self::Cpu => "CPU ↓",
            Self::Memory => "memory ↓",
        }
    }
}
struct LogView {
    service_id: String,
    content: String,
    scroll: u16,
    follow: bool,
    loading: bool,
    error: Option<String>,
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
        scroll: u16,
    },
    DeploymentFile {
        path: String,
        error: Option<String>,
    },
    Deployment {
        service: Box<DeploymentService>,
        preview: Value,
        scroll: u16,
    },
    Workspaces {
        selection: usize,
    },
    NewWorkspace {
        fields: [String; 3],
        field: usize,
        error: Option<String>,
    },
    Machines {
        items: Vec<Machine>,
        selection: usize,
        loading: bool,
        error: Option<String>,
    },
    Search {
        previous: String,
    },
    Help {
        scroll: u16,
    },
}
struct App {
    state: Value,
    ready: bool,
    selected_id: Option<String>,
    selection: TableState,
    scope: Scope,
    query: String,
    sort: Sort,
    view: View,
    target: Target,
    generation: u64,
    modal: Option<Modal>,
    logs: Option<LogView>,
    host: HostView,
    message: String,
    busy: bool,
    refreshing: bool,
    too_small: bool,
}
impl App {
    fn new() -> Self {
        Self {
            state: json!({"services":[],"projects":[]}),
            ready: false,
            selected_id: None,
            selection: TableState::default(),
            scope: Scope::All,
            query: String::new(),
            sort: Sort::State,
            view: View::Services,
            target: Target::Local,
            generation: 0,
            modal: None,
            logs: None,
            host: HostView::default(),
            message: "Loading this computer…".into(),
            busy: false,
            refreshing: false,
            too_small: false,
        }
    }
    fn target_label(&self) -> String {
        match &self.target {
            Target::Local => "This computer".into(),
            Target::Remote(machine) => {
                format!("{} · SSH {}", machine.profile.name, machine.profile.host)
            }
        }
    }
    fn scope_label(&self) -> String {
        match &self.scope {
            Scope::All => "All workspaces".into(),
            Scope::Default => "Default (ungrouped)".into(),
            Scope::Project(id) => self.state["projects"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|p| p["id"].as_str() == Some(id))
                .map(|p| text(p, "name"))
                .unwrap_or_else(|| id.clone()),
        }
    }
    fn workspaces(&self) -> Vec<(Scope, String)> {
        let mut groups = vec![
            (Scope::All, "All workspaces".into()),
            (Scope::Default, "Default (ungrouped)".into()),
        ];
        for p in self.state["projects"].as_array().into_iter().flatten() {
            if let Some(id) = p["id"].as_str() {
                groups.push((
                    Scope::Project(id.into()),
                    format!("{} · {}", text(p, "name"), id),
                ));
            }
        }
        for s in self.state["services"].as_array().into_iter().flatten() {
            if let Some(id) = s["project"].as_str().filter(|id| !id.is_empty()) {
                let scope = Scope::Project(id.into());
                if !groups.iter().any(|(existing, _)| existing == &scope) {
                    groups.push((scope, format!("{id} · unlisted workspace")));
                }
            }
        }
        groups
    }
    fn visible(&self) -> Vec<&Value> {
        let query = self.query.to_lowercase();
        let mut rows: Vec<_> = self.state["services"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| {
                let project = s["project"].as_str().filter(|id| !id.is_empty());
                let in_scope = match &self.scope {
                    Scope::All => true,
                    Scope::Default => project.is_none(),
                    Scope::Project(id) => project == Some(id),
                };
                in_scope
                    && (query.is_empty()
                        || ["id", "name", "description"].into_iter().any(|key| {
                            s[key]
                                .as_str()
                                .is_some_and(|value| value.to_lowercase().contains(&query))
                        }))
            })
            .collect();
        rows.sort_by(|a, b| {
            let order = match self.sort {
                Sort::State => state_rank(a).cmp(&state_rank(b)),
                Sort::Name => text(a, "name")
                    .to_lowercase()
                    .cmp(&text(b, "name").to_lowercase()),
                Sort::Cpu => metric_order(a, b, "cpu_percent"),
                Sort::Memory => metric_order(a, b, "memory_mb"),
            };
            order.then_with(|| text(a, "id").cmp(&text(b, "id")))
        });
        rows
    }
    fn reconcile(&mut self) {
        let rows = self.visible();
        let index = self
            .selected_id
            .as_deref()
            .and_then(|id| rows.iter().position(|s| s["id"].as_str() == Some(id)))
            .or_else(|| (!rows.is_empty()).then_some(0));
        let selected = index.map(|i| text(rows[i], "id"));
        self.selected_id = selected;
        self.selection.select(index);
    }
    fn selected(&self) -> Option<&Value> {
        if !self.ready {
            return None;
        }
        let id = self.selected_id.as_deref()?;
        self.visible()
            .into_iter()
            .find(|s| s["id"].as_str() == Some(id))
    }
    fn select(&mut self, delta: isize) {
        let rows = self.visible();
        if rows.is_empty() {
            return;
        }
        let index = rows
            .iter()
            .position(|s| Some(text(s, "id")) == self.selected_id)
            .unwrap_or(0);
        let next = (index as isize + delta).rem_euclid(rows.len() as isize) as usize;
        let id = text(rows[next], "id");
        self.selected_id = Some(id);
        self.selection.select(Some(next));
    }
    fn update_snapshot(&mut self, result: Result<Value>) {
        self.refreshing = false;
        match result {
            Ok(state) => {
                let was_ready = self.ready;
                self.ready = true;
                if let Some(error) = state["manifest_error"].as_str() {
                    self.message = format!("Configuration error: {error}");
                } else if !was_ready || self.message.starts_with("Configuration error: ") {
                    self.message = "Ready · snapshots every 5s".into();
                }
                self.host.record(&state);
                self.state = state;
                self.reconcile();
            }
            Err(error) => {
                self.ready = false;
                self.state = json!({"services":[],"projects":[]});
                self.selected_id = None;
                self.selection.select(None);
                self.message = format!(
                    "Cannot read {}: {error:#}. F5 retries; m changes machine.",
                    self.target_label()
                );
            }
        }
    }
    fn switch_target(&mut self, target: Target) {
        self.generation = self.generation.wrapping_add(1);
        self.target = target;
        self.ready = false;
        self.busy = true;
        self.state = json!({"services":[],"projects":[]});
        self.selected_id = None;
        self.selection = TableState::default();
        self.scope = Scope::All;
        self.query.clear();
        self.modal = None;
        self.logs = None;
        self.host = HostView::default();
        self.message = format!("Connecting to {}…", self.target_label());
    }
    fn update(&mut self, generation: u64, update: Update) {
        if generation != self.generation {
            return;
        }
        match update {
            Update::Snapshot(result) => self.update_snapshot(result),
            Update::Switched(result) => {
                self.busy = false;
                self.update_snapshot(result);
            }
            Update::Completed(result) => {
                self.busy = false;
                match result {
                    Ok(Some(preview)) => {
                        let enabled = preview["enabled"].as_bool().unwrap_or(false);
                        self.modal = Some(Modal::Allocation {
                            enabled,
                            preview,
                            scroll: 0,
                        });
                        self.message =
                            "Review all controller allocation changes before applying.".into();
                    }
                    Ok(None) => self.message = "Applied · refreshing services…".into(),
                    Err(error) => self.message = format!("{error:#}"),
                }
            }
            Update::Logs { service_id, result } => {
                if let Some(logs) = self
                    .logs
                    .as_mut()
                    .filter(|logs| logs.service_id == service_id)
                {
                    logs.loading = false;
                    match result {
                        Ok(content) => {
                            logs.content = content;
                            logs.error = None;
                        }
                        Err(error) => logs.error = Some(format!("{error:#}")),
                    }
                }
            }
            Update::Deployment(result) => {
                self.busy = false;
                match result {
                    Ok((service, preview)) => {
                        self.modal = Some(Modal::Deployment {
                            service,
                            preview,
                            scroll: 0,
                        });
                        self.message = "Review the deployment. Enter applies; Esc cancels.".into();
                    }
                    Err(error) => {
                        let error = format!("{error:#}");
                        if let Some(Modal::DeploymentFile { error: field, .. }) = &mut self.modal {
                            *field = Some(error.clone());
                        }
                        self.message = error;
                    }
                }
            }
            Update::ProjectCreated { project_id, result } => {
                self.busy = false;
                match result {
                    Ok(()) => {
                        self.scope = Scope::Project(project_id);
                        self.query.clear();
                        self.modal = None;
                        self.view = View::Services;
                        self.reconcile();
                        self.message = "Workspace created · d deploys a service here.".into();
                    }
                    Err(error) => {
                        let error = format!("{error:#}");
                        if let Some(Modal::NewWorkspace { error: field, .. }) = &mut self.modal {
                            *field = Some(error.clone());
                        }
                        self.message = error;
                    }
                }
            }
            Update::Machines(result) => {
                self.busy = false;
                let selection = match &self.target {
                    Target::Local => 0,
                    Target::Remote(current) => result
                        .as_ref()
                        .ok()
                        .and_then(|items| {
                            items
                                .iter()
                                .position(|item| item.profile.id == current.profile.id)
                        })
                        .map_or(0, |i| i + 1),
                };
                if let Some(Modal::Machines {
                    items,
                    selection: row,
                    loading,
                    error,
                }) = &mut self.modal
                {
                    *loading = false;
                    *row = selection;
                    match result {
                        Ok(machines) => {
                            *items = machines;
                            *error = None
                        }
                        Err(failure) => *error = Some(format!("{failure:#}")),
                    }
                }
            }
        }
    }
    fn text_input(&self) -> bool {
        matches!(
            self.modal,
            Some(Modal::DeploymentFile { .. } | Modal::NewWorkspace { .. } | Modal::Search { .. })
        )
    }
    fn wants_quit(&self, key: KeyEvent) -> bool {
        (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
            || (key.code == KeyCode::Char('q') && !self.text_input())
    }
    fn key(&mut self, key: KeyEvent) -> Option<Command> {
        if self.too_small {
            return None;
        }
        if !self.ready && key.code == KeyCode::F(5) && !self.busy {
            return Some(Command::Refresh);
        }
        if !self.ready
            && key.code == KeyCode::Enter
            && matches!(
                self.modal,
                Some(
                    Modal::Confirm { .. }
                        | Modal::Resource { .. }
                        | Modal::Allocation { .. }
                        | Modal::DeploymentFile { .. }
                        | Modal::Deployment { .. }
                        | Modal::NewWorkspace { .. }
                )
            )
        {
            self.message =
                "Connection unavailable. F5 refreshes before submitting; Esc cancels.".into();
            return None;
        }
        if self.busy {
            if key.code == KeyCode::Esc && matches!(self.modal, Some(Modal::Machines { .. })) {
                self.modal = None;
            }
            return None;
        }
        if let Some(modal) = self.modal.take() {
            match modal {
                Modal::Confirm { message, operation } => match key.code {
                    KeyCode::Enter => return Some(Command::Execute(operation)),
                    KeyCode::Esc => {}
                    _ => self.modal = Some(Modal::Confirm { message, operation }),
                },
                Modal::Workspaces { mut selection } => {
                    let entries = self.workspaces();
                    match key.code {
                        KeyCode::Esc => return None,
                        KeyCode::Up | KeyCode::Char('k') => selection = selection.saturating_sub(1),
                        KeyCode::Down | KeyCode::Char('j') => {
                            selection = (selection + 1).min(entries.len() - 1)
                        }
                        KeyCode::Enter => {
                            self.scope = entries[selection.min(entries.len() - 1)].0.clone();
                            self.reconcile();
                            return None;
                        }
                        _ => {}
                    }
                    self.modal = Some(Modal::Workspaces { selection });
                }
                Modal::Machines {
                    items,
                    mut selection,
                    loading,
                    mut error,
                } => {
                    match key.code {
                        KeyCode::Esc => return None,
                        KeyCode::Up | KeyCode::Char('k') => selection = selection.saturating_sub(1),
                        KeyCode::Down | KeyCode::Char('j') => {
                            selection = (selection + 1).min(items.len())
                        }
                        KeyCode::Enter => {
                            let target = if selection == 0 {
                                Some(Target::Local)
                            } else {
                                items
                                    .get(selection - 1)
                                    .filter(|machine| machine.identity.is_some())
                                    .cloned()
                                    .map(Target::Remote)
                            };
                            if let Some(target) = target {
                                return Some(Command::Switch(target));
                            }
                            error = Some(
                                "Test this connection in Machines in the desktop or web console before using it."
                                    .into(),
                            );
                        }
                        _ => {}
                    }
                    self.modal = Some(Modal::Machines {
                        items,
                        selection,
                        loading,
                        error,
                    });
                }
                Modal::Search { previous } => {
                    match key.code {
                        KeyCode::Esc => {
                            self.query = previous;
                            self.reconcile();
                            return None;
                        }
                        KeyCode::Enter => return None,
                        _ => {
                            edit(&mut self.query, key);
                        }
                    }
                    self.reconcile();
                    self.modal = Some(Modal::Search { previous });
                }
                Modal::NewWorkspace {
                    mut fields,
                    mut field,
                    mut error,
                } => {
                    match key.code {
                        KeyCode::Esc => return None,
                        KeyCode::Tab | KeyCode::Down => field = (field + 1) % 3,
                        KeyCode::BackTab | KeyCode::Up => field = (field + 2) % 3,
                        KeyCode::Enter => {
                            let project = Project {
                                id: fields[0].trim().into(),
                                name: if fields[1].trim().is_empty() {
                                    fields[0].trim().into()
                                } else {
                                    fields[1].trim().into()
                                },
                                description: fields[2].trim().into(),
                            };
                            match project.validate() {
                                Ok(()) => {
                                    self.modal = Some(Modal::NewWorkspace {
                                        fields,
                                        field,
                                        error: None,
                                    });
                                    return Some(Command::Execute(Operation::CreateProject(
                                        project,
                                    )));
                                }
                                Err(failure) => error = Some(failure.to_string()),
                            }
                        }
                        _ => {
                            if edit(&mut fields[field], key) {
                                error = None;
                            }
                        }
                    }
                    self.modal = Some(Modal::NewWorkspace {
                        fields,
                        field,
                        error,
                    });
                }
                Modal::DeploymentFile {
                    mut path,
                    mut error,
                } => {
                    match key.code {
                        KeyCode::Esc => {
                            self.message = "Deployment canceled.".into();
                            return None;
                        }
                        KeyCode::Enter if !path.trim().is_empty() => {
                            let project = match &self.scope {
                                Scope::Project(id) => Some(id.clone()),
                                _ => None,
                            };
                            let operation = Operation::DeployPreview {
                                path: PathBuf::from(path.trim()),
                                project,
                            };
                            self.modal = Some(Modal::DeploymentFile { path, error: None });
                            return Some(Command::Execute(operation));
                        }
                        _ => {
                            if edit(&mut path, key) {
                                error = None;
                            }
                        }
                    }
                    self.modal = Some(Modal::DeploymentFile { path, error });
                }
                Modal::Deployment {
                    service,
                    preview,
                    mut scroll,
                } => {
                    match key.code {
                        KeyCode::Enter => {
                            return Some(Command::Execute(Operation::Deploy(service)));
                        }
                        KeyCode::Esc => {
                            self.message = "Deployment canceled.".into();
                            return None;
                        }
                        _ => scroll_key(&mut scroll, key),
                    }
                    self.modal = Some(Modal::Deployment {
                        service,
                        preview,
                        scroll,
                    });
                }
                Modal::Allocation {
                    enabled,
                    preview,
                    mut scroll,
                } => {
                    match key.code {
                        KeyCode::Enter => {
                            return Some(Command::Execute(Operation::Allocate(enabled)));
                        }
                        KeyCode::Esc => return None,
                        _ => scroll_key(&mut scroll, key),
                    }
                    self.modal = Some(Modal::Allocation {
                        enabled,
                        preview,
                        scroll,
                    });
                }
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
                            let limits = Limits {
                                cpus,
                                memory_mb: memory,
                            };
                            return Some(Command::Execute(match id {
                                Some(id) => Operation::Scale(id, limits),
                                None => Operation::Budget(limits),
                            }));
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
                Modal::Help { mut scroll } => {
                    if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
                        return None;
                    }
                    scroll_key(&mut scroll, key);
                    self.modal = Some(Modal::Help { scroll });
                }
            }
            return None;
        }
        if key.code == KeyCode::Char('?') {
            self.modal = Some(Modal::Help { scroll: 0 });
            return None;
        }
        if let Some(logs) = &mut self.logs {
            match key.code {
                KeyCode::Esc | KeyCode::Char('l') => self.logs = None,
                KeyCode::Char('f') | KeyCode::End => logs.follow = true,
                KeyCode::Up | KeyCode::Char('k') | KeyCode::PageUp | KeyCode::Home => {
                    logs.follow = false;
                    scroll_key(&mut logs.scroll, key);
                }
                KeyCode::Down | KeyCode::Char('j') | KeyCode::PageDown => {
                    logs.follow = false;
                    scroll_key(&mut logs.scroll, key);
                }
                KeyCode::F(5) if !logs.loading => {
                    logs.loading = true;
                    return Some(Command::Execute(Operation::Logs(logs.service_id.clone())));
                }
                _ => {}
            }
            return None;
        }
        match key.code {
            KeyCode::Char('m')=>{self.modal=Some(Modal::Machines{items:vec![],selection:0,loading:true,error:None});return Some(Command::Execute(Operation::Machines));}
            KeyCode::F(5)=>return Some(Command::Refresh),
            KeyCode::Char('h')=>self.view=if self.view==View::Services{View::Host}else{View::Services},
            KeyCode::Esc if self.view==View::Host=>self.view=View::Services,
            KeyCode::Up|KeyCode::Char('k') if self.view==View::Host=>self.host.scroll(-1),
            KeyCode::Down|KeyCode::Char('j') if self.view==View::Host=>self.host.scroll(1),
            KeyCode::PageUp if self.view==View::Host=>self.host.scroll(-10),
            KeyCode::PageDown if self.view==View::Host=>self.host.scroll(10),
            _ if self.view==View::Host=>{},
            KeyCode::Char('w')=>{let entries=self.workspaces();let selection=entries.iter().position(|(scope,_)|scope==&self.scope).unwrap_or(0);self.modal=Some(Modal::Workspaces{selection});}
            KeyCode::Char('/')=>self.modal=Some(Modal::Search{previous:self.query.clone()}),
            KeyCode::Char('o')=>{self.sort=self.sort.next();self.reconcile();}
            KeyCode::Up|KeyCode::Char('k')=>self.select(-1),KeyCode::Down|KeyCode::Char('j')=>self.select(1),
            _ if !self.ready=>self.message="Connect to a controller before changing services. m selects a machine; F5 retries.".into(),
            KeyCode::Char('n')=>self.modal=Some(Modal::NewWorkspace{fields:Default::default(),field:0,error:None}),
            KeyCode::Char('d')=>self.modal=Some(Modal::DeploymentFile{path:String::new(),error:None}),
            KeyCode::Char('b')=>self.modal=Some(Modal::Resource{id:None,cpus:self.state["budget"]["cpus"].as_f64().unwrap_or(1.),memory:self.state["budget"]["memory_mb"].as_u64().unwrap_or(1024),field:0}),
            KeyCode::Char('a')=>return Some(Command::Execute(Operation::Preview(self.state["allocation"]["enabled"]!=true))),
            KeyCode::Char('l')|KeyCode::Enter=>if let Some(s)=self.selected(){let id=text(s,"id");self.logs=Some(LogView{service_id:id.clone(),content:"Loading logs…".into(),scroll:0,follow:true,loading:true,error:None});return Some(Command::Execute(Operation::Logs(id)));},
            KeyCode::Char('e')=>if let Some(s)=self.selected().filter(|s|s["capabilities"]["scale"]==true&&s["enabled"]!=false){self.modal=Some(Modal::Resource{id:Some(text(s,"id")),cpus:s["limits"]["cpus"].as_f64().unwrap_or(1.),memory:s["limits"]["memory_mb"].as_u64().unwrap_or(256),field:0});},
            KeyCode::Char(c@('s'|'x'|'r'|'u'))=>if let Some(s)=self.selected(){
                if s["observe_only"]==true || s["capabilities"]["lifecycle"]==false {self.message="This service is observed. Enable management in its manifest to change it.".into();return None;}
                let action=match c{'s'=>"start",'x'=>"stop",'r'=>"restart",_=>"run"};let id=text(s,"id");
                self.modal=Some(Modal::Confirm{message:format!("{action} {} ({id}) on {}?",text(s,"name"),self.target_label()),operation:Operation::Service(id,action)});
            },_=>{}
        }
        None
    }
    fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        f.render_widget(
            Block::default().style(Style::default().bg(BACKGROUND).fg(FOREGROUND)),
            area,
        );
        self.too_small = area.width < 65 || area.height < 15;
        if self.too_small {
            f.render_widget(Paragraph::new(format!("INIT · {}\n\nResize to at least 65 × 15.\nYour selection and unfinished input are kept.\n\nCtrl+C quits safely.",self.target_label())).wrap(Wrap{trim:false}),area);
            return;
        }
        let detail_height = if self.view == View::Services && self.logs.is_none() {
            if area.height >= 24 { 4 } else { 2 }
        } else {
            0
        };
        let zones = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(detail_height),
            Constraint::Length(2),
            Constraint::Length(2),
        ])
        .split(area);
        let heading = if self.view == View::Host {
            "Host activity"
        } else {
            "Services"
        };
        let query = if self.query.is_empty() {
            String::new()
        } else {
            format!(" · /{}", self.query)
        };
        f.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled("INIT  ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(self.target_label()),
                    Span::styled(
                        if self.busy {
                            " · working…"
                        } else if self.refreshing {
                            " · refreshing…"
                        } else {
                            ""
                        },
                        Style::default().fg(MUTED),
                    ),
                ]),
                Line::from(format!(
                    "{heading} · {} · sort {}{query}",
                    self.scope_label(),
                    self.sort.label()
                )),
            ]),
            zones[0],
        );
        if let Some(logs) = &mut self.logs {
            let height = zones[1].height.saturating_sub(2);
            let max_scroll = logs
                .content
                .lines()
                .count()
                .saturating_sub(height as usize)
                .min(u16::MAX as usize) as u16;
            if logs.follow {
                logs.scroll = max_scroll;
            } else {
                logs.scroll = logs.scroll.min(max_scroll);
            }
            let title = format!(
                "Logs · {} · {}{}",
                logs.service_id,
                if logs.follow { "following" } else { "paused" },
                if logs.loading {
                    " · refreshing"
                } else if logs.error.is_some() {
                    " · refresh failed"
                } else {
                    ""
                }
            );
            f.render_widget(
                Paragraph::new(logs.content.as_str())
                    .scroll((logs.scroll, 0))
                    .block(Block::bordered().title(title)),
                zones[1],
            );
        } else if self.view == View::Host {
            self.host.draw(f, zones[1], &self.state);
        } else {
            self.draw_services(f, zones[1]);
        }
        if detail_height > 0 {
            let detail = if let Some(service) = self.selected() {
                let limit = if service["effective_limits"].is_object() {
                    &service["effective_limits"]
                } else {
                    &service["limits"]
                };
                let mut lines = vec![
                    format!(
                        "{} · {} · {}",
                        text(service, "name"),
                        text(service, "id"),
                        text(service, "runtime")
                    ),
                    service["description"].as_str().unwrap_or("").to_string(),
                ];
                if area.height >= 24 {
                    lines.push(format!(
                        "Target: {} · {}",
                        text(&service["execution"], "label"),
                        text(&service["execution"], "target")
                    ));
                    lines.push(if let Some(error) = service["telemetry_error"].as_str() {
                        format!("Metrics unavailable: {error}")
                    } else {
                        format!(
                            "Health: {} · limits {} CPU / {} MiB{}",
                            text(service, "health"),
                            limit["cpus"],
                            limit["memory_mb"],
                            if service["observe_only"] == true {
                                " · observed"
                            } else {
                                ""
                            }
                        )
                    });
                }
                lines.join("\n")
            } else {
                String::new()
            };
            f.render_widget(
                Paragraph::new(detail)
                    .wrap(Wrap { trim: true })
                    .style(Style::default().fg(MUTED)),
                zones[2],
            );
        }
        let status = self
            .logs
            .as_ref()
            .and_then(|logs| logs.error.as_ref())
            .map(|error| format!("Log refresh failed: {error}"))
            .unwrap_or_else(|| self.message.clone());
        f.render_widget(
            Paragraph::new(status)
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(MUTED)),
            zones[3],
        );
        let footer = if self.busy {
            "Working… · Ctrl+C quits after submitted work finishes"
        } else if self.logs.is_some() {
            "↑↓/PgUp/PgDn Scroll · f/End Follow · F5 Refresh\nl/Esc Services · ? Help · q Quit"
        } else if self.view == View::Host {
            "↑↓/PgUp/PgDn Scroll · F5 Refresh · m Machine\nh/Esc Services · ? Help · q Quit"
        } else if !self.ready {
            "F5 Retry selected machine · m Choose machine\n? Help · q Quit"
        } else if area.width >= 120 {
            "↑↓ Select · w Workspace · n New · m Machine · / Search · o Sort · h Host\nd Deploy · l Logs · s Start · x Stop · r Restart · u Run · e Limits · b Budget · a Auto · F5 Refresh · ? Help · q Quit"
        } else if area.width >= 100 {
            "↑↓ Select · w Workspace · n New · m Machine · / Search · o Sort\nd Deploy · l Logs · s/x/r/u Control · h Host · F5 Refresh · ? Help · q Quit"
        } else if area.width >= 80 {
            "↑↓ Select · w Workspace · n New · m Machine · / Search\nd Deploy · l Logs · o Sort · h Host · F5 Refresh · ? Help · q Quit"
        } else {
            "↑↓ Select · w Workspace · m Machine · / Search · o Sort\nn New · d Deploy · l Logs · h Host · F5 · ? Help · q Quit"
        };
        f.render_widget(Paragraph::new(footer).wrap(Wrap { trim: true }), zones[4]);
        if self.modal.is_some() {
            self.draw_modal(f, area);
        }
    }
    fn draw_services(&mut self, f: &mut Frame, area: Rect) {
        let services = self.visible();
        if services.is_empty() {
            let content = if !self.ready {
                format!(
                    "{}\n\nF5 retries the selected machine. m chooses another machine.\nNo services can be changed until a fresh snapshot succeeds.",
                    self.message
                )
            } else if !self.query.is_empty() {
                format!(
                    "No services match /{} in {}.\n/ edits the search. Ctrl+U clears it. w changes workspace.",
                    self.query,
                    self.scope_label()
                )
            } else {
                format!(
                    "No services in {}.\nd deploys a service here. n creates a workspace. w browses workspaces.",
                    self.scope_label()
                )
            };
            f.render_widget(
                Paragraph::new(content)
                    .wrap(Wrap { trim: true })
                    .block(Block::default().borders(Borders::TOP)),
                area,
            );
            return;
        }
        let mut widths = vec![Constraint::Min(18), Constraint::Length(12)];
        let mut headers = vec!["Service", "State"];
        if area.width >= 80 {
            widths.push(Constraint::Length(8));
            headers.push("Runtime");
        }
        widths.extend([Constraint::Length(8), Constraint::Length(8)]);
        headers.extend(["CPU", "MiB"]);
        if area.width >= 120 {
            widths.push(Constraint::Length(17));
            headers.push("CPU / MiB cap");
        }
        let rows: Vec<_> = services
            .into_iter()
            .map(|s| {
                let mut cells = vec![Cell::from(text(s, "name")), Cell::from(text(s, "status"))];
                if area.width >= 80 {
                    cells.push(Cell::from(text(s, "runtime")));
                }
                cells.extend([
                    Cell::from(metric(s, "cpu_percent", 1, "%")),
                    Cell::from(metric(s, "memory_mb", 0, "")),
                ]);
                if area.width >= 120 {
                    let cap = if s["effective_limits"].is_object() {
                        &s["effective_limits"]
                    } else {
                        &s["limits"]
                    };
                    cells.push(Cell::from(
                        cap["cpus"]
                            .as_f64()
                            .map(|cpu| format!("{cpu:.2} / {}", cap["memory_mb"]))
                            .unwrap_or_else(|| "Observed".into()),
                    ));
                }
                Row::new(cells)
            })
            .collect();
        let table = Table::new(rows, widths)
            .header(Row::new(headers).style(Style::default().add_modifier(Modifier::BOLD)))
            .row_highlight_style(Style::default().bg(SELECTED))
            .highlight_symbol("› ")
            .block(Block::default().borders(Borders::TOP | Borders::BOTTOM));
        f.render_stateful_widget(table, area, &mut self.selection);
    }
    fn draw_modal(&self, f: &mut Frame, area: Rect) {
        let popup = center(area, 94, 88);
        f.render_widget(Clear, popup);
        f.render_widget(
            Block::default().style(Style::default().bg(BACKGROUND).fg(FOREGROUND)),
            popup,
        );
        let target = self.target_label();
        let scope = self.scope_label();
        let modal = self.modal.as_ref().unwrap();
        let (mut title, content, scroll): (String, Text<'_>, u16) = match modal {
            Modal::Confirm { message, .. } => (
                "Confirm action".into(),
                format!("{message}\n\nEnter confirms · Esc cancels").into(), 0,
            ),
            Modal::Search { .. } => (
                "Search services".into(),
                format!("Search name, ID or description\n\n/{}▏\n\nEnter keeps · Esc restores · Ctrl+U clears", self.query).into(), 0,
            ),
            Modal::DeploymentFile { path, error } => (
                "Deploy service".into(),
                format!("On {target} · workspace {scope}\nLocal YAML or JSON file:\n{path}▏\n{}\n{}\ninit deploy --example prints a template.", error.as_deref().unwrap_or("Review before anything is created."), if self.busy { "Reading deployment…" } else { "Enter previews · Esc cancels · Ctrl+U clears" }).into(), 0,
            ),
            Modal::Deployment { preview, scroll, .. } => (
                "Deployment · ↑↓ scroll · Enter applies · Esc cancels".into(),
                deployment_review(preview).into(), *scroll,
            ),
            Modal::Resource { id, cpus, memory, field } => (
                id.clone().unwrap_or_else(|| "Controller budget · all workspaces".into()),
                format!("On {target}\n\n{}CPU       {cpus:.2} cores\n{}Memory    {memory} MiB\n\nTab selects · ←/→ or -/+ adjusts\nEnter applies · Esc cancels", if *field == 0 { "› " } else { "  " }, if *field == 1 { "› " } else { "  " }).into(), 0,
            ),
            Modal::Allocation { enabled, preview, scroll } => {
                let mut lines = vec![format!("{} automatic allocation on {target}", if *enabled { "Enable" } else { "Disable" }), "This affects all workspaces on this controller.".into(), String::new()];
                for row in preview["allocations"].as_array().into_iter().flatten() {
                    lines.push(format!("{}: {} CPU / {} MiB", text(row, "id"), row["cpus"], row["memory_mb"]));
                }
                for warning in preview["warnings"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    lines.push(format!("Warning: {warning}"));
                }
                ("Allocation · ↑↓ scroll · Enter applies · Esc cancels".into(), lines.join("\n").into(), *scroll)
            }
            Modal::NewWorkspace { fields, field, error } => {
                let mut lines = vec![Line::from(format!("Create on {target}"))];
                for (index, label) in ["ID", "Name", "Description"].into_iter().enumerate() {
                    let selected = index == *field;
                    lines.push(Line::styled(format!("{} {label}: {}{}", if selected { "›" } else { " " }, fields[index], if selected { "▏" } else { "" }), if selected { Style::default().add_modifier(Modifier::BOLD) } else { Style::default() }));
                }
                lines.push(Line::from(error.as_deref().unwrap_or("Name defaults to ID. Choose a machine with m before opening.")));
                lines.push(Line::from(if self.busy { "Creating… Please wait." } else { "Tab/↑↓ field · Enter creates · Esc cancels · Ctrl+U clears" }));
                ("New workspace".into(), lines.into(), 0)
            }
            Modal::Machines { items, selection, loading, error } => {
                let mut lines = vec![Line::from(format!("Current: {target}")), Line::from("Enter selects · ↑↓ moves · Esc cancels")];
                if *loading { lines.push(Line::from("Loading saved connections…")); }
                if let Some(error) = error { lines.push(Line::from(error.as_str())); }
                let mut options = vec!["This computer · local controller".into()];
                options.extend(items.iter().map(|machine| format!("{} · {}{}", machine.profile.name, machine.profile.host, if machine.identity.is_some() { "" } else { " · test first" })));
                let offset = selection.saturating_sub(popup.height.saturating_sub(lines.len() as u16 + 3) as usize);
                for (index, item) in options.into_iter().enumerate().skip(offset) {
                    lines.push(Line::styled(format!("{} {item}", if index == *selection { "›" } else { " " }), if index == *selection { Style::default().bg(SELECTED) } else { Style::default() }));
                }
                ("Choose machine · saved tested SSH connections".into(), lines.into(), 0)
            }
            Modal::Workspaces { selection } => {
                let entries = self.workspaces();
                let mut lines = vec![Line::from(format!("On {target} · Enter selects · ↑↓ moves · Esc cancels"))];
                let offset = selection.saturating_sub(popup.height.saturating_sub(5) as usize);
                for (index, (scope, label)) in entries.iter().enumerate().skip(offset) {
                    let count = self.state["services"].as_array().into_iter().flatten().filter(|service| match scope {
                        Scope::All => true, Scope::Default => service["project"].as_str().is_none_or(|id| id.is_empty()), Scope::Project(id) => service["project"].as_str() == Some(id),
                    }).count();
                    lines.push(Line::styled(format!("{} {label} ({count})", if index == *selection { "›" } else { " " }), if index == *selection { Style::default().bg(SELECTED) } else { Style::default() }));
                }
                ("Choose workspace".into(), lines.into(), 0)
            }
            Modal::Help { scroll } => {
                let content = if self.logs.is_some() {
                    "LOGS\n↑↓ / j k    Scroll one line\nPgUp/PgDn    Scroll ten lines\nHome         First line, pause following\nf / End      Follow newest lines\nF5           Refresh now\nl / Esc      Return to services\n\nLogs refresh every 5s. Manual scrolling is preserved.\n? / Esc closes help. q quits. Ctrl+C quits safely."
                } else if self.view == View::Host {
                    "HOST ACTIVITY\nHost measurements describe the selected controller host.\nContainer engines may run on a different machine or VM.\n\n↑↓ / j k      Scroll\nPgUp/PgDn     Scroll ten lines\nF5            Refresh now\nm             Choose machine\nh / Esc       Return to services\n\nMissing measurements remain unknown. History is session-only.\n? / Esc closes help. q quits. Ctrl+C quits safely."
                } else {
                    "SERVICES\nw            Choose workspace, including empty groups\nn            Create workspace on the selected machine\nm            Choose local or saved tested SSH machine\n/            Search name, ID and description\no            Sort state → name → CPU → memory\n↑↓ / j k     Select service\nl / Enter    Open automatically refreshed logs\ns / x / r    Start / stop / restart (confirmation)\nu            Run a job (confirmation)\ne            Edit selected service resource limits\nb            Edit controller budget, across workspaces\na            Preview automatic allocation for this controller\nd            Deploy local YAML/JSON, review before apply\nh            Host CPU, memory and network activity\nF5           Refresh now (also every 5s)\n\nTab changes input fields. Ctrl+U clears the active field.\nq is normal text in all inputs. Ctrl+C quits safely.\n? / Esc closes help. ↑↓ / PgUp/PgDn scrolls."
                };
                let content = if self.view == View::Services && self.logs.is_none() {
                    self.selected().map(|service| format!("Selected: {} ({})\n{}\nOn {target} · workspace {scope}\n\n{content}", text(service, "name"), text(service, "id"), service["description"].as_str().unwrap_or(""))).unwrap_or_else(|| content.to_string())
                } else { content.to_string() };
                ("Init help · ↑↓ scroll · Esc closes".into(), content.into(), *scroll)
            }
        };
        if !self.ready
            && !matches!(
                modal,
                Modal::Machines { .. }
                    | Modal::Help { .. }
                    | Modal::Workspaces { .. }
                    | Modal::Search { .. }
            )
        {
            title = "Connection unavailable · F5 retries · Esc cancels".into();
        }
        f.render_widget(
            Paragraph::new(content)
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0))
                .block(Block::bordered().title(title)),
            popup,
        );
    }
}

fn edit(value: &mut String, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Backspace => value.pop().is_some(),
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let changed = !value.is_empty();
            value.clear();
            changed
        }
        KeyCode::Char(c)
            if !c.is_control()
                && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            value.push(c);
            true
        }
        _ => false,
    }
}

fn scroll_key(scroll: &mut u16, key: KeyEvent) {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => *scroll = scroll.saturating_add(1),
        KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
        KeyCode::PageDown => *scroll = scroll.saturating_add(10),
        KeyCode::Home => *scroll = 0,
        _ => {}
    }
}
fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or("—").into()
}
fn metric(value: &Value, key: &str, precision: usize, suffix: &str) -> String {
    value[key]
        .as_f64()
        .filter(|n| n.is_finite())
        .map(|n| format!("{n:.precision$}{suffix}"))
        .unwrap_or_else(|| "—".into())
}
fn metric_order(a: &Value, b: &Value, key: &str) -> Ordering {
    match (
        a[key].as_f64().filter(|n| n.is_finite()),
        b[key].as_f64().filter(|n| n.is_finite()),
    ) {
        (Some(a), Some(b)) => b.total_cmp(&a),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        _ => Ordering::Equal,
    }
}
fn state_rank(service: &Value) -> u8 {
    match service["status"].as_str() {
        Some("running" | "active") => 0,
        Some("restarting") => 1,
        Some("paused") => 2,
        Some("exited" | "stopped" | "inactive" | "completed") => 3,
        Some("created" | "not created") => 4,
        Some("unavailable") => 5,
        _ => 6,
    }
}
fn center(area: Rect, width: u16, height: u16) -> Rect {
    let w = (area.width.saturating_mul(width) / 100)
        .max(1)
        .min(area.width);
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
fn deployment_review(preview: &Value) -> String {
    let service = &preview["service"];
    let mut lines = vec![
        format!(
            "Service: {} ({})",
            service["name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .unwrap_or(service["id"].as_str().unwrap_or("—")),
            text(service, "id")
        ),
        format!("Image: {}", text(service, "image")),
        format!(
            "Project: {}",
            preview["project"]["name"].as_str().unwrap_or("Ungrouped")
        ),
        format!(
            "Runs on: {} · {}",
            text(&preview["execution"], "label"),
            text(&preview["execution"], "target")
        ),
        format!("Action: {}", text(preview, "action")),
        format!(
            "Effective limits: {} CPU / {} MiB",
            preview["effective_limits"]["cpus"], preview["effective_limits"]["memory_mb"]
        ),
        format!(
            "Private workspace: {}",
            if preview["workspace"]["enabled"] == true {
                format!(
                    "{} at /workspace · persistent{}",
                    text(&preview["workspace"], "volume"),
                    if preview["workspace"]["reused"] == true {
                        " · reuses retained data"
                    } else {
                        ""
                    }
                )
            } else {
                "None".into()
            }
        ),
        format!("Command: {}", service["command"]),
    ];
    for warning in preview["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        lines.push(format!("Warning: {warning}"));
    }
    lines.push("\nFull normalized plan (including all allocation changes):".into());
    lines.push(serde_json::to_string_pretty(preview).unwrap_or_else(|error| error.to_string()));
    lines.join("\n")
}

pub async fn run(controller: Controller) -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "The TUI needs an interactive terminal. Use init status --json in scripts."
    );
    let mut terminal = ratatui::init();
    let _restore = Restore;
    let backend = Arc::new(Mutex::new(Backend::new(controller)));
    let (tx, mut updates) = mpsc::unbounded_channel::<(u64, Update)>();
    let poll_backend = backend.clone();
    let poll_tx = tx.clone();
    let refresher = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let mut backend = poll_backend.lock().await;
            let generation = backend.generation();
            let snapshot = backend.snapshot().await;
            if poll_tx
                .send((generation, Update::Snapshot(snapshot)))
                .is_err()
            {
                break;
            }
        }
    });
    let mut app = App::new();
    let mut dirty = true;
    let mut pending: Option<tokio::task::JoinHandle<()>> = None;
    let mut next_logs = tokio::time::Instant::now() + Duration::from_secs(5);
    let result = async {
        loop {
            while let Ok((generation, update)) = updates.try_recv() {
                app.update(generation, update);
                dirty = true;
            }
            if dirty {
                terminal.draw(|frame| app.draw(frame))?;
                dirty = false;
            }
            if tokio::time::Instant::now() >= next_logs {
                next_logs = tokio::time::Instant::now() + Duration::from_secs(5);
                if !app.busy
                    && let Some(logs) = app.logs.as_mut().filter(|logs| !logs.loading)
                {
                    logs.loading = true;
                    spawn_operation(
                        backend.clone(),
                        tx.clone(),
                        app.generation,
                        Operation::Logs(logs.service_id.clone()),
                    );
                    dirty = true;
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
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                dirty = true;
                if app.wants_quit(key) {
                    break;
                }
                if let Some(command) = app.key(key) {
                    match command {
                        Command::Execute(operation) => {
                            let read_only_logs = matches!(operation, Operation::Logs(_));
                            if !read_only_logs {
                                app.busy = true;
                                app.message = "Working…".into();
                            }
                            let task = spawn_operation(
                                backend.clone(),
                                tx.clone(),
                                app.generation,
                                operation,
                            );
                            if !read_only_logs {
                                pending = Some(task);
                            }
                        }
                        Command::Switch(target) => {
                            app.switch_target(target.clone());
                            let generation = app.generation;
                            let worker = backend.clone();
                            let worker_tx = tx.clone();
                            pending = Some(tokio::spawn(async move {
                                let mut backend = worker.lock().await;
                                let result = match backend.set_target(target, generation) {
                                    Ok(()) => backend.snapshot().await,
                                    Err(error) => Err(error),
                                };
                                let _ = worker_tx.send((generation, Update::Switched(result)));
                            }));
                        }
                        Command::Refresh => {
                            if !app.refreshing {
                                app.refreshing = true;
                                let worker = backend.clone();
                                let worker_tx = tx.clone();
                                let generation = app.generation;
                                tokio::spawn(async move {
                                    let mut backend = worker.lock().await;
                                    if backend.generation() != generation {
                                        return;
                                    }
                                    let result = backend.snapshot().await;
                                    let _ = worker_tx.send((generation, Update::Snapshot(result)));
                                });
                            }
                        }
                    }
                }
            }
            tokio::task::yield_now().await;
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    refresher.abort();
    ratatui::restore();
    if let Some(pending) = pending {
        pending.await?;
    }
    result
}
fn spawn_operation(
    backend: Arc<Mutex<Backend>>,
    tx: mpsc::UnboundedSender<(u64, Update)>,
    generation: u64,
    operation: Operation,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut backend = backend.lock().await;
        if backend.generation() != generation {
            return;
        }
        let refresh = !matches!(
            operation,
            Operation::Logs(_)
                | Operation::Machines
                | Operation::DeployPreview { .. }
                | Operation::Preview(_)
        );
        let update = backend.execute(operation).await;
        let _ = tx.send((generation, update));
        if refresh {
            let snapshot = backend.snapshot().await;
            let _ = tx.send((generation, Update::Snapshot(snapshot)));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn press(app: &mut App, c: char) -> Option<Command> {
        app.key(key(KeyCode::Char(c)))
    }
    fn populated() -> App {
        let mut app = App::new();
        app.update_snapshot(Ok(json!({"projects":[{"id":"apps","name":"Applications"},{"id":"empty","name":"Empty workspace"}],"services":[
            {"id":"api","name":"API","description":"Public endpoint","project":"apps","status":"running","cpu_percent":5.0,"memory_mb":64.0,"runtime":"docker","capabilities":{"lifecycle":true,"scale":true},"limits":{"cpus":1,"memory_mb":256}},
            {"id":"worker","name":"Worker","description":"Queues","project":null,"status":"stopped","cpu_percent":0.0,"memory_mb":null,"runtime":"docker"},
            {"id":"orphan","name":"Old worker","description":"Unlisted group","project":"retired","status":"unknown","cpu_percent":null,"memory_mb":256.0,"runtime":"systemd"}
        ],"budget":{"cpus":2,"memory_mb":1024}})));
        app
    }
    fn machine() -> Machine {
        serde_json::from_value(json!({"profile":{"id":"remote","name":"Remote server","host":"offline.invalid","ssh_port":null,"init_port":8787},"identity":{"app":"init","version":"0.5.0","workspace":"/remote","config":"/remote/services.json"},"revision":"one"})).unwrap()
    }
    fn screen(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    #[test]
    fn actions_keep_the_selected_service_identity_through_poll_reorder_and_confirmation() {
        let mut app = populated();
        press(&mut app, 'j');
        assert_eq!(app.selected_id.as_deref(), Some("worker"));
        let mut reordered = app.state.clone();
        reordered["services"].as_array_mut().unwrap().reverse();
        app.update_snapshot(Ok(reordered));
        assert_eq!(app.selected().unwrap()["id"], "worker");
        press(&mut app, 's');
        let mut next = app.state.clone();
        next["services"][1]["status"] = json!("running");
        app.update_snapshot(Ok(next));
        match app.key(key(KeyCode::Enter)) {
            Some(Command::Execute(Operation::Service(id, "start"))) => assert_eq!(id, "worker"),
            _ => panic!("must confirm the owned worker ID"),
        }
        app.sort = Sort::Memory;
        app.reconcile();
        assert_eq!(app.selected().unwrap()["id"], "worker");
    }
    #[test]
    fn workspace_scopes_include_empty_default_and_unlisted_groups_and_filter_actions() {
        let mut app = populated();
        let labels = app.workspaces();
        assert_eq!(labels.len(), 5);
        assert!(
            labels
                .iter()
                .any(|(scope, _)| *scope == Scope::Project("empty".into()))
        );
        assert!(
            labels
                .iter()
                .any(|(scope, _)| *scope == Scope::Project("retired".into()))
        );
        app.scope = Scope::Default;
        app.reconcile();
        assert_eq!(app.selected().unwrap()["id"], "worker");
        app.scope = Scope::Project("empty".into());
        app.reconcile();
        assert!(app.selected().is_none());
        assert!(press(&mut app, 's').is_none());
        assert!(app.modal.is_none());
        assert!(screen(&mut app, 80, 24).contains("No services in Empty workspace"));
        press(&mut app, 'd');
        press(&mut app, 'q');
        assert!(!app.wants_quit(key(KeyCode::Char('q'))));
        match app.key(key(KeyCode::Enter)) {
            Some(Command::Execute(Operation::DeployPreview { path, project })) => {
                assert_eq!(path, PathBuf::from("q"));
                assert_eq!(project.as_deref(), Some("empty"));
            }
            _ => panic!("deployment uses selected named group"),
        }
    }
    #[test]
    fn search_matches_description_and_ids_and_unknown_metrics_sort_last() {
        let mut app = populated();
        app.query = "ENDPOINT".into();
        app.reconcile();
        assert_eq!(app.selected().unwrap()["id"], "api");
        app.query = "orphan".into();
        app.reconcile();
        assert_eq!(app.selected().unwrap()["id"], "orphan");
        app.query.clear();
        app.sort = Sort::Cpu;
        app.reconcile();
        assert_eq!(
            app.visible()
                .iter()
                .map(|s| text(s, "id"))
                .collect::<Vec<_>>(),
            ["api", "worker", "orphan"]
        );
        app.sort = Sort::Memory;
        app.reconcile();
        assert_eq!(
            app.visible()
                .iter()
                .map(|s| text(s, "id"))
                .collect::<Vec<_>>(),
            ["orphan", "api", "worker"]
        );
        let snapshot = app.state.clone();
        app.update_snapshot(Ok(snapshot));
        assert_eq!(app.selected().unwrap()["id"], "orphan");
    }
    #[test]
    fn changing_machine_clears_actionable_data_and_ignores_every_late_old_update() {
        let mut app = populated();
        app.scope = Scope::Project("apps".into());
        press(&mut app, 'l');
        let old = app.generation;
        app.switch_target(Target::Remote(machine()));
        assert!(!app.ready);
        assert!(app.selected().is_none());
        assert!(app.logs.is_none());
        assert!(app.scope == Scope::All);
        app.update(
            old,
            Update::Snapshot(Ok(json!({"services":[{"id":"local-secret"}]}))),
        );
        app.update(old, Update::Completed(Ok(None)));
        assert!(app.busy);
        assert!(app.state["services"].as_array().unwrap().is_empty());
        app.update(
            app.generation,
            Update::Switched(Err(anyhow::anyhow!("SSH connection refused"))),
        );
        assert!(!app.busy && !app.ready);
        assert!(app.target_label().contains("Remote server"));
        assert!(screen(&mut app, 100, 24).contains("SSH connection refused"));
        assert!(press(&mut app, 's').is_none());
        assert!(press(&mut app, 'n').is_none());
        assert!(app.modal.is_none());
        assert!(matches!(
            press(&mut app, 'm'),
            Some(Command::Execute(Operation::Machines))
        ));
    }
    #[test]
    fn machine_picker_allows_local_recovery_and_rejects_untested_destinations() {
        let mut app = populated();
        let mut untested = machine();
        untested.identity = None;
        app.modal = Some(Modal::Machines {
            items: vec![untested],
            selection: 1,
            loading: false,
            error: None,
        });
        assert!(app.key(key(KeyCode::Enter)).is_none());
        assert!(matches!(
            &app.modal,
            Some(Modal::Machines { error: Some(_), .. })
        ));
        app.key(key(KeyCode::Up));
        assert!(matches!(
            app.key(key(KeyCode::Enter)),
            Some(Command::Switch(Target::Local))
        ));
    }
    #[test]
    fn workspace_input_survives_rejection_and_q_is_text_in_all_input_fields() {
        let mut app = populated();
        press(&mut app, 'n');
        for c in "queue".chars() {
            assert!(!app.wants_quit(key(KeyCode::Char(c))));
            press(&mut app, c);
        }
        app.key(key(KeyCode::Tab));
        for c in "Queue q".chars() {
            press(&mut app, c);
        }
        app.key(key(KeyCode::Tab));
        for c in "q jobs".chars() {
            press(&mut app, c);
        }
        match app.key(key(KeyCode::Enter)) {
            Some(Command::Execute(Operation::CreateProject(project))) => {
                assert_eq!(project.id, "queue");
                assert_eq!(project.name, "Queue q");
                assert_eq!(project.description, "q jobs");
            }
            _ => panic!("valid workspace submits once"),
        }
        app.busy = true;
        assert!(app.key(key(KeyCode::Enter)).is_none());
        app.update(
            app.generation,
            Update::ProjectCreated {
                project_id: "queue".into(),
                result: Err(anyhow::anyhow!("Workspace already exists")),
            },
        );
        match &app.modal {
            Some(Modal::NewWorkspace {
                fields,
                error: Some(error),
                ..
            }) => {
                assert_eq!(fields.as_slice(), ["queue", "Queue q", "q jobs"]);
                assert!(error.contains("already exists"));
            }
            _ => panic!("failure must retain all fields"),
        }
        assert!(screen(&mut app, 100, 24).contains("Workspace already exists"));
        assert!(!app.wants_quit(key(KeyCode::Char('q'))));
        app.key(key(KeyCode::Esc));
        press(&mut app, '/');
        press(&mut app, 'q');
        assert_eq!(app.query, "q");
        assert!(!app.wants_quit(key(KeyCode::Char('q'))));
        app.key(key(KeyCode::Esc));
        assert_eq!(app.query, "");
        press(&mut app, 'd');
        for c in "配置/queue.yaml".chars() {
            press(&mut app, c);
        }
        assert!(!app.wants_quit(key(KeyCode::Char('q'))));
        match app.key(key(KeyCode::Enter)) {
            Some(Command::Execute(Operation::DeployPreview { path, .. })) => {
                assert_eq!(path, PathBuf::from("配置/queue.yaml"))
            }
            _ => panic!("file submission only previews"),
        }
        app.update(
            app.generation,
            Update::Deployment(Err(anyhow::anyhow!("Invalid YAML"))),
        );
        assert!(
            matches!(&app.modal,Some(Modal::DeploymentFile{path,error:Some(_),..}) if path=="配置/queue.yaml")
        );
        assert!(app.wants_quit(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)));
    }
    #[test]
    fn created_workspace_becomes_the_scope_and_deployment_confirmation_preserves_reviewed_dto() {
        let mut app = populated();
        press(&mut app, 'n');
        for c in "new-group".chars() {
            press(&mut app, c);
        }
        match app.key(key(KeyCode::Enter)) {
            Some(Command::Execute(Operation::CreateProject(project))) => {
                assert_eq!(project.name, "new-group")
            }
            _ => panic!("blank name defaults to ID"),
        }
        app.update(
            app.generation,
            Update::ProjectCreated {
                project_id: "new-group".into(),
                result: Ok(()),
            },
        );
        assert!(app.scope == Scope::Project("new-group".into()));
        assert!(app.modal.is_none());
        let service = Box::new(crate::example_deployment());
        let expected = serde_json::to_value(&service).unwrap();
        app.update(
            app.generation,
            Update::Deployment(Ok((service, json!({"service":expected})))),
        );
        app.key(key(KeyCode::PageDown));
        assert!(matches!(
            app.modal,
            Some(Modal::Deployment { scroll: 10, .. })
        ));
        app.update_snapshot(Ok(json!({"services":[]})));
        match app.key(key(KeyCode::Enter)) {
            Some(Command::Execute(Operation::Deploy(service))) => {
                assert_eq!(serde_json::to_value(service).unwrap(), expected)
            }
            _ => panic!("confirmation must retain owned reviewed DTO"),
        }
    }
    #[test]
    fn log_polls_preserve_scroll_follow_and_closed_views_cannot_reopen() {
        let mut app = populated();
        assert!(
            matches!(press(&mut app,'l'),Some(Command::Execute(Operation::Logs(id))) if id=="api")
        );
        app.update(
            app.generation,
            Update::Logs {
                service_id: "api".into(),
                result: Ok((0..80).map(|n| format!("line {n}\n")).collect()),
            },
        );
        screen(&mut app, 100, 24);
        app.key(key(KeyCode::PageUp));
        let paused = app.logs.as_ref().unwrap().scroll;
        assert!(!app.logs.as_ref().unwrap().follow);
        app.update(
            app.generation,
            Update::Logs {
                service_id: "worker".into(),
                result: Ok("wrong service".into()),
            },
        );
        assert!(!app.logs.as_ref().unwrap().content.contains("wrong"));
        app.update(
            app.generation,
            Update::Logs {
                service_id: "api".into(),
                result: Ok((0..100).map(|n| format!("updated {n}\n")).collect()),
            },
        );
        screen(&mut app, 100, 24);
        assert_eq!(app.logs.as_ref().unwrap().scroll, paused);
        press(&mut app, 'f');
        screen(&mut app, 100, 24);
        assert!(app.logs.as_ref().unwrap().follow);
        assert!(app.logs.as_ref().unwrap().scroll > paused);
        app.key(key(KeyCode::Esc));
        app.update(
            app.generation,
            Update::Logs {
                service_id: "api".into(),
                result: Ok("late".into()),
            },
        );
        assert!(app.logs.is_none());
    }
    #[test]
    fn polling_cannot_release_mutation_guards_and_quitting_is_still_available() {
        let mut app = populated();
        app.busy = true;
        let state = app.state.clone();
        app.update(app.generation, Update::Snapshot(Ok(state)));
        assert!(app.busy);
        assert!(press(&mut app, 'm').is_none());
        assert!(press(&mut app, 's').is_none());
        assert!(press(&mut app, 'n').is_none());
        assert!(app.modal.is_none());
        assert!(app.wants_quit(key(KeyCode::Char('q'))));
        assert!(app.wants_quit(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)));
    }
    #[test]
    fn a_failed_poll_blocks_open_confirmation_until_a_fresh_snapshot_succeeds() {
        let mut app = populated();
        let good = app.state.clone();
        press(&mut app, 's');
        app.update_snapshot(Err(anyhow::anyhow!("Connection failed")));
        assert!(app.key(key(KeyCode::Enter)).is_none());
        assert!(app.modal.is_some());
        assert!(matches!(
            app.key(key(KeyCode::F(5))),
            Some(Command::Refresh)
        ));
        assert!(screen(&mut app, 100, 24).contains("Connection unavailable"));
        app.update_snapshot(Ok(good));
        assert!(
            matches!(app.key(key(KeyCode::Enter)), Some(Command::Execute(Operation::Service(id, "start"))) if id == "api")
        );
    }
    #[test]
    fn compact_layout_keeps_context_and_blocks_invisible_confirmation_on_resize() {
        let mut app = populated();
        for (width, height) in [(130, 30), (100, 24), (80, 24), (65, 15)] {
            let output = screen(&mut app, width, height);
            assert!(output.contains("This computer"));
            assert!(output.contains("All workspaces"));
            assert!(output.contains("Service"));
            assert!(output.contains("q Quit"));
            assert!(!app.too_small);
        }
        press(&mut app, 's');
        let output = screen(&mut app, 40, 10);
        assert!(output.contains("Resize"));
        assert!(app.key(key(KeyCode::Enter)).is_none());
        assert!(app.modal.is_some());
        screen(&mut app, 65, 15);
        assert!(matches!(
            app.key(key(KeyCode::Enter)),
            Some(Command::Execute(Operation::Service(_, _)))
        ));
    }
    #[test]
    fn observed_services_cannot_be_changed_and_configuration_errors_recover() {
        let mut app = populated();
        app.state["services"][0]["observe_only"] = json!(true);
        press(&mut app, 's');
        assert!(app.modal.is_none());
        assert!(app.message.contains("observed"));
        app.update_snapshot(Ok(
            json!({"services":[],"manifest_error":"Invalid YAML at line 3"}),
        ));
        assert!(app.message.contains("Invalid YAML"));
        app.update_snapshot(Ok(json!({"services":[],"manifest_error":null})));
        assert!(app.message.starts_with("Ready"));
    }
    #[test]
    fn deployment_review_keeps_execution_and_storage_before_large_allocation_details() {
        let preview = json!({"service":crate::example_deployment(),"execution":{"label":"Lima VM · workers","target":"owned-worker"},"workspace":{"enabled":true,"volume":"owned-data","reused":true},"warnings":["Existing data is retained"],"allocations":vec![json!({"id":"other","cpus":1});256]});
        let review = deployment_review(&preview);
        assert!(review.find("alpine:3").unwrap() < review.find("Full normalized plan").unwrap());
        assert!(review.contains("Lima VM · workers") && review.contains("reuses retained data"));
        assert!(review.contains("Warning: Existing data is retained"));
    }
}
