//! 时间线：记忆发生过什么。时间由系统写入，模型改不了。
//!
//! 时间存在写入日志的每个条目里，重放时用它，所以重启后时间不变，也不会出现日志和时间线不一致。
//! 导出时日志被合并，时间线写进 `timeline.jsonl`，加载时先读它，再重放日志。

use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::durable;
use crate::error::Error;

pub const FILE_NAME: &str = "timeline.jsonl";

/// 一件发生过的事。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Unix 秒。旧日志没有记录时间，这里是 `None`。
    #[serde(default)]
    pub at: Option<u64>,
    pub text: String,
    /// 这件事改动了哪些节点。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<String>,
    #[serde(default)]
    pub action: Action,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    #[default]
    Change,
    /// `nodes` 的第一个节点是这时新建的。
    Create,
    /// `nodes` 的第一个节点是这时删除的。
    Remove,
}

/// 节点的时间。和反向列表一样由系统维护，模型不能写。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NodeTimes {
    pub created: Option<u64>,
    pub updated: Option<u64>,
}

pub fn now() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_secs())
}

/// 显示成 `2026-10-31T11:52Z`。没有时间时显示“未知”。
pub fn format_time(at: Option<u64>) -> String {
    let Some(at) = at else {
        return "未知".to_owned();
    };
    let days = i64::try_from(at / 86400).unwrap_or(i64::MAX / 2);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}Z",
        at % 86400 / 3600,
        at % 3600 / 60
    )
}

/// 从 1970-01-01 起的天数换算成日期。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (
        year,
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

/// 读出来的时间线。
pub struct Loaded {
    pub events: Vec<Event>,
    /// 损坏被跳过的行数。
    pub skipped: usize,
}

/// 读取知识库的时间线。时间线只是历史，坏了不影响记忆，所以损坏的部分跳过，不报错。
pub fn read(knowledge_base: &Path) -> Loaded {
    let path = knowledge_base.join(FILE_NAME);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == ErrorKind::NotFound => Vec::new(),
        Err(_) => {
            return Loaded {
                events: Vec::new(),
                skipped: 1,
            };
        }
    };
    let mut events = Vec::new();
    let mut skipped = 0;
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice(line) {
            Ok(event) => events.push(event),
            Err(_) => skipped += 1,
        }
    }
    Loaded { events, skipped }
}

/// 写出时间线。没有内容时不写文件。
pub fn write(events: &[Event], dir: &Path) -> Result<(), Error> {
    if events.is_empty() {
        return Ok(());
    }
    let path = dir.join(FILE_NAME);
    let mut text = String::new();
    for event in events {
        text.push_str(&serde_json::to_string(event).expect("事件总是可以序列化"));
        text.push('\n');
    }
    durable::write_private(&path, text.as_bytes())
        .map_err(|e| Error::Save(format!("{}：{e}", path.display())))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn formats_times_in_utc() {
        assert_eq!(format_time(Some(0)), "1970-01-01T00:00Z");
        assert_eq!(format_time(Some(951_782_400)), "2000-02-29T00:00Z");
        assert_eq!(format_time(Some(1_793_447_520)), "2026-10-31T11:52Z");
        assert_eq!(format_time(None), "未知");
    }

    #[test]
    fn reads_back_what_it_writes() {
        let dir = TempDir::new().unwrap();
        assert!(read(dir.path()).events.is_empty());
        write(&[], dir.path()).unwrap();
        assert!(!dir.path().join(FILE_NAME).exists());

        let events = vec![
            Event {
                at: Some(5),
                text: "add a".into(),
                nodes: vec!["a".into()],
                action: Action::Create,
            },
            Event {
                at: None,
                text: "log 换行\n也可以".into(),
                nodes: Vec::new(),
                action: Action::Change,
            },
        ];
        write(&events, dir.path()).unwrap();
        assert_eq!(read(dir.path()).events, events);
    }

    #[test]
    fn damaged_lines_are_skipped() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(FILE_NAME),
            b"not json\n{\"at\":1,\"text\":\"ok\"}\n\xE4\xB8\n",
        )
        .unwrap();

        let loaded = read(dir.path());

        assert_eq!(loaded.skipped, 2);
        assert_eq!(loaded.events[0].text, "ok");
    }
}
