//! Process scanner: walks /proc, finds AI agent processes and works out
//! what each one is doing (CPU, RAM, child commands, session activity).

use crate::claude;
use crate::util;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Claude,
    Codex,
    Gemini,
    Agy,
    Opencode,
    Hermes,
}

pub const ALL_KINDS: [Kind; 6] = [
    Kind::Claude,
    Kind::Codex,
    Kind::Gemini,
    Kind::Agy,
    Kind::Opencode,
    Kind::Hermes,
];

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Claude => "claude",
            Kind::Codex => "codex",
            Kind::Gemini => "gemini",
            Kind::Agy => "agy",
            Kind::Opencode => "opencode",
            Kind::Hermes => "hermes",
        }
    }
    pub fn short(self) -> &'static str {
        match self {
            Kind::Claude => "C",
            Kind::Codex => "X",
            Kind::Gemini => "G",
            Kind::Agy => "A",
            Kind::Opencode => "O",
            Kind::Hermes => "H",
        }
    }
    pub fn color(self) -> &'static str {
        match self {
            Kind::Claude => "#E07B53",
            Kind::Codex => "#10B981",
            Kind::Gemini => "#60A5FA",
            Kind::Agy => "#A78BFA",
            Kind::Opencode => "#FACC15",
            Kind::Hermes => "#22D3EE",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Busy,
    Waiting,
    Idle,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Busy => "busy",
            Status::Waiting => "waiting",
            Status::Idle => "idle",
        }
    }
    pub fn color(self) -> &'static str {
        match self {
            Status::Busy => "#4ADE80",
            Status::Waiting => "#FBBF24",
            Status::Idle => "#6B7280",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Agent {
    pub kind: Kind,
    pub pid: u32,
    /// Session name (Claude) or project directory name.
    pub name: String,
    pub cwd: String,
    pub status: Status,
    /// Smoothed CPU % of the whole process tree.
    pub cpu: f32,
    pub rss_kb: u64,
    pub uptime_s: u64,
    pub procs: usize,
    /// Commands the agent is currently running in a shell.
    pub running: Vec<String>,
    /// Last thing the agent did (tool call / message), when known.
    pub activity: Option<String>,
    pub activity_age_s: Option<u64>,
    pub session_id: Option<String>,
    pub busy_for_s: Option<u64>,
}

struct Proc {
    pid: u32,
    ppid: u32,
    comm: String,
    argv: Vec<String>,
    ticks: u64,
    start: u64,
    rss_pages: u64,
}

/// Smoothed CPU % above which a heuristically-tracked agent counts as busy.
const BUSY_CPU: f32 = 5.0;

pub struct Scanner {
    home: PathBuf,
    clk: f64,
    page_kb: u64,
    prev_at: Option<Instant>,
    prev_ticks: HashMap<u32, u64>,
    ema: HashMap<u32, f32>,
    last_signal: HashMap<u32, Instant>,
    busy_since: HashMap<u32, Instant>,
    argv_cache: HashMap<u32, (u64, Vec<String>)>,
    claude: claude::Cache,
}

impl Scanner {
    pub fn new() -> Self {
        let clk = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
        let page_kb = (unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) / 1024) as u64;
        Scanner {
            home: util::home(),
            clk,
            page_kb,
            prev_at: None,
            prev_ticks: HashMap::new(),
            ema: HashMap::new(),
            last_signal: HashMap::new(),
            busy_since: HashMap::new(),
            argv_cache: HashMap::new(),
            claude: claude::Cache::default(),
        }
    }

    fn read_procs(&mut self) -> Vec<Proc> {
        let mut out = Vec::with_capacity(512);
        let Ok(rd) = fs::read_dir("/proc") else {
            return out;
        };
        let mut live = HashSet::new();
        for e in rd.flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
                continue;
            };
            let (Some(l), Some(r)) = (stat.find('('), stat.rfind(')')) else {
                continue;
            };
            let f: Vec<&str> = stat[r + 1..].split_whitespace().collect();
            if f.len() < 22 {
                continue;
            }
            let ppid: u32 = f[1].parse().unwrap_or(0);
            if pid == 2 || ppid == 2 {
                continue; // kernel threads
            }
            let start: u64 = f[19].parse().unwrap_or(0);
            let argv = match self.argv_cache.get(&pid) {
                Some((s, a)) if *s == start => a.clone(),
                _ => {
                    let a: Vec<String> = fs::read(format!("/proc/{pid}/cmdline"))
                        .map(|b| {
                            b.split(|&c| c == 0)
                                .filter(|s| !s.is_empty())
                                .map(|s| String::from_utf8_lossy(s).into_owned())
                                .collect()
                        })
                        .unwrap_or_default();
                    self.argv_cache.insert(pid, (start, a.clone()));
                    a
                }
            };
            live.insert(pid);
            out.push(Proc {
                pid,
                ppid,
                comm: stat[l + 1..r].to_string(),
                argv,
                ticks: f[11].parse::<u64>().unwrap_or(0) + f[12].parse::<u64>().unwrap_or(0),
                start,
                rss_pages: f[21].parse().unwrap_or(0),
            });
        }
        self.argv_cache.retain(|p, _| live.contains(p));
        out
    }

    pub fn scan(&mut self) -> Vec<Agent> {
        let now = Instant::now();
        let dt = self
            .prev_at
            .map(|t| now.duration_since(t).as_secs_f64())
            .filter(|d| *d > 0.05);
        let procs = self.read_procs();
        let uptime = fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|s| s.split_whitespace().next().and_then(|x| x.parse::<f64>().ok()))
            .unwrap_or(0.0);

        let mut idx: HashMap<u32, usize> = HashMap::with_capacity(procs.len());
        let mut children: HashMap<u32, Vec<usize>> = HashMap::new();
        for (i, p) in procs.iter().enumerate() {
            idx.insert(p.pid, i);
            children.entry(p.ppid).or_default().push(i);
        }
        let kinds: Vec<Option<Kind>> = procs.iter().map(classify).collect();
        // An agent is "top level" unless its parent is the same kind of agent
        // (wrappers that re-exec themselves, node launchers, ...).
        let top: Vec<bool> = procs
            .iter()
            .enumerate()
            .map(|(i, p)| match kinds[i] {
                None => false,
                Some(k) => idx.get(&p.ppid).is_none_or(|&pi| kinds[pi] != Some(k)),
            })
            .collect();

        let mut agents = Vec::new();
        let mut new_ema = HashMap::new();
        let mut seen = HashSet::new();

        for (i, p) in procs.iter().enumerate() {
            if !top[i] {
                continue;
            }
            let kind = kinds[i].unwrap();
            seen.insert(p.pid);

            // Process tree, not descending into other (nested) agents.
            let mut tree = vec![i];
            let mut stack = vec![p.pid];
            while let Some(pp) = stack.pop() {
                if let Some(ch) = children.get(&pp) {
                    for &c in ch {
                        if top[c] {
                            continue;
                        }
                        tree.push(c);
                        stack.push(procs[c].pid);
                    }
                }
            }

            let (mut dticks, mut rss) = (0u64, 0u64);
            for &t in &tree {
                let q = &procs[t];
                rss += q.rss_pages * self.page_kb;
                dticks += match self.prev_ticks.get(&q.pid) {
                    Some(&pt) => q.ticks.saturating_sub(pt),
                    None if dt.is_some() => q.ticks,
                    None => 0,
                };
            }
            let cpu_now = dt.map_or(0.0, |d| dticks as f64 / self.clk / d * 100.0) as f32;
            let cpu = match self.ema.get(&p.pid) {
                Some(e) => 0.5 * e + 0.5 * cpu_now,
                None => cpu_now,
            };
            new_ema.insert(p.pid, cpu);

            let cwd = fs::read_link(format!("/proc/{}/cwd", p.pid))
                .map(|x| x.to_string_lossy().into_owned())
                .unwrap_or_default();
            let running = running_cmds(&procs, &tree, &children, &idx);

            let mut a = Agent {
                kind,
                pid: p.pid,
                name: default_name(kind, p, &cwd),
                cwd,
                status: Status::Idle,
                cpu,
                rss_kb: rss,
                uptime_s: (uptime - p.start as f64 / self.clk).max(0.0) as u64,
                procs: tree.len(),
                running,
                activity: None,
                activity_age_s: None,
                session_id: None,
                busy_for_s: None,
            };

            let mut exact = None;
            if kind == Kind::Claude {
                if let Some(s) = claude::session(&self.home, p.pid) {
                    if let Some(n) = &s.name {
                        a.name = n.clone();
                    }
                    if !s.cwd.is_empty() {
                        a.cwd = s.cwd.clone();
                    }
                    exact = Some(match s.status.as_deref() {
                        Some("busy") | Some("running") | Some("working") => Status::Busy,
                        Some("idle") | None => Status::Idle,
                        Some(_) => Status::Waiting,
                    });
                    if let Some(ms) = s.status_since_ms {
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0);
                        let since = now.checked_sub(Duration::from_millis(now_ms.saturating_sub(ms)));
                        if let Some(since) = since {
                            self.busy_since.insert(p.pid, since);
                        }
                    }
                    let (act, age) = self.claude.activity(&self.home, &s);
                    a.activity = act;
                    a.activity_age_s = age;
                    a.session_id = Some(s.session_id);
                }
            }
            a.status = exact.unwrap_or_else(|| {
                // Heuristic: sustained CPU or a shell command running.
                // A short hold-off avoids flapping between busy and idle.
                if cpu >= BUSY_CPU || !a.running.is_empty() {
                    self.last_signal.insert(p.pid, now);
                }
                match self.last_signal.get(&p.pid) {
                    Some(t) if now.duration_since(*t) < Duration::from_secs(6) => Status::Busy,
                    _ => Status::Idle,
                }
            });
            if a.activity.is_none() {
                a.activity = a.running.first().map(|c| format!("$ {c}"));
            }
            if a.status == Status::Busy {
                let since = *self.busy_since.entry(p.pid).or_insert(now);
                a.busy_for_s = Some(now.duration_since(since).as_secs());
            } else {
                self.busy_since.remove(&p.pid);
            }
            agents.push(a);
        }

        self.prev_ticks = procs.iter().map(|p| (p.pid, p.ticks)).collect();
        self.ema = new_ema;
        self.prev_at = Some(now);
        self.last_signal.retain(|p, _| seen.contains(p));
        self.busy_since.retain(|p, _| seen.contains(p));
        self.claude.retain(&agents);

        agents.sort_by(|a, b| {
            (a.status, a.kind, &a.name, a.pid).cmp(&(b.status, b.kind, &b.name, b.pid))
        });
        agents
    }
}

