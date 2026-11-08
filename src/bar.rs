//! Status bar outputs: polybar (custom/script, tail = true), i3bar
//! (wraps i3status or runs standalone) and a one-shot `status`.

use crate::notify::Notifier;
use crate::scan::{self, Agent, Scanner, Status};
use crate::util;
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::sync::mpsc;
use std::time::Duration;

const ICON: &str = "󰚩";
const DIM: &str = "#6B7280";

fn headline_color(agents: &[Agent]) -> &'static str {
    let (busy, wait, total) = scan::totals(agents);
    if wait > 0 {
        Status::Waiting.color()
    } else if busy > 0 {
        Status::Busy.color()
    } else if total > 0 {
        "#9CA3AF"
    } else {
        DIM
    }
}

/// Most interesting thing happening right now, for the `--detail` mode.
fn detail(agents: &[Agent]) -> Option<String> {
    agents
        .iter()
        .filter(|a| a.status != Status::Idle)
        .min_by_key(|a| a.activity_age_s.unwrap_or(u64::MAX))
        .and_then(|a| a.activity.as_ref().map(|t| format!("{}: {}", a.name, t)))
}

pub fn polybar_line(agents: &[Agent], short: bool, show_detail: bool, icons: bool) -> String {
    let mut s = if icons {
        format!("%{{F{}}}{ICON}%{{F-}}", headline_color(agents))
    } else {
        format!("%{{F{}}}AI%{{F-}}", headline_color(agents))
    };
    let sum = scan::summary(agents);
    if sum.is_empty() {
        s.push_str(&format!(" %{{F{DIM}}}–%{{F-}}"));
    }
    for (k, busy, wait, total) in sum {
        let label = if short { k.short() } else { k.label() };
        s.push_str(&format!(" %{{F{}}}{label}%{{F-}} ", k.color()));
        if wait > 0 {
            s.push_str(&format!("%{{F{}}}!{wait}%{{F-}} ", Status::Waiting.color()));
        }
        if busy > 0 {
            s.push_str(&format!("%{{F{}}}{busy}%{{F-}}/{total}", Status::Busy.color()));
        } else {
            s.push_str(&format!("%{{F{DIM}}}{total}%{{F-}}"));
        }
    }
    if show_detail {
        if let Some(d) = detail(agents) {
            let d = util::trunc(&d, 50).replace("%{", "% {");
            s.push_str(&format!(" %{{F{DIM}}}{d}%{{F-}}"));
        }
    }
    s
}

fn pango_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn i3_block(agents: &[Agent], short: bool, show_detail: bool) -> Value {
    let span = |c: &str, t: &str| format!("<span foreground='{c}'>{}</span>", pango_escape(t));
    let mut full = span(headline_color(agents), ICON);
    let mut brief = full.clone();
    let sum = scan::summary(agents);
    if sum.is_empty() {
        full.push(' ');
        full.push_str(&span(DIM, "–"));
    }
    for (k, busy, wait, total) in &sum {
        let mut counts = String::new();
        if *wait > 0 {
            counts.push_str(&span(Status::Waiting.color(), &format!("!{wait} ")));
        }
        if *busy > 0 {
            counts.push_str(&span(Status::Busy.color(), &busy.to_string()));
            counts.push_str(&format!("/{total}"));
        } else {
            counts.push_str(&span(DIM, &total.to_string()));
        }
        let label = if short { k.short() } else { k.label() };
        full.push_str(&format!(" {} {counts}", span(k.color(), label)));
        brief.push_str(&format!(" {}{counts}", span(k.color(), k.short())));
    }
    if show_detail {
        if let Some(d) = detail(agents) {
            full.push(' ');
            full.push_str(&span(DIM, &util::trunc(&d, 50)));
        }
    }
    json!({
        "name": "agentop",
        "full_text": full,
        "short_text": brief,
        "markup": "pango",
    })
}

