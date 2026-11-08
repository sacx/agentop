use std::path::PathBuf;
use std::process::{Command, Stdio};

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into())
}

pub fn tilde(p: &str) -> String {
    let h = home();
    let h = h.to_string_lossy();
    if h.len() > 1 {
        if p == h {
            return "~".into();
        }
        if let Some(r) = p.strip_prefix(&format!("{h}/")) {
            return format!("~/{r}");
        }
        // also inside strings (commands)
        if p.contains(&*h) {
            return p.replace(&format!("{h}/"), "~/");
        }
    }
    p.to_string()
}

pub fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut o: String = s.chars().take(n.saturating_sub(1)).collect();
    o.push('…');
    o
}

/// First non-empty line, whitespace collapsed.
pub fn one_line(s: &str) -> String {
    let l = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    l.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn fmt_dur(s: u64) -> String {
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
        _ => format!("{}d{}h", s / 86400, (s % 86400) / 3600),
    }
}

pub fn fmt_mem(kb: u64) -> String {
    if kb < 1024 {
        format!("{kb}K")
    } else if kb < 1024 * 1024 {
        format!("{}M", kb / 1024)
    } else {
        format!("{:.1}G", kb as f64 / 1048576.0)
    }
}

pub fn rgb(hex: &str) -> u32 {
    u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0xFFFFFF)
}

pub fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

pub fn which(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
        .unwrap_or(false)
}

/// Spawn without blocking; a thread reaps the child.
pub fn spawn_detached(cmd: &mut Command) {
    if let Ok(mut c) = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        std::thread::spawn(move || {
            let _ = c.wait();
        });
    }
}

/// Terminal used for the dashboard window. Override with AGENTOP_TERM,
/// e.g. AGENTOP_TERM="alacritty --class agentop -e".
pub fn terminal_cmd() -> Vec<String> {
    if let Ok(t) = std::env::var("AGENTOP_TERM") {
        return t.split_whitespace().map(String::from).collect();
    }
    const TERMS: &[&[&str]] = &[
        &["kitty", "--class", "agentop", "-T", "agentop", "-e"],
        &["alacritty", "--class", "agentop", "-T", "agentop", "-e"],
        &["wezterm", "start", "--class", "agentop", "--"],
        &["ghostty", "--class=agentop", "--title=agentop", "-e"],
        &["foot", "--app-id", "agentop", "-T", "agentop"],
        &["gnome-terminal", "--class=agentop", "--title=agentop", "--"],
        &["konsole", "-p", "tabtitle=agentop", "-e"],
        &["xfce4-terminal", "--class=agentop", "-T", "agentop", "-x"],
        &["tilix", "-t", "agentop", "-e"],
        &["xterm", "-class", "agentop", "-T", "agentop", "-e"],
        &["x-terminal-emulator", "-e"],
    ];
    let v = TERMS.iter().find(|t| which(t[0])).copied().unwrap_or(TERMS[9]);
    v.iter().map(|s| s.to_string()).collect()
}

pub fn pidfile() -> PathBuf {
    runtime_dir().join("agentop-popup.pid")
}

/// Toggle the dashboard popup: close it if open, otherwise open it.
pub fn open_tui() {
    if let Ok(s) = std::fs::read_to_string(pidfile()) {
        if let Ok(pid) = s.trim().parse::<i32>() {
            let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
            if comm.trim() == "agentop" {
                unsafe { libc::kill(pid, libc::SIGTERM) };
                let _ = std::fs::remove_file(pidfile());
                return;
            }
        }
    }
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "agentop".into());
    let mut t = terminal_cmd();
    t.push(exe);
    t.push("tui".into());
    t.push("--popup".into());
    spawn_detached(Command::new(&t[0]).args(&t[1..]));
}

pub fn notify(summary: &str, body: &str, urgent: bool) {
    if !which("notify-send") {
        return;
    }
    let mut c = Command::new("notify-send");
    c.args(["-a", "agentop", "-u", if urgent { "critical" } else { "normal" }, summary, body]);
    spawn_detached(&mut c);
}