fn base(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

fn classify(p: &Proc) -> Option<Kind> {
    // some npm packages ship native binaries as "<name>.exe" even on Linux
    let a0 = base(p.argv.first()?).trim_end_matches(".exe");
    let arg1 = p.argv.get(1).map(String::as_str).unwrap_or("");
    let a1 = base(arg1).trim_end_matches(".exe");
    let c = p.comm.trim_end_matches(".exe");
    let js = matches!(a0, "node" | "bun" | "deno" | "nodejs");

    if a0 == "claude" || c == "claude" {
        if p
            .argv
            .iter()
            .any(|a| a == "--chrome-native-host" || a == "mcp" || a == "daemon")
        {
            return None;
        }
        return Some(Kind::Claude);
    }
    if a0 == "codex" || c == "codex" || (js && (a1 == "codex" || arg1.contains("@openai/codex"))) {
        return Some(Kind::Codex);
    }
    if a0 == "gemini" || c == "gemini" || (js && (a1 == "gemini" || arg1.contains("gemini-cli"))) {
        return Some(Kind::Gemini);
    }
    if a0 == "agy" || c == "agy" {
        return Some(Kind::Agy);
    }
    if matches!(a0, "opencode" | ".opencode") || matches!(c, "opencode" | ".opencode") {
        return Some(Kind::Opencode);
    }
    if a0 == "hermes"
        || c == "hermes"
        || (a0.starts_with("python")
            && p.argv
                .iter()
                .any(|a| a.contains("hermes_cli") || a.contains("hermes-agent")))
    {
        return Some(Kind::Hermes);
    }
    None
}

fn default_name(kind: Kind, p: &Proc, cwd: &str) -> String {
    if kind == Kind::Hermes && p.argv.iter().any(|a| a == "gateway") {
        return "gateway".into();
    }
    if p.argv.get(1).is_some_and(|a| a == "serve" || a == "server" || a == "daemon") {
        return "server".into();
    }
    let t = util::tilde(cwd);
    if t == "~" || t.is_empty() {
        return "~".into();
    }
    base(&t).to_string()
}

fn is_shell(p: &Proc) -> bool {
    matches!(p.comm.as_str(), "bash" | "zsh" | "sh" | "dash" | "fish" | "ksh")
}

/// Commands spawned through a shell by the agent (e.g. Claude's Bash tool).
fn running_cmds(
    procs: &[Proc],
    tree: &[usize],
    children: &HashMap<u32, Vec<usize>>,
    _idx: &HashMap<u32, usize>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for &t in &tree[1..] {
        let q = &procs[t];
        if !is_shell(q) {
            continue;
        }
        // Only shells the agent itself spawned (tool calls). Shells started by
        // helpers (e.g. `npm exec mcp-remote` -> sh -> node) are long-lived
        // servers, not work in progress.
        if q.ppid != procs[tree[0]].pid {
            continue;
        }
        let mut found = false;
        let mut stack = vec![q.pid];
        while let Some(sp) = stack.pop() {
            if let Some(ch) = children.get(&sp) {
                for &c in ch {
                    let cp = &procs[c];
                    if is_shell(cp) {
                        stack.push(cp.pid);
                    } else if !cp.argv.is_empty() {
                        out.push(cmd_summary(&cp.argv));
                        found = true;
                    }
                }
            }
        }
        if !found {
            if let Some(c) = shell_script(&q.argv) {
                out.push(c);
            }
        }
    }
    let mut seen = HashSet::new();
    out.retain(|c| !c.contains("mcp") && seen.insert(c.clone()));
    out.truncate(5);
    out
}

fn cmd_summary(argv: &[String]) -> String {
    let mut s = base(&argv[0]).to_string();
    for a in &argv[1..] {
        s.push(' ');
        s.push_str(&util::tilde(a));
        if s.len() > 160 {
            break;
        }
    }
    util::trunc(&util::one_line(&s), 100)
}

fn shell_script(argv: &[String]) -> Option<String> {
    let i = argv
        .iter()
        .position(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('c') && a.len() <= 4)?;
    let script = argv.get(i + 1)?;
    // Claude Code wraps commands as: source snapshot && ... && eval '<cmd>' < /dev/null && pwd ...
    let cmd = match script.find("eval '") {
        Some(p) => {
            let rest = &script[p + 6..];
            let end = rest.rfind("' \\< /dev/null").or_else(|| rest.rfind('\'')).unwrap_or(rest.len());
            rest[..end].replace("'\\''", "'")
        }
        None => script.clone(),
    };
    let c = util::one_line(&cmd);
    (!c.is_empty()).then(|| util::trunc(&c, 100))
}

/// (busy, waiting, total) per kind, only for kinds that are present.
pub fn summary(agents: &[Agent]) -> Vec<(Kind, usize, usize, usize)> {
    ALL_KINDS
        .iter()
        .filter_map(|&k| {
            let v: Vec<&Agent> = agents.iter().filter(|a| a.kind == k).collect();
            if v.is_empty() {
                return None;
            }
            let busy = v.iter().filter(|a| a.status == Status::Busy).count();
            let wait = v.iter().filter(|a| a.status == Status::Waiting).count();
            Some((k, busy, wait, v.len()))
        })
        .collect()
}

pub fn totals(agents: &[Agent]) -> (usize, usize, usize) {
    let busy = agents.iter().filter(|a| a.status == Status::Busy).count();
    let wait = agents.iter().filter(|a| a.status == Status::Waiting).count();
    (busy, wait, agents.len())
}
