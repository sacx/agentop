//! XEmbed tray backend: system tray icon via the XEmbed / freedesktop system tray protocol
//! (works with polybar's internal/tray, i3bar, stalonetray, trayer, ...).
//! Pure x11rb, no GTK: hover shows a summary popup, left-click toggles the
//! dashboard, right-click pins the popup, clicking a row focuses that agent.

use crate::canvas::{self, Canvas, TextFont};
use crate::notify::Notifier;
use crate::scan::{self, Agent, Scanner};
use crate::{util, xutil};
use std::error::Error;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{CURRENT_TIME, NONE};

type R<T> = Result<T, Box<dyn Error>>;

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        _NET_SYSTEM_TRAY_OPCODE,
        _NET_SYSTEM_TRAY_VISUAL,
        _XEMBED_INFO,
        MANAGER,
    }
}

const BG: u32 = 0x0F141B;
const FG: u32 = 0xE6E6E6;
const DIM: u32 = 0x6B7280;
const SOFT: u32 = 0xA9B1BD;
const ACCENT: u32 = 0x00F0FF;
const BORDER: u32 = 0x2A3441;
const FONT_PX: f32 = 14.0;

struct PRow {
    segs: Vec<(String, u32)>,
    dot: Option<u32>,
    pid: Option<u32>,
    bold: bool,
}

struct Popup {
    win: Window,
    gc: Gcontext,
    pinned: bool,
    rows: Vec<Option<u32>>,
    y0: i32,
    lh: i32,
}

struct Tray {
    conn: RustConnection,
    screen: usize,
    root: Window,
    root_visual: Visualid,
    root_depth: u8,
    sw: i32,
    sh: i32,
    atoms: Atoms,
    tray_atom: Atom,
    win: Window,
    gc: Gcontext,
    visual: Visualid,
    depth: u8,
    w: u16,
    h: u16,
    owner: Window,
    docked: bool,
    fallback_bg: u32,
    font: Option<TextFont>,
    bold: Option<TextFont>,
    popup: Option<Popup>,
    agents: Vec<Agent>,
}

pub fn run_xembed(interval: Duration, notify: bool) -> R<()> {
    let mut t = Tray::new()?;
    let mut sc = Scanner::new();
    let mut nt = Notifier::new(notify);
    t.agents = sc.scan();
    t.dock()?;
    if !t.docked {
        eprintln!("agentop: no system tray yet, waiting for one…");
    }
    let fd = t.conn.stream().as_raw_fd();
    let mut next = Instant::now() + interval;
    loop {
        while let Some(ev) = t.conn.poll_for_event()? {
            t.handle(ev)?;
        }
        t.conn.flush()?;
        let now = Instant::now();
        if now >= next {
            t.agents = sc.scan();
            nt.update(&t.agents);
            if !t.docked {
                t.dock()?;
            }
            t.draw_icon()?;
            if t.popup.is_some() {
                t.draw_popup()?;
            }
            t.conn.flush()?;
            next = Instant::now() + interval;
            continue;
        }
        let ms = (next - now).as_millis().clamp(1, 60_000) as i32;
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        unsafe { libc::poll(&mut pfd, 1, ms) };
    }
}

