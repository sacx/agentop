//! Terminal dashboard (ratatui): agents, a top-style process list, or both.

use crate::scan::{self, Agent, ProcInfo, Scanner, Status, SysStat};
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
const BORDER: Color = Color::Rgb(0x33, 0x3D, 0x4D);
const SEL_BG: Color = Color::Rgb(0x1E, 0x29, 0x3B);
const TREE_BG: Color = Color::Rgb(0x14, 0x1B, 0x26);
const WARN: Color = Color::Rgb(0xFB, 0xBF, 0x24);
const HOT: Color = Color::Rgb(0xF8, 0x71, 0x71);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Split,
    Agents,
    Procs,
}

impl View {
    pub fn parse(s: &str) -> Option<View> {
        match s {
            "split" | "overview" | "both" => Some(View::Split),
            "agents" => Some(View::Agents),
            "procs" | "processes" | "top" => Some(View::Procs),
            _ => None,
        }
    }
    fn next(self, d: i32) -> View {
        const ALL: [View; 3] = [View::Split, View::Agents, View::Procs];
        let i = ALL.iter().position(|v| *v == self).unwrap_or(0) as i32;
        ALL[(i + d).rem_euclid(3) as usize]
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pane {
    Agents,
    Procs,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Cpu,
    Mem,
    Time,
    Pid,
    Name,
}

impl Sort {
    fn label(self) -> &'static str {
        match self {
            Sort::Cpu => "cpu",
            Sort::Mem => "mem",
            Sort::Time => "time",
            Sort::Pid => "pid",
            Sort::Name => "name",
        }
    }
}

struct App {
    view: View,
    focus: Pane,
    popup: bool,
    agents: Vec<Agent>,
    astate: TableState,
    apid: Option<u32>,
    procs: Vec<ProcInfo>,
    /// indices into `procs`, filtered and sorted
    visible: Vec<usize>,
    pstate: TableState,
    ppid: Option<u32>,
    sys: SysStat,
    sort: Sort,
    desc: bool,
    only_agents: bool,
    filter: String,
    typing: bool,
    confirm_kill: Option<(u32, String)>,
    page: usize,
    msg: Option<(String, Instant)>,
}

impl App {
    fn pane(&self) -> Pane {
        match self.view {
            View::Agents => Pane::Agents,
            View::Procs => Pane::Procs,
            View::Split => self.focus,
        }
    }

    fn say(&mut self, m: String) {
        self.msg = Some((m, Instant::now()));
    }

    fn set_agents(&mut self, agents: Vec<Agent>) {
        self.agents = agents;
        let idx = self
            .apid
            .and_then(|p| self.agents.iter().position(|a| a.pid == p))
            .or(if self.agents.is_empty() { None } else { Some(0) });
        self.astate.select(idx);
        self.apid = idx.map(|i| self.agents[i].pid);
    }

    fn set_procs(&mut self, procs: Vec<ProcInfo>, sys: SysStat) {
        self.procs = procs;
        self.sys = sys;
        self.refilter();
    }

    fn refilter(&mut self) {
        let f = self.filter.to_lowercase();
        let p = &self.procs;
        let mut v: Vec<usize> = (0..p.len())
            .filter(|&i| !self.only_agents || p[i].agent.is_some())
            .filter(|&i| {
                f.is_empty() || {
                    let q = &p[i];
                    q.pid.to_string() == f
                        || q.user.to_lowercase().contains(&f)
                        || q.comm.to_lowercase().contains(&f)
                        || q.argv.iter().any(|a| a.to_lowercase().contains(&f))
                        || q.agent.is_some_and(|(k, _)| k.label() == f)
                }
            })
            .collect();
        let key = |a: &ProcInfo, b: &ProcInfo| match self.sort {
            Sort::Cpu => a.cpu.total_cmp(&b.cpu).then(a.cpu_time_s.total_cmp(&b.cpu_time_s)),
            Sort::Mem => a.rss_kb.cmp(&b.rss_kb),
            Sort::Time => a.cpu_time_s.total_cmp(&b.cpu_time_s),
            Sort::Pid => a.pid.cmp(&b.pid),
            Sort::Name => a.comm.to_lowercase().cmp(&b.comm.to_lowercase()),
        };
        v.sort_by(|&a, &b| {
            let o = key(&p[a], &p[b]).then(p[a].pid.cmp(&p[b].pid));
            if self.desc { o.reverse() } else { o }
        });
        self.visible = v;
        let idx = self
            .ppid
            .and_then(|pid| self.visible.iter().position(|&i| self.procs[i].pid == pid))
            .or(if self.visible.is_empty() { None } else { Some(0) });
        self.pstate.select(idx);
        self.ppid = idx.map(|i| self.procs[self.visible[i]].pid);
    }

