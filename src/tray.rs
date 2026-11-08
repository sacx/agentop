//! Tray entry point: single-instance lock + backend selection.
//!
//! auto: StatusNotifierItem if a watcher (KDE, GNOME+AppIndicator, XFCE,
//! waybar, …) is running; otherwise XEmbed on X11 (polybar, i3bar,
//! stalonetray, …); otherwise wait for an SNI host (Wayland).

use crate::scan::Scanner;
use crate::{sni, util, xembed};
use std::error::Error;
use std::os::fd::AsRawFd;
use std::time::Duration;

pub fn run(interval: Duration, notify: bool, backend: &str) -> Result<(), Box<dyn Error>> {
    let lock_path = util::runtime_dir().join("agentop-tray.lock");
    let lock = std::fs::File::create(&lock_path)?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        eprintln!("agentop: tray already running");
        return Ok(());
    }

    let has_x = std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty());
    match backend {
        "xembed" => xembed::run_xembed(interval, notify),
        "sni" => {
            let mut sc = Scanner::new();
            let h = sni::start(sc.scan(), true)?;
            sni::run(h, sc, interval, notify)
        }
        "auto" => {
            let mut sc = Scanner::new();
            match sni::start(sc.scan(), false) {
                Ok(h) => sni::run(h, sc, interval, notify),
                Err(_) if has_x => xembed::run_xembed(interval, notify),
                Err(_) => {
                    eprintln!("agentop: no tray host yet (no X11, no StatusNotifierWatcher); waiting…");
                    let h = sni::start(sc.scan(), true)?;
                    sni::run(h, sc, interval, notify)
                }
            }
        }
        other => Err(format!("unknown tray backend '{other}' (auto, sni, xembed)").into()),
    }
}
