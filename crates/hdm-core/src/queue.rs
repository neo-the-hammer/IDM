//! Download queues, their schedules, and what happens when they drain.
//!
//! This is the automation half of a download manager: queue overnight, run at
//! a cheaper hour, cap how many transfer at once, and shut the machine down
//! afterwards. IDM's users lean on it heavily, so it is modelled properly
//! rather than approximated with a single global concurrency number.

use crate::platform::{LocalTime, PowerAction};
use hdm_json::{json, Json};

/// What to do when every download in a queue has finished.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Completion {
    #[default]
    Nothing,
    Shutdown,
    Sleep,
    Hibernate,
    /// Stop the daemon.
    Exit,
    /// Run a program. The finished folder is appended as an argument.
    Run(String),
}

impl Completion {
    pub fn to_json(&self) -> Json {
        match self {
            Completion::Nothing => json!({"kind": "nothing"}),
            Completion::Shutdown => json!({"kind": "shutdown"}),
            Completion::Sleep => json!({"kind": "sleep"}),
            Completion::Hibernate => json!({"kind": "hibernate"}),
            Completion::Exit => json!({"kind": "exit"}),
            Completion::Run(command) => json!({"kind": "run", "command": (command.as_str())}),
        }
    }

    pub fn from_json(value: &Json) -> Completion {
        match value.str_or("kind", "nothing") {
            "shutdown" => Completion::Shutdown,
            "sleep" => Completion::Sleep,
            "hibernate" => Completion::Hibernate,
            "exit" => Completion::Exit,
            "run" => Completion::Run(value.str_or("command", "").to_string()),
            _ => Completion::Nothing,
        }
    }

    pub fn power_action(&self) -> Option<PowerAction> {
        match self {
            Completion::Shutdown => Some(PowerAction::Shutdown),
            Completion::Sleep => Some(PowerAction::Sleep),
            Completion::Hibernate => Some(PowerAction::Hibernate),
            _ => None,
        }
    }
}

/// Bit positions for [`Schedule::days`]; bit 0 is Sunday.
pub const EVERY_DAY: u8 = 0b0111_1111;
pub const WEEKDAYS: u8 = 0b0011_1110;
pub const WEEKENDS: u8 = 0b0100_0001;

/// When a queue is allowed to run.
///
/// Times are minutes since local midnight. Local rather than UTC because
/// "start at 2am" means two in the morning where the user is, and should keep
/// meaning that across a daylight-saving change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    pub enabled: bool,
    pub start: u16,
    /// When the window closes. `None` means it never does, so the queue runs
    /// until it is empty.
    pub stop: Option<u16>,
    /// Bitmask of days the window applies to.
    pub days: u8,
}

impl Default for Schedule {
    fn default() -> Self {
        Schedule {
            enabled: false,
            start: 0,
            stop: None,
            days: EVERY_DAY,
        }
    }
}

impl Schedule {
    /// Whether the window is open at `now`.
    ///
    /// A window whose stop time is earlier than its start time wraps past
    /// midnight — "23:00 to 06:00" is the obvious way to express an overnight
    /// queue, and refusing it would be a poor reading of the user's intent.
    pub fn is_open(&self, now: LocalTime) -> bool {
        if !self.enabled {
            return true;
        }
        if self.days & (1 << now.weekday) == 0 {
            // The window may still be open from yesterday if it wraps midnight.
            let yesterday = (now.weekday + 6) % 7;
            let wraps = self.stop.map(|stop| stop < self.start).unwrap_or(false);
            if !(wraps && self.days & (1 << yesterday) != 0) {
                return false;
            }
        }

        let minute = now.minutes();
        match self.stop {
            None => minute >= self.start,
            Some(stop) if stop > self.start => minute >= self.start && minute < stop,
            // Wraps past midnight.
            Some(stop) => minute >= self.start || minute < stop,
        }
    }

    pub fn to_json(&self) -> Json {
        json!({
            "enabled": (self.enabled),
            "start": (self.start),
            "stop": (self.stop),
            "days": (self.days)
        })
    }

    pub fn from_json(value: &Json) -> Schedule {
        Schedule {
            enabled: value.bool_or("enabled", false),
            start: value.u64_or("start", 0).min(1439) as u16,
            stop: value
                .get("stop")
                .and_then(Json::as_u64)
                .map(|s| s.min(1439) as u16),
            days: (value.u64_or("days", EVERY_DAY as u64) as u8) & EVERY_DAY,
        }
    }
}

/// A named group of downloads with its own limits and schedule.
#[derive(Debug, Clone, PartialEq)]
pub struct Queue {
    pub id: String,
    pub name: String,
    /// How many of this queue's downloads may transfer at once.
    pub concurrency: u8,
    /// Bytes per second across this queue; 0 means only the global limit applies.
    pub speed_limit: u64,
    pub schedule: Schedule,
    pub completion: Completion,
    /// A paused queue starts nothing, whatever its schedule says.
    pub paused: bool,
}

impl Queue {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Queue {
        Queue {
            id: id.into(),
            name: name.into(),
            concurrency: 2,
            speed_limit: 0,
            schedule: Schedule::default(),
            completion: Completion::Nothing,
            paused: false,
        }
    }

    /// Whether this queue may start work now.
    pub fn is_runnable(&self, now: LocalTime) -> bool {
        !self.paused && self.schedule.is_open(now)
    }

    pub fn to_json(&self) -> Json {
        json!({
            "id": (self.id.as_str()),
            "name": (self.name.as_str()),
            "concurrency": (self.concurrency),
            "speedLimit": (self.speed_limit),
            "schedule": (self.schedule.to_json()),
            "completion": (self.completion.to_json()),
            "paused": (self.paused)
        })
    }

    pub fn from_json(value: &Json) -> Option<Queue> {
        Some(Queue {
            id: value.get("id")?.as_str()?.to_string(),
            name: value.str_or("name", "Queue").to_string(),
            concurrency: (value.u64_or("concurrency", 2) as u8).clamp(1, 32),
            speed_limit: value.u64_or("speedLimit", 0),
            schedule: value
                .get("schedule")
                .map(Schedule::from_json)
                .unwrap_or_default(),
            completion: value
                .get("completion")
                .map(Completion::from_json)
                .unwrap_or_default(),
            paused: value.bool_or("paused", false),
        })
    }
}

/// The queue every download belongs to unless it is put somewhere else.
pub const MAIN_QUEUE: &str = "main";

pub fn default_queues() -> Vec<Queue> {
    let mut main = Queue::new(MAIN_QUEUE, "Main");
    main.concurrency = 4;
    let mut overnight = Queue::new("overnight", "Overnight");
    overnight.concurrency = 8;
    overnight.schedule = Schedule {
        enabled: true,
        // 01:00 to 07:00, the window people actually pick for a big queue.
        start: 60,
        stop: Some(7 * 60),
        days: EVERY_DAY,
    };
    vec![main, overnight]
}