pub struct BarOpts {
    pub interval: Duration,
    pub short: bool,
    pub detail: bool,
    pub notify: bool,
    pub icons: bool,
}

pub fn polybar(o: BarOpts) -> io::Result<()> {
    let mut sc = Scanner::new();
    let mut nt = Notifier::new(o.notify);
    let mut last = String::new();
    let out = io::stdout();
    loop {
        let agents = sc.scan();
        nt.update(&agents);
        let line = polybar_line(&agents, o.short, o.detail, o.icons);
        if line != last {
            let mut w = out.lock();
            writeln!(w, "{line}")?; // exits on EPIPE when polybar goes away
            w.flush()?;
            last = line;
        }
        std::thread::sleep(o.interval);
    }
}

fn stdin_is_pipe() -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata("/dev/stdin")
        .map(|m| m.file_type().is_fifo())
        .unwrap_or(false)
}

/// i3bar protocol. With i3status piped in (`i3status | agentop i3bar`) our
/// block is prepended to i3status' blocks; otherwise runs standalone.
pub fn i3bar(o: BarOpts) -> io::Result<()> {
    let mut sc = Scanner::new();
    let mut nt = Notifier::new(o.notify);
    let out = io::stdout();
    let (tx, rx) = mpsc::channel::<String>();
    let wrap = stdin_is_pipe();

    let mut header_done = false;
    if wrap {
        std::thread::spawn(move || {
            for line in io::stdin().lock().lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    } else {
        let mut w = out.lock();
        writeln!(w, "{{\"version\":1}}\n[")?;
        w.flush()?;
        header_done = true;
    }

    let mut others: Vec<Value> = Vec::new();
    let mut started = false;
    let mut agents = sc.scan();
    nt.update(&agents);
    let mut next_scan = std::time::Instant::now() + o.interval;
    loop {
        let timeout = next_scan.saturating_duration_since(std::time::Instant::now());
        let mut emit = !wrap || header_done;
        match rx.recv_timeout(timeout) {
            Ok(line) => {
                let t = line.trim();
                if !header_done {
                    // pass through i3status header and the opening '['
                    let mut w = out.lock();
                    writeln!(w, "{t}")?;
                    w.flush()?;
                    if t.starts_with('[') {
                        header_done = true;
                    }
                    continue;
                }
                let t = t.trim_start_matches(',');
                if let Ok(Value::Array(a)) = serde_json::from_str(t) {
                    others = a;
                }
                emit = true;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                agents = sc.scan();
                nt.update(&agents);
                next_scan = std::time::Instant::now() + o.interval;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        }
        if !emit {
            continue;
        }
        let mut blocks = vec![i3_block(&agents, o.short, o.detail)];
        blocks.extend(others.iter().cloned());
        let mut w = out.lock();
        writeln!(w, "{}{}", if started { "," } else { "" }, Value::Array(blocks))?;
        w.flush()?;
        started = true;
    }
}

pub fn status(as_json: bool) -> io::Result<()> {
    let mut sc = Scanner::new();
    sc.scan();
    std::thread::sleep(Duration::from_millis(500)); // second sample for CPU
    let agents = sc.scan();
    let mut w = io::stdout().lock();
    if as_json {
        writeln!(w, "{}", serde_json::to_string_pretty(&agents).unwrap_or_default())?;
        return Ok(());
    }
    if agents.is_empty() {
        writeln!(w, "no agents running")?;
    }
    for a in &agents {
        writeln!(
            w,
            "{:<8} {:>7} {:<7} {:<24} {:>5.1}% {:>6} {:>6}  {}",
            a.kind.label(),
            a.pid,
            a.status.label(),
            util::trunc(&a.name, 24),
            a.cpu,
            util::fmt_mem(a.rss_kb),
            util::fmt_dur(a.uptime_s),
            util::trunc(a.activity.as_deref().unwrap_or(""), 70)
        )?;
    }
    Ok(())
}
