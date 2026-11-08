//! Terminal dashboard (ratatui).

use crate::scan::{self, Agent, Scanner, Status};
use crate::{util, xutil};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;
use std::error::Error;
use std::time::{Duration, Instant};

fn hex(s: &str) -> Color {
    let v = util::rgb(s);
    Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

const FG: Color = Color::Rgb(0xE6, 0xE6, 0xE6);
const DIM: Color = Color::Rgb(0x6B, 0x72, 0x80);
const ACCENT: Color = Color::Rgb(0x00, 0xF0, 0xFF);

struct App {
    agents: Vec<Agent>,
    state: TableState,
    selected_pid: Option<u32>,
    msg: Option<(String, Instant)>,
}

impl App {
    fn set_agents(&mut self, agents: Vec<Agent>) {
        self.agents = agents;
        let idx = self
            .selected_pid
            .and_then(|p| self.agents.iter().position(|a| a.pid == p))
            .or(if self.agents.is_empty() { None } else { Some(0) });
        let idx = idx.map(|i| i.min(self.agents.len().saturating_sub(1)));
        self.state.select(idx);
        self.selected_pid = idx.map(|i| self.agents[i].pid);
    }
    fn mv(&mut self, d: i32) {
        if self.agents.is_empty() {
            return;
        }
        let n = self.agents.len() as i32;
        let i = (self.state.selected().unwrap_or(0) as i32 + d).rem_euclid(n) as usize;
        self.state.select(Some(i));
        self.selected_pid = Some(self.agents[i].pid);
    }
    fn selected(&self) -> Option<&Agent> {
        self.state.selected().and_then(|i| self.agents.get(i))
    }
}

pub fn run(popup: bool, interval: Duration) -> Result<(), Box<dyn Error>> {
    if popup {
        let _ = std::fs::write(util::pidfile(), std::process::id().to_string());
    }
    let mut sc = Scanner::new();
    let mut app = App { agents: vec![], state: TableState::default(), selected_pid: None, msg: None };
    sc.scan();
    std::thread::sleep(Duration::from_millis(250));
    app.set_agents(sc.scan());

    let mut term = ratatui::init();
    let mut next = Instant::now() + interval;
    let res = (|| -> Result<(), Box<dyn Error>> {
        loop {
            term.draw(|f| ui(f, &mut app))?;
            let timeout = next.saturating_duration_since(Instant::now());
            if event::poll(timeout)? {
                if let Event::Key(k) = event::read()? {
                    if k.kind != KeyEventKind::Press {
                        continue;
                    }
                    match k.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => break,
                        KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => app.mv(1),
                        KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => app.mv(-1),
                        KeyCode::Home | KeyCode::Char('g') => app.mv(-(app.agents.len() as i32)),
                        KeyCode::Char('r') => next = Instant::now(),
                        KeyCode::Enter | KeyCode::Char('f') => {
                            if let Some(a) = app.selected() {
                                let (pid, name) = (a.pid, a.name.clone());
                                match xutil::focus_pid(pid) {
                                    Ok(true) if popup => break,
                                    Ok(true) => app.msg = Some((format!("focused {name}"), Instant::now())),
                                    _ => {
                                        app.msg = Some((format!("no window found for pid {pid}"), Instant::now()))
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            if Instant::now() >= next {
                app.set_agents(sc.scan());
                next = Instant::now() + interval;
            }
        }
        Ok(())
    })();
    ratatui::restore();
    if popup {
        let _ = std::fs::remove_file(util::pidfile());
    }
    res
}

fn status_span(s: Status) -> Span<'static> {
    let sym = match s {
        Status::Busy => "● busy",
        Status::Waiting => "◆ wait",
        Status::Idle => "○ idle",
    };
    Span::styled(sym, Style::new().fg(hex(s.color())))
}

fn ui(f: &mut Frame, app: &mut App) {
    let [top, mid, bottom, help] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(10),
        Constraint::Length(1),
    ])
    .areas(f.area());

    // header
    let (busy, wait, total) = scan::totals(&app.agents);
    let mut h = vec![
        Span::styled(" 󰚩 agentop ", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::styled(format!(" {busy} busy "), Style::new().fg(hex(Status::Busy.color()))),
        Span::styled(format!(" {wait} waiting "), Style::new().fg(hex(Status::Waiting.color()))),
        Span::styled(format!(" {} idle ", total - busy - wait), Style::new().fg(DIM)),
        Span::raw("  "),
    ];
    for (k, b, _, t) in scan::summary(&app.agents) {
        h.push(Span::styled(format!("{} ", k.label()), Style::new().fg(hex(k.color()))));
        h.push(Span::styled(format!("{b}/{t}  "), Style::new().fg(FG)));
    }
    let mem: u64 = app.agents.iter().map(|a| a.rss_kb).sum();
    let cpu: f32 = app.agents.iter().map(|a| a.cpu).sum();
    h.push(Span::styled(
        format!("Σ {:.0}% cpu {}", cpu, util::fmt_mem(mem)),
        Style::new().fg(DIM),
    ));
    f.render_widget(Paragraph::new(Line::from(h)), top);

    // table
    let rows: Vec<Row> = app
        .agents
        .iter()
        .map(|a| {
            let act = a.activity.clone().unwrap_or_default();
            Row::new(vec![
                Cell::from(Span::styled(a.kind.label(), Style::new().fg(hex(a.kind.color())))),
                Cell::from(Span::styled(a.name.clone(), Style::new().fg(FG).add_modifier(Modifier::BOLD))),
                Cell::from(status_span(a.status)),
                Cell::from(Span::styled(act, Style::new().fg(FG))),
                Cell::from(format!("{:>5.1}%", a.cpu)),
                Cell::from(format!("{:>6}", util::fmt_mem(a.rss_kb))),
                Cell::from(format!(
                    "{:>6}",
                    a.busy_for_s.map(util::fmt_dur).unwrap_or_else(|| util::fmt_dur(a.uptime_s))
                )),
            ])
            .style(Style::new().fg(DIM))
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Length(22),
            Constraint::Length(7),
            Constraint::Fill(1),
            Constraint::Length(6),
            Constraint::Length(6),
            Constraint::Length(6),
        ],
    )
    .header(
        Row::new(["AGENT", "SESSION", "STATE", "DOING", "CPU", "MEM", "TIME"])
            .style(Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
    )
    .row_highlight_style(Style::new().bg(Color::Rgb(0x1E, 0x29, 0x3B)))
    .highlight_symbol("▌")
    .block(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(Color::Rgb(0x33, 0x3D, 0x4D))),
    );
    if app.agents.is_empty() {
        f.render_widget(
            Paragraph::new("  no AI agents running (claude, codex, gemini, agy, opencode, hermes)")
                .style(Style::new().fg(DIM))
                .block(Block::bordered().border_type(BorderType::Rounded)),
            mid,
        );
    } else {
        f.render_stateful_widget(table, mid, &mut app.state);
    }

    detail(f, app, bottom);

    let mut hl = vec![Span::styled(
        " ↑↓/jk select · ⏎/f focus window · r refresh · q quit",
        Style::new().fg(DIM),
    )];
    if let Some((m, t)) = &app.msg {
        if t.elapsed() < Duration::from_secs(4) {
            hl.push(Span::styled(format!("   {m}"), Style::new().fg(ACCENT)));
        }
    }
    f.render_widget(Paragraph::new(Line::from(hl)), help);
}

fn detail(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Rgb(0x33, 0x3D, 0x4D)));
    let Some(a) = app.selected() else {
        f.render_widget(block, area);
        return;
    };
    let k = |s: &str| Span::styled(format!("{s:<9}"), Style::new().fg(DIM));
    let v = |s: String| Span::styled(s, Style::new().fg(FG));
    let mut lines = vec![
        Line::from(vec![
            k("agent"),
            Span::styled(a.kind.label(), Style::new().fg(hex(a.kind.color()))),
            v(format!("  pid {}  ·  {} procs  ·  up {}", a.pid, a.procs, util::fmt_dur(a.uptime_s))),
        ]),
        Line::from(vec![k("dir"), v(util::tilde(&a.cwd))]),
    ];
    if let Some(s) = &a.session_id {
        lines.push(Line::from(vec![k("session"), v(s.clone())]));
    }
    if let Some(act) = &a.activity {
        let age = a.activity_age_s.map(|s| format!("  ({} ago)", util::fmt_dur(s))).unwrap_or_default();
        lines.push(Line::from(vec![k("last"), v(act.clone()), Span::styled(age, Style::new().fg(DIM))]));
    }
    if a.running.is_empty() {
        lines.push(Line::from(vec![k("running"), Span::styled("—", Style::new().fg(DIM))]));
    }
    for (i, c) in a.running.iter().enumerate() {
        lines.push(Line::from(vec![
            k(if i == 0 { "running" } else { "" }),
            Span::styled("$ ", Style::new().fg(hex(Status::Busy.color()))),
            v(c.clone()),
        ]));
    }
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(block.title(Span::styled(
            format!(" {} ", a.name),
            Style::new().fg(FG).add_modifier(Modifier::BOLD),
        ))),
        area,
    );
}
