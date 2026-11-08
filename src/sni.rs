//! StatusNotifierItem tray backend (KDE, GNOME + AppIndicator extension,
//! XFCE, Cinnamon, waybar, …). Works on X11 and Wayland.
//! Left-click toggles the dashboard; the menu lists agents (click = focus).

use crate::canvas::{self, Canvas, TextFont};
use crate::notify::Notifier;
use crate::scan::{self, Agent, Scanner, Status};
use crate::{util, xutil};
use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{MenuItem, StandardItem};
use std::error::Error;
use std::time::Duration;

pub struct SniTray {
    agents: Vec<Agent>,
    font: Option<TextFont>,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

impl ksni::Tray for SniTray {
    fn id(&self) -> String {
        "agentop".into()
    }
    fn title(&self) -> String {
        "agentop".into()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        [22usize, 32, 48]
            .iter()
            .map(|&s| {
                let mut cv = Canvas::new(s, s, 0);
                canvas::badge(&mut cv, &self.agents, self.font.as_ref());
                ksni::Icon { width: s as i32, height: s as i32, data: cv.argb_be() }
            })
            .collect()
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        let (b, w, t) = scan::totals(&self.agents);
        let description = self
            .agents
            .iter()
            .map(|a| {
                let act = a.activity.as_deref().map(|s| format!(" — {}", util::trunc(s, 60))).unwrap_or_default();
                esc(&format!("{} {} · {}  {}{}", dot(a.status), a.kind.label(), a.name, a.status.label(), act))
            })
            .collect::<Vec<_>>()
            .join("\n");
        ksni::ToolTip {
            title: format!("agentop — {b} busy · {w} waiting · {t} agents"),
            description,
            ..Default::default()
        }
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        util::open_tui();
    }
    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut m: Vec<MenuItem<Self>> = Vec::new();
        if self.agents.is_empty() {
            m.push(StandardItem { label: "No AI agents running".into(), enabled: false, ..Default::default() }.into());
        }
        for a in &self.agents {
            let pid = a.pid;
            let label = format!(
                "{} {}  ·  {}  ·  {}",
                dot(a.status),
                a.kind.label(),
                util::trunc(&a.name, 28),
                a.activity.as_deref().map(|s| util::trunc(s, 50)).unwrap_or_else(|| a.status.label().into())
            );
            m.push(
                StandardItem {
                    label: label.replace('_', "__"),
                    activate: Box::new(move |_: &mut Self| {
                        let _ = xutil::focus_pid(pid);
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }
        m.push(MenuItem::Separator);
        m.push(
            StandardItem {
                label: "Open dashboard".into(),
                icon_name: "utilities-system-monitor".into(),
                activate: Box::new(|_: &mut Self| util::open_tui()),
                ..Default::default()
            }
            .into(),
        );
        m.push(
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|_: &mut Self| std::process::exit(0)),
                ..Default::default()
            }
            .into(),
        );
        m
    }
    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        true // keep running; the panel may come back
    }
}

fn dot(s: Status) -> &'static str {
    match s {
        Status::Busy => "●",
        Status::Waiting => "◆",
        Status::Idle => "○",
    }
}

/// Registers the tray. With `wait = false` this fails when no
/// StatusNotifierWatcher (panel/host) is running.
pub fn start(agents: Vec<Agent>, wait: bool) -> Result<Handle<SniTray>, ksni::Error> {
    SniTray { agents, font: TextFont::load(true) }.assume_sni_available(wait).spawn()
}

pub fn run(handle: Handle<SniTray>, mut sc: Scanner, interval: Duration, notify: bool) -> Result<(), Box<dyn Error>> {
    let mut nt = Notifier::new(notify);
    loop {
        std::thread::sleep(interval);
        let agents = sc.scan();
        nt.update(&agents);
        if handle.update(|t| t.agents = agents).is_none() {
            return Ok(());
        }
    }
}