impl Tray {
    fn new() -> R<Self> {
        let (conn, screen) = x11rb::connect(None)?;
        let s = &conn.setup().roots[screen];
        let (root, root_visual, root_depth) = (s.root, s.root_visual, s.root_depth);
        let (sw, sh) = (s.width_in_pixels as i32, s.height_in_pixels as i32);
        let atoms = Atoms::new(&conn)?.reply()?;
        let tray_atom = conn
            .intern_atom(false, format!("_NET_SYSTEM_TRAY_S{screen}").as_bytes())?
            .reply()?
            .atom;
        conn.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
        )?;
        let fallback_bg = std::env::var("AGENTOP_TRAY_BG")
            .map(|s| util::rgb(&s))
            .unwrap_or(0x000000);
        Ok(Tray {
            conn,
            screen,
            root,
            root_visual,
            root_depth,
            sw,
            sh,
            atoms,
            tray_atom,
            win: 0,
            gc: 0,
            visual: 0,
            depth: 0,
            w: 22,
            h: 22,
            owner: NONE,
            docked: false,
            fallback_bg,
            font: TextFont::load(false),
            bold: TextFont::load(true),
            popup: None,
            agents: vec![],
        })
    }

    fn tray_visual(&self, owner: Window) -> R<(Visualid, u8)> {
        let r = self
            .conn
            .get_property(false, owner, self.atoms._NET_SYSTEM_TRAY_VISUAL, AtomEnum::VISUALID, 0, 1)?
            .reply()?;
        if let Some(vid) = r.value32().and_then(|mut v| v.next()) {
            for d in &self.conn.setup().roots[self.screen].allowed_depths {
                if d.visuals.iter().any(|v| v.visual_id == vid) {
                    return Ok((vid, d.depth));
                }
            }
        }
        Ok((self.root_visual, self.root_depth))
    }

    fn dock(&mut self) -> R<()> {
        let owner = self.conn.get_selection_owner(self.tray_atom)?.reply()?.owner;
        if owner == NONE {
            self.docked = false;
            return Ok(());
        }
        let (visual, depth) = self.tray_visual(owner)?;
        if self.win == 0 || visual != self.visual || depth != self.depth {
            self.create_icon(visual, depth)?;
        }
        self.conn.change_window_attributes(
            owner,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
        )?;
        let ev = ClientMessageEvent::new(
            32,
            owner,
            self.atoms._NET_SYSTEM_TRAY_OPCODE,
            [CURRENT_TIME, 0 /* REQUEST_DOCK */, self.win, 0, 0],
        );
        self.conn.send_event(false, owner, EventMask::NO_EVENT, ev)?;
        self.conn.flush()?;
        self.owner = owner;
        self.docked = true;
        Ok(())
    }

    fn create_icon(&mut self, visual: Visualid, depth: u8) -> R<()> {
        if self.win != 0 {
            self.conn.destroy_window(self.win)?;
        }
        let win = self.conn.generate_id()?;
        let mut aux = CreateWindowAux::new().event_mask(
            EventMask::EXPOSURE
                | EventMask::STRUCTURE_NOTIFY
                | EventMask::BUTTON_PRESS
                | EventMask::ENTER_WINDOW
                | EventMask::LEAVE_WINDOW,
        );
        if depth == 32 {
            let cmap = self.conn.generate_id()?;
            self.conn.create_colormap(ColormapAlloc::NONE, cmap, self.root, visual)?;
            aux = aux.colormap(cmap).background_pixel(0).border_pixel(0);
        } else {
            aux = aux.background_pixmap(BackPixmap::PARENT_RELATIVE);
        }
        self.conn.create_window(
            depth, win, self.root, 0, 0, self.w, self.h, 0,
            WindowClass::INPUT_OUTPUT, visual, &aux,
        )?;
        self.conn.change_property32(
            PropMode::REPLACE, win, self.atoms._XEMBED_INFO, self.atoms._XEMBED_INFO, &[0, 1],
        )?;
        self.conn.change_property8(
            PropMode::REPLACE, win, AtomEnum::WM_CLASS, AtomEnum::STRING, b"agentop\0agentop\0",
        )?;
        self.conn.change_property8(PropMode::REPLACE, win, AtomEnum::WM_NAME, AtomEnum::STRING, b"agentop")?;
        let gc = self.conn.generate_id()?;
        self.conn.create_gc(gc, win, &CreateGCAux::new().graphics_exposures(0))?;
        self.win = win;
        self.gc = gc;
        self.visual = visual;
        self.depth = depth;
        Ok(())
    }

    fn put(&self, win: Window, gc: Gcontext, depth: u8, cv: &Canvas) -> R<()> {
        let bytes = cv.bytes();
        let stride = cv.w * 4;
        let max = self.conn.maximum_request_bytes().saturating_sub(64);
        let rows = (max / stride).max(1);
        let mut y = 0;
        while y < cv.h {
            let n = rows.min(cv.h - y);
            self.conn.put_image(
                ImageFormat::Z_PIXMAP, win, gc, cv.w as u16, n as u16, 0, y as i16, 0, depth,
                &bytes[y * stride..(y + n) * stride],
            )?;
            y += n;
        }
        Ok(())
    }

    /// For non-ARGB trays: read back the parent-relative background so the
    /// icon blends into the bar.
    fn background(&self) -> Option<Canvas> {
        let (w, h) = (self.w as usize, self.h as usize);
        self.conn.clear_area(false, self.win, 0, 0, 0, 0).ok()?;
        let r = self
            .conn
            .get_image(ImageFormat::Z_PIXMAP, self.win, 0, 0, self.w, self.h, !0)
            .ok()?
            .reply()
            .ok()?;
        if r.data.len() < w * h * 4 {
            return None;
        }
        let px = r
            .data
            .chunks_exact(4)
            .take(w * h)
            .map(|b| 0xFF00_0000 | (b[2] as u32) << 16 | (b[1] as u32) << 8 | b[0] as u32)
            .collect();
        Some(Canvas { w, h, px })
    }

    fn draw_icon(&mut self) -> R<()> {
        if self.win == 0 || !self.docked {
            return Ok(());
        }
        let (w, h) = (self.w as usize, self.h as usize);
        let mut cv = if self.depth == 32 {
            Canvas::new(w, h, 0)
        } else {
            self.background()
                .unwrap_or_else(|| Canvas::new(w, h, 0xFF00_0000 | self.fallback_bg))
        };
        canvas::badge(&mut cv, &self.agents, self.bold.as_ref().or(self.font.as_ref()));
        self.put(self.win, self.gc, self.depth, &cv)
    }

    fn popup_rows(&self) -> Vec<PRow> {
        let (busy, wait, total) = scan::totals(&self.agents);
        let row = |segs: Vec<(String, u32)>| PRow { segs, dot: None, pid: None, bold: false };
        let mut rows = vec![PRow {
            segs: vec![
                ("󰚩 agentop".into(), ACCENT),
                (format!("   {busy} busy · {wait} waiting · {total} agents"), SOFT),
            ],
            dot: None,
            pid: None,
            bold: true,
        }];
        rows.push(row(vec![]));
        if self.agents.is_empty() {
            rows.push(row(vec![("no AI agents running".into(), DIM)]));
        }
        for a in &self.agents {
            let t = a.busy_for_s.map(util::fmt_dur).unwrap_or_else(|| util::fmt_dur(a.uptime_s));
            let act = a.activity.as_deref().unwrap_or("");
            rows.push(PRow {
                segs: vec![
                    (format!("{:<9}", a.kind.label()), util::rgb(a.kind.color())),
                    (format!("{:<22}", util::trunc(&a.name, 21)), FG),
                    (format!("{:<8}", a.status.label()), util::rgb(a.status.color())),
                    (format!("{t:>6}  "), DIM),
                    (util::trunc(act, 70), SOFT),
                ],
                dot: Some(util::rgb(a.status.color())),
                pid: Some(a.pid),
                bold: false,
            });
        }
        rows.push(row(vec![]));
        rows.push(row(vec![(
            "left-click: dashboard · right-click: pin · click a row: focus".into(),
            DIM,
        )]));
        rows
    }

    fn show_popup(&mut self, pinned: bool) -> R<()> {
        if let Some(p) = &mut self.popup {
            p.pinned = pinned;
            return self.draw_popup();
        }
        let win = self.conn.generate_id()?;
        self.conn.create_window(
            self.root_depth, win, self.root, 0, 0, 1, 1, 0, WindowClass::INPUT_OUTPUT, self.root_visual,
            &CreateWindowAux::new()
                .override_redirect(1)
                .background_pixel(BG)
                .event_mask(
                    EventMask::EXPOSURE
                        | EventMask::BUTTON_PRESS
                        | EventMask::ENTER_WINDOW
                        | EventMask::LEAVE_WINDOW,
                ),
        )?;
        let gc = self.conn.generate_id()?;
        self.conn.create_gc(gc, win, &CreateGCAux::new().graphics_exposures(0))?;
        self.popup = Some(Popup { win, gc, pinned, rows: vec![], y0: 0, lh: 1 });
        self.draw_popup()
    }

    fn hide_popup(&mut self) -> R<()> {
        if let Some(p) = self.popup.take() {
            self.conn.free_gc(p.gc)?;
            self.conn.destroy_window(p.win)?;
        }
        Ok(())
    }

    fn draw_popup(&mut self) -> R<()> {
        let Some(p) = &self.popup else { return Ok(()) };
        let (pwin, pgc) = (p.win, p.gc);
        let Some(font) = &self.font else { return Ok(()) };
        let bold = self.bold.as_ref().unwrap_or(font);
        let rows = self.popup_rows();
        let cw = font.advance(FONT_PX, 'M');
        let lh = (FONT_PX * 1.6).round() as i32;
        let pad = 12i32;
        let dotw = (cw * 2.0).round() as i32;
        let maxc = rows
            .iter()
            .map(|r| r.segs.iter().map(|s| s.0.chars().count()).sum::<usize>())
            .max()
            .unwrap_or(10);
        let w = (pad * 2 + dotw + (maxc as f32 * cw).ceil() as i32).min(self.sw - 8);
        let h = pad * 2 + rows.len() as i32 * lh;

        let pos = self.conn.translate_coordinates(self.win, self.root, 0, 0)?.reply()?;
        let (ix, iy) = (pos.dst_x as i32, pos.dst_y as i32);
        let x = (ix + self.w as i32 / 2 - w / 2).clamp(4, (self.sw - w - 4).max(4));
        let y = if iy < self.sh / 2 { iy + self.h as i32 + 8 } else { iy - h - 8 };
        self.conn.configure_window(
            pwin,
            &ConfigureWindowAux::new()
                .x(x)
                .y(y)
                .width(w as u32)
                .height(h as u32)
                .stack_mode(StackMode::ABOVE),
        )?;
        self.conn.map_window(pwin)?;

        let mut cv = Canvas::new(w as usize, h as usize, 0xFF00_0000 | BG);
        cv.fill_rect(0, 0, w, 1, BORDER);
        cv.fill_rect(0, h - 1, w, 1, BORDER);
        cv.fill_rect(0, 0, 1, h, BORDER);
        cv.fill_rect(w - 1, 0, 1, h, BORDER);
        cv.fill_rect(1, 1, w - 2, 2, ACCENT);
        for (i, r) in rows.iter().enumerate() {
            let top = pad + i as i32 * lh;
            let base = top as f32 + lh as f32 * 0.68;
            if let Some(c) = r.dot {
                cv.circle(pad as f32 + cw * 0.6, top as f32 + lh as f32 / 2.0, FONT_PX * 0.28, c);
            }
            let mut x = (pad + if r.dot.is_some() || i > 0 { dotw } else { 0 }) as f32;
            for (s, c) in &r.segs {
                x = cv.text(if r.bold { bold } else { font }, FONT_PX, x, base, s, *c);
            }
        }
        self.put(pwin, pgc, self.root_depth, &cv)?;
        if let Some(p) = &mut self.popup {
            p.rows = rows.iter().map(|r| r.pid).collect();
            p.y0 = pad;
            p.lh = lh;
        }
        Ok(())
    }

    fn popup_win(&self) -> Option<Window> {
        self.popup.as_ref().map(|p| p.win)
    }

    fn handle(&mut self, ev: Event) -> R<()> {
        match ev {
            Event::Expose(e) if e.count == 0 => {
                if e.window == self.win {
                    self.draw_icon()?;
                } else if Some(e.window) == self.popup_win() {
                    self.draw_popup()?;
                }
            }
            Event::ConfigureNotify(e) if e.window == self.win => {
                if (e.width, e.height) != (self.w, self.h) {
                    self.w = e.width.max(1);
                    self.h = e.height.max(1);
                    self.draw_icon()?;
                }
            }
            Event::ReparentNotify(e) if e.window == self.win && e.parent == self.root => {
                // tray went away and handed us back to the root window
                self.conn.unmap_window(self.win)?;
                self.docked = false;
            }
            Event::DestroyNotify(e) if self.owner != NONE && e.window == self.owner => {
                self.docked = false;
                self.owner = NONE;
                self.hide_popup()?;
            }
            Event::ClientMessage(e) if e.type_ == self.atoms.MANAGER => {
                if e.data.as_data32()[1] == self.tray_atom {
                    self.dock()?;
                }
            }
            Event::ButtonPress(e) if e.event == self.win => match e.detail {
                1 => {
                    self.hide_popup()?;
                    util::open_tui();
                }
                3 => {
                    if self.popup.as_ref().is_some_and(|p| p.pinned) {
                        self.hide_popup()?;
                    } else {
                        self.show_popup(true)?;
                    }
                }
                _ => {}
            },
            Event::ButtonPress(e) if Some(e.event) == self.popup_win() => {
                let p = self.popup.as_ref().unwrap();
                let row = (e.event_y as i32 - p.y0).div_euclid(p.lh.max(1));
                let pid = usize::try_from(row).ok().and_then(|r| p.rows.get(r).copied().flatten());
                self.hide_popup()?;
                if let Some(pid) = pid {
                    let _ = xutil::focus_pid(pid);
                }
            }
            Event::EnterNotify(e) if e.event == self.win => {
                if self.popup.is_none() {
                    self.show_popup(false)?;
                }
            }
            Event::LeaveNotify(e) if e.event == self.win => {
                if self.popup.as_ref().is_some_and(|p| !p.pinned) {
                    self.hide_popup()?;
                }
            }
            Event::LeaveNotify(e)
                if Some(e.event) == self.popup_win() && e.detail != NotifyDetail::INFERIOR =>
            {
                if self.popup.as_ref().is_some_and(|p| p.pinned) {
                    self.hide_popup()?;
                }
            }
            Event::Error(e) => eprintln!("agentop: X error {:?}", e.error_kind),
            _ => {}
        }
        Ok(())
    }
}
