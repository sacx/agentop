//! Focus the terminal window an agent runs in: walk the agent's parent
//! chain until a pid owns an X window (_NET_WM_PID), then focus it via
//! i3-msg (or EWMH _NET_ACTIVE_WINDOW on other window managers).

use std::collections::HashMap;
use std::error::Error;
use std::process::Command;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::rust_connection::RustConnection;

fn ppid(pid: u32) -> Option<u32> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let r = s.rfind(')')?;
    s[r + 1..].split_whitespace().nth(1)?.parse().ok()
}

fn atom(c: &RustConnection, n: &str) -> Result<Atom, Box<dyn Error>> {
    Ok(c.intern_atom(false, n.as_bytes())?.reply()?.atom)
}

pub fn focus_pid(pid: u32) -> Result<bool, Box<dyn Error>> {
    let (c, screen) = x11rb::connect(None)?;
    let root = c.setup().roots[screen].root;
    let client_list = atom(&c, "_NET_CLIENT_LIST")?;
    let wm_pid = atom(&c, "_NET_WM_PID")?;
    let wins: Vec<u32> = c
        .get_property(false, root, client_list, AtomEnum::WINDOW, 0, 4096)?
        .reply()?
        .value32()
        .map(|v| v.collect())
        .unwrap_or_default();
    let mut by_pid: HashMap<u32, u32> = HashMap::new();
    for w in wins {
        if let Ok(r) = c.get_property(false, w, wm_pid, AtomEnum::CARDINAL, 0, 1)?.reply() {
            if let Some(p) = r.value32().and_then(|mut v| v.next()) {
                by_pid.entry(p).or_insert(w);
            }
        }
    }
    let mut p = pid;
    for _ in 0..64 {
        if let Some(&w) = by_pid.get(&p) {
            let ok = Command::new("i3-msg")
                .args(["-q", &format!("[id=\"{w}\"] focus")])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                let active = atom(&c, "_NET_ACTIVE_WINDOW")?;
                let ev = ClientMessageEvent::new(32, w, active, [1, 0, 0, 0, 0]);
                c.send_event(
                    false,
                    root,
                    EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                    ev,
                )?;
                c.flush()?;
            }
            return Ok(true);
        }
        match ppid(p) {
            Some(pp) if pp > 1 => p = pp,
            _ => break,
        }
    }
    Ok(false)
}