    fn set_sort(&mut self, s: Sort) {
        if self.sort == s {
            self.desc = !self.desc;
        } else {
            self.sort = s;
            self.desc = matches!(s, Sort::Cpu | Sort::Mem | Sort::Time);
        }
        self.refilter();
    }

    fn mv(&mut self, d: i32) {
        let (n, st) = match self.pane() {
            Pane::Agents => (self.agents.len(), &mut self.astate),
            Pane::Procs => (self.visible.len(), &mut self.pstate),
        };
        if n == 0 {
            return;
        }
        let i = (st.selected().unwrap_or(0) as i32 + d).clamp(0, n as i32 - 1) as usize;
        st.select(Some(i));
        match self.pane() {
            Pane::Agents => self.apid = Some(self.agents[i].pid),
            Pane::Procs => self.ppid = Some(self.procs[self.visible[i]].pid),
        }
    }

    fn selected_agent(&self) -> Option<&Agent> {
        self.astate.selected().and_then(|i| self.agents.get(i))
    }

    fn selected_proc(&self) -> Option<&ProcInfo> {
        self.pstate.selected().and_then(|i| self.visible.get(i)).map(|&i| &self.procs[i])
    }

    /// Returns true when the app should exit.
    fn key(&mut self, code: KeyCode, mods: KeyModifiers, next: &mut Instant) -> bool {
        if code == KeyCode::Char('c') && mods.contains(KeyModifiers::CONTROL) {
            return true;
        }
        if self.typing {
            match code {
                KeyCode::Enter => self.typing = false,
                KeyCode::Esc => {
                    self.typing = false;
                    self.filter.clear();
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) => self.filter.push(c),
                _ => return false,
            }
            self.refilter();
            return false;
        }
        if let Some((pid, name)) = self.confirm_kill.take() {
            if matches!(code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                let r = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
                self.say(if r == 0 {
                    format!("sent SIGTERM to {pid} ({name})")
                } else {
                    format!("kill {pid}: {}", std::io::Error::last_os_error())
                });
                *next = Instant::now() + Duration::from_millis(300);
            } else {
                self.say("kill cancelled".into());
            }
            return false;
        }
        let procs_shown = self.view != View::Agents;
        match code {
            KeyCode::Char('q') => return true,
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.refilter();
            }
            KeyCode::Esc => return true,
            KeyCode::Char('1') => self.view = View::Split,
            KeyCode::Char('2') => self.view = View::Agents,
            KeyCode::Char('3') => self.view = View::Procs,
            KeyCode::Tab => self.view = self.view.next(1),
            KeyCode::BackTab => self.view = self.view.next(-1),
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l')
                if self.view == View::Split =>
            {
                self.focus = if self.focus == Pane::Agents { Pane::Procs } else { Pane::Agents };
            }
            KeyCode::Down | KeyCode::Char('j') => self.mv(1),
            KeyCode::Up | KeyCode::Char('k') => self.mv(-1),
            KeyCode::PageDown => self.mv(self.page as i32),
            KeyCode::PageUp => self.mv(-(self.page as i32)),
            KeyCode::Home | KeyCode::Char('g') => self.mv(i32::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.mv(i32::MAX / 2),
            KeyCode::Char('r') => *next = Instant::now(),
            KeyCode::Char('c') if procs_shown => self.set_sort(Sort::Cpu),
            KeyCode::Char('m') if procs_shown => self.set_sort(Sort::Mem),
            KeyCode::Char('t') if procs_shown => self.set_sort(Sort::Time),
            KeyCode::Char('p') if procs_shown => self.set_sort(Sort::Pid),
            KeyCode::Char('n') if procs_shown => self.set_sort(Sort::Name),
            KeyCode::Char('a') if procs_shown => {
                self.only_agents = !self.only_agents;
                self.refilter();
            }
            KeyCode::Char('/') if procs_shown => {
                self.typing = true;
                if self.view == View::Split {
                    self.focus = Pane::Procs;
                }
            }
            KeyCode::Char('K') | KeyCode::Delete if self.pane() == Pane::Procs => {
                if let Some(p) = self.selected_proc() {
                    self.confirm_kill = Some((p.pid, p.comm.clone()));
                }
            }
            KeyCode::Enter | KeyCode::Char('f') => {
                let target = match self.pane() {
                    Pane::Agents => self.selected_agent().map(|a| (a.pid, a.name.clone())),
                    Pane::Procs => self.selected_proc().map(|p| (p.pid, p.comm.clone())),
                };
                if let Some((pid, name)) = target {
                    match xutil::focus_pid(pid) {
                        Ok(true) if self.popup => return true,
                        Ok(true) => self.say(format!("focused {name}")),
                        _ => self.say(format!("no window found for pid {pid}")),
                    }
                }
            }
            _ => {}
        }
        false
    }
}

pub fn run(popup: bool, interval: Duration, view: View) -> Result<(), Box<dyn Error>> {
    if popup {
        let _ = std::fs::write(util::pidfile(), std::process::id().to_string());
    }
    let mut sc = Scanner::new().with_procs();
    let mut app = App {
        view,
        focus: if view == View::Procs { Pane::Procs } else { Pane::Agents },
        popup,
        agents: vec![],
        astate: TableState::default(),
        apid: None,
        procs: vec![],
        visible: vec![],
        pstate: TableState::default(),
        ppid: None,
        sys: SysStat::default(),
        sort: Sort::Cpu,
        desc: true,
        only_agents: false,
        filter: String::new(),
        typing: false,
        confirm_kill: None,
        page: 10,
        msg: None,
    };
    sc.scan();
    std::thread::sleep(Duration::from_millis(250));
    app.set_agents(sc.scan());
    app.set_procs(sc.take_procs(), sc.sys().clone());

    let mut term = ratatui::init();
    let mut next = Instant::now() + interval;
    let res = (|| -> Result<(), Box<dyn Error>> {
        loop {
            term.draw(|f| ui(f, &mut app))?;
            let timeout = next.saturating_duration_since(Instant::now());
            if event::poll(timeout)? {
                if let Event::Key(k) = event::read()? {
                    if k.kind == KeyEventKind::Press && app.key(k.code, k.modifiers, &mut next) {
                        break;
                    }
                }
            }
            if Instant::now() >= next {
                app.set_agents(sc.scan());
                app.set_procs(sc.take_procs(), sc.sys().clone());
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

fn pane_block(focused: bool) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if focused { ACCENT } else { BORDER }))
}

fn ui(f: &mut Frame, app: &mut App) {
    let [top, body, help] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(5), Constraint::Length(1)]).areas(f.area());

    header(f, app, top);
    match app.view {
        View::Agents => {
            let [mid, bottom] = Layout::vertical([Constraint::Min(5), Constraint::Length(10)]).areas(body);
            agents_table(f, app, mid, false);
            detail(f, app, bottom);
        }
        View::Procs => procs_table(f, app, body, false),
        View::Split => {
            let want = app.agents.len().max(1) as u16 + 3;
            let h = want.clamp(5, (body.height * 2 / 5).max(5));
            let [a, p] = Layout::vertical([Constraint::Length(h), Constraint::Min(5)]).areas(body);
            agents_table(f, app, a, app.focus == Pane::Agents);
            procs_table(f, app, p, app.focus == Pane::Procs);
        }
    }
    help_line(f, app, help);
}

fn header(f: &mut Frame, app: &App, area: Rect) {
    let mut h = vec![Span::styled(" 󰚩 agentop ", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))];
    for (v, n, t) in [(View::Split, '1', "Overview"), (View::Agents, '2', "Agents"), (View::Procs, '3', "Processes")] {
        let st = if app.view == v {
            Style::new().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(DIM)
        };
        h.push(Span::styled(format!(" {n} {t} "), st));
        h.push(Span::raw(" "));
    }
    let (busy, wait, total) = scan::totals(&app.agents);
    h.push(Span::raw(" "));
    h.push(Span::styled(format!("{busy} busy "), Style::new().fg(hex(Status::Busy.color()))));
    h.push(Span::styled(format!("{wait} waiting "), Style::new().fg(hex(Status::Waiting.color()))));
    h.push(Span::styled(format!("{} idle  ", total - busy - wait), Style::new().fg(DIM)));
    for (k, b, _, t) in scan::summary(&app.agents) {
        h.push(Span::styled(format!("{} ", k.label()), Style::new().fg(hex(k.color()))));
        h.push(Span::styled(format!("{b}/{t}  "), Style::new().fg(FG)));
    }
    let mem: u64 = app.agents.iter().map(|a| a.rss_kb).sum();
    let cpu: f32 = app.agents.iter().map(|a| a.cpu).sum();
    h.push(Span::styled(format!("Σ {:.0}% cpu {}", cpu, util::fmt_mem(mem)), Style::new().fg(DIM)));
    f.render_widget(Paragraph::new(Line::from(h)), area);
}

