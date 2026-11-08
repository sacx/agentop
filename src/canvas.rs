//! Tiny software rasterizer (premultiplied ARGB) for the tray icon and popup.

use ab_glyph::{point, Font, FontVec, PxScale, ScaleFont};
use std::process::Command;

pub struct TextFont {
    font: FontVec,
}

impl TextFont {
    /// Font from AGENTOP_FONT (path), else fontconfig.
    pub fn load(bold: bool) -> Option<Self> {
        let path = std::env::var("AGENTOP_FONT").ok().or_else(|| {
            let style = if bold { "Bold" } else { "Regular" };
            let pat = format!("JetBrainsMono Nerd Font Mono,JetBrains Mono,DejaVu Sans Mono,monospace:style={style}");
            let o = Command::new("fc-match").args(["-f", "%{file}", &pat]).output().ok()?;
            String::from_utf8(o.stdout).ok().filter(|s| !s.is_empty())
        })?;
        let data = std::fs::read(path).ok()?;
        Some(TextFont { font: FontVec::try_from_vec(data).ok()? })
    }

    pub fn advance(&self, px: f32, c: char) -> f32 {
        self.font.as_scaled(PxScale::from(px)).h_advance(self.font.glyph_id(c))
    }

    pub fn width(&self, px: f32, s: &str) -> f32 {
        s.chars().map(|c| self.advance(px, c)).sum()
    }
}

pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u32>,
}

impl Canvas {
    pub fn new(w: usize, h: usize, argb: u32) -> Self {
        Canvas { w, h, px: vec![argb; w * h] }
    }

    #[inline]
    pub fn blend(&mut self, x: i32, y: i32, rgb: u32, a: f32) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h || a <= 0.0 {
            return;
        }
        let a = a.min(1.0);
        let i = y as usize * self.w + x as usize;
        let d = self.px[i];
        let inv = 1.0 - a;
        let ch = |v: u32, sh: u32| ((v >> sh) & 0xFF) as f32;
        let oa = 255.0 * a + ch(d, 24) * inv;
        let or = ch(rgb, 16) * a + ch(d, 16) * inv;
        let og = ch(rgb, 8) * a + ch(d, 8) * inv;
        let ob = ch(rgb, 0) * a + ch(d, 0) * inv;
        self.px[i] = ((oa.round() as u32) << 24)
            | ((or.round() as u32) << 16)
            | ((og.round() as u32) << 8)
            | ob.round() as u32;
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, rgb: u32) {
        for yy in y..y + h {
            for xx in x..x + w {
                self.blend(xx, yy, rgb, 1.0);
            }
        }
    }

    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, rgb: u32) {
        self.shape(cx, cy, r, |d| (r - d + 0.5).clamp(0.0, 1.0), rgb);
    }

    pub fn ring(&mut self, cx: f32, cy: f32, r: f32, width: f32, rgb: u32) {
        self.shape(cx, cy, r, |d| (r - d + 0.5).min(d - (r - width) + 0.5).clamp(0.0, 1.0), rgb);
    }

    fn shape(&mut self, cx: f32, cy: f32, r: f32, cov: impl Fn(f32) -> f32, rgb: u32) {
        let (x0, x1) = ((cx - r - 1.0).floor() as i32, (cx + r + 1.0).ceil() as i32);
        let (y0, y1) = ((cy - r - 1.0).floor() as i32, (cy + r + 1.0).ceil() as i32);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                self.blend(x, y, rgb, cov((dx * dx + dy * dy).sqrt()));
            }
        }
    }

    /// Draws text with its baseline at `y`; returns the end x.
    pub fn text(&mut self, f: &TextFont, px: f32, mut x: f32, y: f32, s: &str, rgb: u32) -> f32 {
        let scale = PxScale::from(px);
        let sf = f.font.as_scaled(scale);
        for c in s.chars() {
            let id = f.font.glyph_id(c);
            let g = id.with_scale_and_position(scale, point(x, y));
            if let Some(og) = f.font.outline_glyph(g) {
                let b = og.px_bounds();
                og.draw(|gx, gy, cov| {
                    self.blend(b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32, rgb, cov)
                });
            }
            x += sf.h_advance(id);
        }
        x
    }

    /// Little-endian 32bpp (BGRA / BGRX) for X11 ZPixmap.
    pub fn bytes(&self) -> Vec<u8> {
        self.px.iter().flat_map(|p| p.to_le_bytes()).collect()
    }
}

/// The tray badge: filled circle with the number of busy/waiting agents, or a
/// ring with the total when everything is idle.
pub fn badge(cv: &mut Canvas, agents: &[crate::scan::Agent], font: Option<&TextFont>) {
    use crate::scan::{totals, Status};
    let (busy, wait, total) = totals(agents);
    let color = crate::util::rgb(if wait > 0 {
        Status::Waiting.color()
    } else if busy > 0 {
        Status::Busy.color()
    } else if total > 0 {
        "#9CA3AF"
    } else {
        "#4B5563"
    });
    let (w, h) = (cv.w as f32, cv.h as f32);
    let s = w.min(h);
    let (cx, cy, r) = (w / 2.0, h / 2.0, s * 0.45);
    let active = busy + wait;
    let (label, text_color) = if active > 0 {
        cv.circle(cx, cy, r, color);
        (active.to_string(), 0x0B0F14)
    } else {
        cv.ring(cx, cy, r, (s * 0.1).max(1.5), color);
        (if total > 0 { total.to_string() } else { String::new() }, color)
    };
    if let Some(f) = font {
        let px = if label.len() > 1 { s * 0.5 } else { s * 0.62 };
        let tw = f.width(px, &label);
        cv.text(f, px, cx - tw / 2.0, cy + px * 0.36, &label, text_color);
    }
}

impl Canvas {
    /// Straight (non-premultiplied) ARGB32, big-endian — the SNI pixmap format.
    pub fn argb_be(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.px.len() * 4);
        for &p in &self.px {
            let a = p >> 24;
            let un = |c: u32| (c * 255 + a / 2).checked_div(a).unwrap_or(0).min(255) as u8;
            out.extend_from_slice(&[a as u8, un((p >> 16) & 255), un((p >> 8) & 255), un(p & 255)]);
        }
        out
    }
}
