//! Persistent progress + import of VLC's own "recent media" history.

use crate::config::{state_file, vlc_history};
use crate::library::folder_hint;
use crate::parse::parse;
use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Past this fraction of the runtime an episode counts as watched.
pub const WATCHED_AT: f64 = 0.90;

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Rec {
    #[serde(default)]
    pub watched: bool,
    /// Resume position in milliseconds (0 = start / finished).
    #[serde(default)]
    pub pos: u64,
    /// Unix time of the last change; the most recent one decides "up next".
    #[serde(default)]
    pub updated: f64,
}

#[derive(Serialize, Deserialize, Default)]
pub struct State {
    #[serde(default)]
    pub progress: HashMap<String, Rec>,
    /// "path|size" -> duration in ms (ffprobe is slow-ish, so cache it).
    #[serde(default)]
    pub durations: HashMap<String, u64>,
    /// VLC history as of the last sync, to tell what was played since.
    #[serde(default)]
    pub vlc_snapshot: Option<Vec<(String, i64)>>,
}

/// Stopped past 90%, or within the last 3 minutes (credits) of anything longer than 10 minutes.
pub fn is_finished(pos: u64, dur: u64) -> bool {
    dur > 0 && (pos as f64 >= WATCHED_AT * dur as f64 || (dur > 600_000 && dur.saturating_sub(pos) <= 180_000))
}

pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

impl State {
    pub fn load() -> State {
        std::fs::read_to_string(state_file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let path = state_file();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = path.with_extension("json.tmp");
        if let Ok(txt) = serde_json::to_string_pretty(self) {
            if std::fs::write(&tmp, txt).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }

    /// Pull in what VLC played since the last sync. Returns true if anything changed.
    /// VLC stores a resume time per file, and 0 once a file is finished.
    pub fn sync_vlc(&mut self, durations: &HashMap<String, u64>) -> bool {
        let Some(new) = read_vlc() else { return false };
        if self.vlc_snapshot.as_ref() == Some(&new) {
            return false;
        }
        let first_run = self.vlc_snapshot.is_none();
        let old: HashMap<&str, (usize, i64)> = self
            .vlc_snapshot
            .iter()
            .flatten()
            .enumerate()
            .map(|(i, (u, t))| (u.as_str(), (i, *t)))
            .collect();
        let t_now = now();
        for (i, (uri, time)) in new.iter().enumerate().rev() {
            if !first_run {
                if let Some(&(oi, ot)) = old.get(uri.as_str()) {
                    if ot == *time && i >= oi {
                        continue; // untouched, just pushed down the list
                    }
                }
            }
            let path = uri_to_path(uri);
            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let Some(info) = parse(&name, &folder_hint(&path)) else { continue };
            let key = info.pkey();
            let dur = durations.get(&key).copied().unwrap_or(0);
            let rec = self.progress.entry(key).or_default();
            if *time <= 0 || is_finished(*time as u64, dur) {
                rec.watched = true;
                rec.pos = 0;
            } else {
                rec.watched = false;
                rec.pos = *time as u64;
            }
            rec.updated = t_now - i as f64 * if first_run { 60.0 } else { 1.0 };
        }
        self.vlc_snapshot = Some(new);
        true
    }
}

fn uri_to_path(uri: &str) -> std::path::PathBuf {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    Path::new(&*percent_decode_str(raw).decode_utf8_lossy()).to_path_buf()
}

/// Qt writes string lists as `a, b, "c, with comma"`.
fn split_qt_list(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted, mut esc) = (vec![], String::new(), false, false);
    for ch in s.chars() {
        match ch {
            _ if esc => {
                cur.push(ch);
                esc = false;
            }
            '\\' if quoted => esc = true,
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur).trim().to_string()),
            _ => cur.push(ch),
        }
    }
    out.push(cur.trim().to_string());
    out
}

/// (uri, resume time in ms), most recent first.
pub fn read_vlc() -> Option<Vec<(String, i64)>> {
    let txt = std::fs::read_to_string(vlc_history()).ok()?;
    let (mut in_sec, mut list, mut times) = (false, String::new(), String::new());
    for line in txt.lines() {
        if line.starts_with('[') {
            in_sec = line.trim() == "[RecentsMRL]";
        } else if in_sec {
            if let Some(v) = line.strip_prefix("list=") {
                list = v.to_string();
            } else if let Some(v) = line.strip_prefix("times=") {
                times = v.to_string();
            }
        }
    }
    let times: Vec<i64> = split_qt_list(&times).iter().filter_map(|t| t.parse().ok()).collect();
    Some(
        split_qt_list(&list)
            .into_iter()
            .enumerate()
            .filter(|(_, u)| u.starts_with("file://"))
            .map(|(i, u)| (u, times.get(i).copied().unwrap_or(0)))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qt_list() {
        assert_eq!(split_qt_list(r#"file:///a.mkv, "file:///b, c.mkv", file:///d.mkv"#), vec![
            "file:///a.mkv",
            "file:///b, c.mkv",
            "file:///d.mkv"
        ]);
    }

    #[test]
    fn uri_decode() {
        assert_eq!(
            uri_to_path("file:///x/%5B%CE%9BV%5D%20Ep-09.mkv"),
            std::path::PathBuf::from("/x/[ΛV] Ep-09.mkv")
        );
    }
}