fn agents_table(f: &mut Frame, app: &mut App, area: Rect, focused: bool) {
    let block = pane_block(focused).title(Span::styled(" agents ", Style::new().fg(FG)));
    if app.agents.is_empty() {
        f.render_widget(
            Paragraph::new("  no AI agents running (claude, codex, gemini, agy, opencode, hermes)")
                .style(Style::new().fg(DIM))
                .block(block),
            area,
        );
        return;
    }
    let rows: Vec<Row> = app
        .agents
        .iter()
        .map(|a| {
            Row::new(vec![
                Cell::from(Span::styled(a.kind.label(), Style::new().fg(hex(a.kind.color())))),
                Cell::from(Span::styled(a.name.clone(), Style::new().fg(FG).add_modifier(Modifier::BOLD))),
                Cell::from(status_span(a.status)),
                Cell::from(Span::styled(a.activity.clone().unwrap_or_default(), Style::new().fg(FG))),
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
    .row_highlight_style(Style::new().bg(SEL_BG))
    .highlight_symbol("▌")
    .block(block);
    if app.pane() == Pane::Agents {
        app.page = area.height.saturating_sub(4).max(1) as usize;
    }
    f.render_stateful_widget(table, area, &mut app.astate);
}

fn fmt_cputime(s: f64) -> String {
    let s = s as u64;
    if s < 3600 {
        format!("{}:{:02}", s / 60, s % 60)
    } else if s < 100 * 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d{:02}h", s / 86400, (s % 86400) / 3600)
    }
}

fn meter(pct: f32, w: usize) -> Vec<Span<'static>> {
    let n = ((pct / 100.0) * w as f32).round().clamp(0.0, w as f32) as usize;
    let c = if pct >= 90.0 {
        HOT
    } else if pct >= 60.0 {
        WARN
    } else {
        hex(Status::Busy.color())
    };
    vec![
        Span::styled("▕", Style::new().fg(BORDER)),
        Span::styled("█".repeat(n), Style::new().fg(c)),
        Span::styled("░".repeat(w - n), Style::new().fg(BORDER)),
        Span::styled("▏", Style::new().fg(BORDER)),
    ]
}

fn procs_table(f: &mut Frame, app: &mut App, area: Rect, focused: bool) {
    let s = &app.sys;
    let pct = |u: u64, t: u64| if t == 0 { 0.0 } else { u as f32 / t as f32 * 100.0 };
    let mem_pct = pct(s.mem_used_kb, s.mem_total_kb);

    // title: what is listed and how
    let arrow = if app.desc { "↓" } else { "↑" };
    let mut tl = vec![
        Span::styled(" processes ", Style::new().fg(FG)),
        Span::styled(format!("{}/{} ", app.visible.len(), s.tasks), Style::new().fg(DIM)),
        Span::styled(format!("· {} running ", s.running), Style::new().fg(DIM)),
        Span::styled(format!("· sort {} {arrow} ", app.sort.label()), Style::new().fg(ACCENT)),
    ];
    if app.only_agents {
        tl.push(Span::styled("· agents only ", Style::new().fg(WARN)));
    }
    if app.typing || !app.filter.is_empty() {
        tl.push(Span::styled(
            format!("· /{}{} ", app.filter, if app.typing { "▏" } else { "" }),
            Style::new().fg(WARN),
        ));
    }
    // right title: system meters
    let mut tr = vec![Span::styled(" cpu ", Style::new().fg(DIM))];
    tr.extend(meter(s.cpu, 10));
    tr.push(Span::styled(format!("{:>3.0}% ×{}  ", s.cpu, s.ncpu), Style::new().fg(FG)));
    tr.push(Span::styled("mem ", Style::new().fg(DIM)));
    tr.extend(meter(mem_pct, 10));
    tr.push(Span::styled(
        format!("{}/{}  ", util::fmt_mem(s.mem_used_kb), util::fmt_mem(s.mem_total_kb)),
        Style::new().fg(FG),
    ));
    if s.swap_total_kb > 0 {
        tr.push(Span::styled(
            format!("swap {}  ", util::fmt_mem(s.swap_used_kb)),
            Style::new().fg(DIM),
        ));
    }
    tr.push(Span::styled(
        format!("load {:.2} {:.2} {:.2} ", s.load[0], s.load[1], s.load[2]),
        Style::new().fg(DIM),
    ));
    let block = pane_block(focused).title(Line::from(tl)).title(Line::from(tr).right_aligned());

    let total_kb = s.mem_total_kb.max(1) as f32;
    let hl_agent = if app.view == View::Split && app.focus == Pane::Agents {
        app.selected_agent().map(|a| a.pid)
    } else {
        None
    };
    let rows: Vec<Row> = app
        .visible
        .iter()
        .map(|&i| {
            let p = &app.procs[i];
            let cpu_c = if p.cpu >= 90.0 {
                HOT
            } else if p.cpu >= 30.0 {
                WARN
            } else if p.cpu >= 0.5 {
                FG
            } else {
                DIM
            };
            let st_c = match p.state {
                'R' => hex(Status::Busy.color()),
                'D' | 'Z' => HOT,
                _ => DIM,
            };
            let agent = match p.agent {
                Some((k, _)) => Span::styled(k.label(), Style::new().fg(hex(k.color()))),
                None => Span::raw(""),
            };
            let name_c = p.agent.map(|(k, _)| hex(k.color())).unwrap_or(FG);
            let (name, args) = match p.argv.split_first() {
                Some((a0, rest)) => {
                    let b = a0.rsplit('/').next().unwrap_or(a0);
                    let mut r = String::new();
                    for a in rest {
                        r.push(' ');
                        r.push_str(&util::tilde(a));
                        if r.len() > 300 {
                            break;
                        }
                    }
                    let r = util::one_line(&r);
                    (b.to_string(), if r.is_empty() { r } else { format!(" {r}") })
                }
                None => (format!("[{}]", p.comm), String::new()),
            };
            let mut row = Row::new(vec![
                Cell::from(format!("{:>7}", p.pid)),
                Cell::from(Span::styled(util::trunc(&p.user, 9), Style::new().fg(DIM))),
                Cell::from(agent),
                Cell::from(Span::styled(p.state.to_string(), Style::new().fg(st_c))),
                Cell::from(Span::styled(format!("{:>6.1}", p.cpu), Style::new().fg(cpu_c))),
                Cell::from(format!("{:>5.1}", p.rss_kb as f32 / total_kb * 100.0)),
                Cell::from(format!("{:>6}", util::fmt_mem(p.rss_kb))),
                Cell::from(format!("{:>4}", p.threads)),
                Cell::from(format!("{:>7}", fmt_cputime(p.cpu_time_s))),
                Cell::from(Line::from(vec![
                    Span::styled(name, Style::new().fg(name_c).add_modifier(Modifier::BOLD)),
                    Span::styled(args, Style::new().fg(DIM)),
                ])),
            ])
            .style(Style::new().fg(FG));
            if hl_agent.is_some() && p.agent.map(|(_, r)| r) == hl_agent {
                row = row.style(Style::new().fg(FG).bg(TREE_BG));
            }
            row
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(7),
            Constraint::Length(9),
            Constraint::Length(8),
            Constraint::Length(1),
            Constraint::Length(6),
            Constraint::Length(5),
            Constraint::Length(6),
            Constraint::Length(4),
            Constraint::Length(7),
            Constraint::Fill(1),
        ],
    )
    .header(
        Row::new(["    PID", "USER", "AGENT", "S", "  CPU%", " MEM%", "   RSS", " THR", "   TIME", "COMMAND"])
            .style(Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
    )
    .row_highlight_style(Style::new().bg(SEL_BG))
    .highlight_symbol(if focused || app.view == View::Procs { "▌" } else { " " })
    .block(block);
    if app.pane() == Pane::Procs {
        app.page = area.height.saturating_sub(4).max(1) as usize;
    }
    f.render_stateful_widget(table, area, &mut app.pstate);
}

fn help_line(f: &mut Frame, app: &App, area: Rect) {
    let dim = Style::new().fg(DIM);
    let line = if let Some((pid, name)) = &app.confirm_kill {
        Line::from(Span::styled(format!(" send SIGTERM to {pid} ({name})?  y / n"), Style::new().fg(HOT)))
    } else if app.typing {
        Line::from(Span::styled(" filter: type to match pid/user/command/agent · ⏎ keep · esc clear", dim))
    } else {
        let mut t = String::from(" 1/2/3/tab views · ↑↓ jk select · ⏎ focus window");
        if app.view == View::Split {
            t.push_str(" · ←→ switch pane");
        }
        if app.view != View::Agents {
            t.push_str(" · sort c m t p n · a agents · / filter");
            if app.pane() == Pane::Procs {
                t.push_str(" · K kill");
            }
        }
        t.push_str(" · q quit");
        let mut v = vec![Span::styled(t, dim)];
        if let Some((m, at)) = &app.msg {
            if at.elapsed() < Duration::from_secs(4) {
                v.push(Span::styled(format!("   {m}"), Style::new().fg(ACCENT)));
            }
        }
        Line::from(v)
    };
    f.render_widget(Paragraph::new(line), area);
}

fn detail(f: &mut Frame, app: &App, area: Rect) {
    let block = pane_block(false);
    let Some(a) = app.selected_agent() else {
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
