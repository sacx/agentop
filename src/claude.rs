//! Claude Code specifics: ~/.claude/sessions/<pid>.json gives the live
//! status and session id; the session transcript (jsonl) gives the last
//! tool call or message.

use crate::scan::Agent;
use crate::util;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub struct Session {
    pub session_id: String,
    pub cwd: String,
    pub name: Option<String>,
    pub status: Option<String>,
    /// Unix ms of the last status change.
    pub status_since_ms: Option<u64>,
}

pub fn session(home: &Path, pid: u32) -> Option<Session> {
    let s = fs::read_to_string(home.join(format!(".claude/sessions/{pid}.json"))).ok()?;
    let v: Value = serde_json::from_str(&s).ok()?;
    if v.get("pid").and_then(Value::as_u64) != Some(pid as u64) {
        return None;
    }
    let st = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(Session {
        session_id: st("sessionId")?,
        cwd: st("cwd").unwrap_or_default(),
        name: st("name"),
        status: st("status"),
        status_since_ms: v.get("statusUpdatedAt").and_then(Value::as_u64),
    })
}

struct Entry {
    path: PathBuf,
    mtime: SystemTime,
    len: u64,
    activity: Option<String>,
}

#[derive(Default)]
pub struct Cache {
    map: HashMap<String, Entry>,
}

impl Cache {
    pub fn activity(&mut self, home: &Path, s: &Session) -> (Option<String>, Option<u64>) {
        let path = match self.map.get(&s.session_id) {
            Some(e) => e.path.clone(),
            None => match locate(home, s) {
                Some(p) => p,
                None => return (None, None),
            },
        };
        let Ok(meta) = fs::metadata(&path) else {
            self.map.remove(&s.session_id);
            return (None, None);
        };
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let age = SystemTime::now().duration_since(mtime).map(|d| d.as_secs()).ok();
        if let Some(e) = self.map.get(&s.session_id) {
            if e.mtime == mtime && e.len == meta.len() {
                return (e.activity.clone(), age);
            }
        }
        let activity = parse_tail(&path, meta.len());
        self.map.insert(
            s.session_id.clone(),
            Entry { path, mtime, len: meta.len(), activity: activity.clone() },
        );
        (activity, age)
    }

    pub fn retain(&mut self, agents: &[Agent]) {
        self.map
            .retain(|sid, _| agents.iter().any(|a| a.session_id.as_deref() == Some(sid)));
    }
}

fn encode(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn locate(home: &Path, s: &Session) -> Option<PathBuf> {
    let projects = home.join(".claude/projects");
    let file = format!("{}.jsonl", s.session_id);
    let p = projects.join(encode(&s.cwd)).join(&file);
    if p.exists() {
        return Some(p);
    }
    fs::read_dir(&projects)
        .ok()?
        .flatten()
        .map(|d| d.path().join(&file))
        .find(|p| p.exists())
}

const TAIL: u64 = 512 * 1024;

fn parse_tail(path: &Path, len: u64) -> Option<String> {
    let mut f = File::open(path).ok()?;
    let start = len.saturating_sub(TAIL);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.take(TAIL).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0); // partial line
    }
    for line in lines.iter().rev() {
        if !line.contains("\"type\":\"assistant\"") && !line.contains("\"type\":\"user\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v.get("isMeta").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let content = &v["message"]["content"];
        match v["type"].as_str() {
            Some("assistant") => {
                let Some(items) = content.as_array() else { continue };
                for it in items.iter().rev() {
                    match it["type"].as_str() {
                        Some("tool_use") => {
                            return Some(fmt_tool(it["name"].as_str().unwrap_or("tool"), &it["input"]))
                        }
                        Some("text") => {
                            let t = util::one_line(it["text"].as_str().unwrap_or(""));
                            if !t.is_empty() {
                                return Some(format!("“{}”", util::trunc(&t, 140)));
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("user") => {
                let prompt = match content {
                    Value::String(s) => Some(s.as_str()),
                    Value::Array(items) => items
                        .iter()
                        .find(|i| i["type"] == "text")
                        .and_then(|i| i["text"].as_str()),
                    _ => None,
                };
                if let Some(p) = prompt {
                    let p = util::one_line(p);
                    if !p.is_empty() && !p.starts_with('<') && !p.starts_with('[') {
                        return Some(format!("» {}", util::trunc(&p, 140)));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn fmt_tool(name: &str, input: &Value) -> String {
    let name = if let Some(rest) = name.strip_prefix("mcp__") {
        let parts: Vec<&str> = rest.split("__").collect();
        let server = parts.first().copied().unwrap_or("").trim_start_matches("claude_ai_");
        format!("{}.{}", server, parts.get(1).copied().unwrap_or(""))
    } else {
        name.to_string()
    };
    const KEYS: [&str; 13] = [
        "command", "file_path", "notebook_path", "path", "pattern", "url", "query", "skill",
        "description", "subject", "prompt", "intent", "action",
    ];
    let arg = KEYS
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .map(|s| util::trunc(&util::one_line(&util::tilde(s)), 120));
    match arg {
        Some(a) => format!("{name} {a}"),
        None => name,
    }
}
