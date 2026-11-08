//! Desktop notifications when an agent finishes or starts waiting for you.

use crate::scan::{Agent, Status};
use crate::util;
use std::collections::HashMap;

pub struct Notifier {
    enabled: bool,
    first: bool,
    prev: HashMap<u32, (Status, u64)>,
}

/// Minimum busy time before a "finished" notification is worth sending.
const MIN_BUSY_S: u64 = 15;

impl Notifier {
    pub fn new(enabled: bool) -> Self {
        Notifier { enabled, first: true, prev: HashMap::new() }
    }

    pub fn update(&mut self, agents: &[Agent]) {
        if self.enabled && !self.first {
            for a in agents {
                let Some(&(ps, busy_s)) = self.prev.get(&a.pid) else { continue };
                if ps == a.status {
                    continue;
                }
                let what = a.activity.as_deref().unwrap_or("");
                match (ps, a.status) {
                    (Status::Busy, Status::Idle) if busy_s >= MIN_BUSY_S => util::notify(
                        &format!("{} finished · {}", a.kind.label(), a.name),
                        &format!("after {} — {}", util::fmt_dur(busy_s), util::trunc(what, 160)),
                        false,
                    ),
                    (_, Status::Waiting) => util::notify(
                        &format!("{} needs you · {}", a.kind.label(), a.name),
                        &util::trunc(what, 160),
                        true,
                    ),
                    _ => {}
                }
            }
        }
        self.first = false;
        self.prev = agents
            .iter()
            .map(|a| (a.pid, (a.status, a.busy_for_s.unwrap_or(0))))
            .collect();
    }
}
