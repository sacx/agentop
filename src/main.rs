mod bar;
mod canvas;
mod claude;
mod notify;
mod scan;
mod sni;
mod tray;
mod tui;
mod util;
mod xembed;
mod xutil;

use std::time::Duration;

const HELP: &str = "\
agentop — see what your AI agents are doing (claude, codex, gemini, agy, opencode, hermes)

USAGE:
  agentop [tui]                 interactive dashboard (default)
  agentop tray                  tray icon (StatusNotifierItem or XEmbed, auto-detected)
  agentop polybar               polybar custom/script module (tail = true)
  agentop i3bar                 i3bar block; wrap i3status: `i3status | agentop i3bar`
  agentop status [--json]       print current agents once
  agentop open                  toggle the dashboard in a floating terminal
  agentop focus <pid>           focus the terminal window of an agent

OPTIONS:
  --interval <secs>             refresh interval (tui 1, others 2)
  --short                       bars: one-letter agent names
  --detail                      bars: also show what the latest busy agent is doing
  --no-icons                    bars: no Nerd Font glyph
  --notify / --no-notify        desktop notifications (default: on for tray, off for bars)
  --backend auto|sni|xembed     tray: force a tray protocol

ENV:
  AGENTOP_TERM    terminal command for `open`, e.g. \"alacritty --class agentop -e\"
  AGENTOP_FONT    font file for the tray popup (default: fontconfig monospace)
  AGENTOP_TRAY_BG tray background fallback colour, e.g. #0A0E14
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("tui");
    let flag = |f: &str| args.iter().any(|a| a == f);
    let opt = |f: &str| {
        args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned()
    };
    let interval = |d: f64| {
        Duration::from_secs_f64(
            opt("--interval").and_then(|s| s.parse::<f64>().ok()).unwrap_or(d).max(0.2),
        )
    };
    let bar_opts = || bar::BarOpts {
        interval: interval(2.0),
        short: flag("--short"),
        detail: flag("--detail"),
        notify: flag("--notify"),
        icons: !flag("--no-icons"),
    };

    let res: Result<(), Box<dyn std::error::Error>> = match cmd {
        "tui" => tui::run(flag("--popup"), interval(1.0)),
        "tray" => tray::run(
            interval(2.0),
            !flag("--no-notify"),
            opt("--backend").as_deref().unwrap_or("auto"),
        ),
        "polybar" => bar::polybar(bar_opts()).map_err(Into::into),
        "i3bar" | "i3status" => bar::i3bar(bar_opts()).map_err(Into::into),
        "status" => bar::status(flag("--json")).map_err(Into::into),
        "open" => {
            util::open_tui();
            Ok(())
        }
        "focus" => match args.get(1).and_then(|s| s.parse().ok()) {
            Some(pid) => xutil::focus_pid(pid).and_then(|ok| {
                if ok { Ok(()) } else { Err(format!("no window found for pid {pid}").into()) }
            }),
            None => Err("usage: agentop focus <pid>".into()),
        },
        "-h" | "--help" | "help" => {
            print!("{HELP}");
            Ok(())
        }
        "-V" | "--version" => {
            println!("agentop {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        other => Err(format!("unknown command '{other}' (see agentop --help)").into()),
    };
    if let Err(e) = res {
        // a closed pipe means the bar went away: exit quietly
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            if io.kind() == std::io::ErrorKind::BrokenPipe {
                std::process::exit(0);
            }
        }
        eprintln!("agentop: {e}");
        std::process::exit(1);
    }
}
