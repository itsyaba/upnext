//! Scanning folders, grouping files into shows, and working out "up next".

use crate::config::cfg;
use crate::parse::{parse, Kind, Parsed};
use crate::state::State;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::UNIX_EPOCH;

const VIDEO_EXT: &[&str] = &["mkv", "mp4", "avi", "webm", "mov", "m4v", "ts"];
/// Loose videos smaller than this are chat clips, not movies.
const MIN_MOVIE_BYTES: u64 = 150 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct VFile {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: f64,
    pub dur: u64,
    pub info: Parsed,
    pub pkey: String,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub pkey: String,
    pub season: u32,
    pub ep: u32,
    pub title: String,
    /// Library copies first. Empty = a gap in the season ("not downloaded").
    pub files: Vec<VFile>,
    pub watched: bool,
    pub pos: u64,
    pub updated: f64,
}

impl Entry {
    pub fn file(&self) -> Option<&VFile> {
        self.files.first()
    }
    pub fn dur(&self) -> u64 {
        self.file().map(|f| f.dur).unwrap_or(0)
    }
    pub fn in_progress(&self) -> bool {
        self.pos > 0 && !self.watched
    }
    pub fn code(&self) -> String {
        format!("S{:02}E{:02}", self.season, self.ep)
    }
    pub fn label(&self) -> String {
        if self.title.is_empty() {
            self.code()
        } else {
            format!("{} · {}", self.code(), self.title)
        }
    }
    pub fn status(&self) -> String {
        if self.file().is_none() {
            return "Not downloaded".into();
        }
        if self.watched {
            return "Watched".into();
        }
        let dur = self.dur();
        if self.pos > 0 {
            let of = if dur > 0 { format!(" of {}", hms(dur)) } else { String::new() };
            return format!("Stopped at {}{of}", hms(self.pos));
        }
        if dur > 0 {
            format!("Not started · {}", hms(dur))
        } else {
            "Not started".into()
        }
    }
}

pub struct Show {
    pub key: String,
    pub name: String,
    pub list: Vec<Entry>,
    pub next: Option<usize>,
    pub last: f64,
    pub watched: usize,
    pub location: String,
    pub target: PathBuf,
    /// Files not yet in the show's library folder.
    pub strays: Vec<VFile>,
}

pub struct Movie {
    pub name: String,
    pub entry: Entry,
    pub location: String,
    pub strays: Vec<VFile>,
}

pub struct Model {
    pub watching: Vec<Show>,
    pub finished: Vec<Show>,
    pub movies: Vec<Movie>,
}

pub fn hms(ms: u64) -> String {
    let s = ms / 1000;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Parent folder name, unless the file sits directly in a scanned root.
pub fn folder_hint(p: &Path) -> String {
    let Some(parent) = p.parent() else { return String::new() };
    if cfg().sources().contains(&parent) {
        return String::new();
    }
    parent.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

fn probe(path: &Path) -> u64 {
    Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
        .arg(path)
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<f64>().ok())
        .map(|s| (s * 1000.0) as u64)
        .unwrap_or(0)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        match e.file_type() {
            Ok(t) if t.is_dir() => walk(&p, out),
            Ok(t) if t.is_file() => out.push(p),
            _ => {}
        }
    }
}

/// Walk the configured folders. `cache` maps "path|size" to duration and is filled in.
pub fn scan(cache: &mut HashMap<String, u64>) -> Vec<VFile> {
    let mut paths = vec![];
    for root in cfg().sources() {
        walk(root, &mut paths);
    }
    let mut out = vec![];
    for p in paths {
        let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if !VIDEO_EXT.contains(&ext.as_str()) {
            continue;
        }
        let Ok(meta) = p.metadata() else { continue };
        let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
        let Some(info) = parse(&name, &folder_hint(&p)) else { continue };
        if info.kind == Kind::Movie && meta.len() < MIN_MOVIE_BYTES {
            continue;
        }
        let ck = format!("{}|{}", p.display(), meta.len());
        let dur = match cache.get(&ck) {
            Some(&d) if d > 0 => d,
            _ => {
                let d = probe(&p);
                cache.insert(ck, d);
                d
            }
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        out.push(VFile { pkey: info.pkey(), path: p, size: meta.len(), mtime, dur, info });
    }
    out
}

fn location(files: &[VFile]) -> String {
    let dl = files.iter().any(|f| cfg().in_downloads(&f.path));
    let lib = files.iter().any(|f| !cfg().in_downloads(&f.path));
    match (dl, lib) {
        (true, true) => "Downloads + Library",
        (true, false) => "Downloads",
        _ => "Library",
    }
    .into()
}

/// The library folder that already holds most of this show, or a new one named after it.
fn target_dir(files: &[VFile], name: &str) -> PathBuf {
    let lib = &cfg().library;
    let mut counts: HashMap<&Path, usize> = HashMap::new();
    for f in files {
        if let Some(parent) = f.path.parent() {
            if parent.starts_with(lib) && parent != lib && !cfg().in_downloads(&f.path) {
                *counts.entry(parent).or_default() += 1;
            }
        }
    }
    if let Some((dir, _)) = counts.into_iter().max_by_key(|(d, n)| (*n, std::cmp::Reverse(d.to_path_buf()))) {
        return dir.to_path_buf();
    }
    let safe: String = name.chars().filter(|c| !r#"/\:*?"<>|"#.contains(*c)).collect();
    lib.join(safe.trim())
}

fn entry(pkey: String, season: u32, ep: u32, title: String, mut files: Vec<VFile>, st: &State) -> Entry {
    files.sort_by_key(|f| cfg().in_downloads(&f.path));
    let rec = st.progress.get(&pkey).cloned().unwrap_or_default();
    let dur = files.first().map(|f| f.dur).unwrap_or(0);
    let watched = rec.watched || (rec.pos > 0 && crate::state::is_finished(rec.pos, dur));
    Entry { pkey, season, ep, title, files, watched, pos: if watched { 0 } else { rec.pos }, updated: rec.updated }
}

pub fn build(files: &[VFile], st: &State) -> Model {
    struct Acc {
        names: HashMap<String, usize>,
        eps: BTreeMap<(u32, u32), (String, Vec<VFile>, String)>,
    }
    let mut shows: HashMap<String, Acc> = HashMap::new();
    let mut movies: HashMap<String, (String, String, Vec<VFile>)> = HashMap::new();
    let mut seen = HashSet::new();

    for f in files {
        if !seen.insert(&f.path) {
            continue;
        }
        match &f.info.kind {
            Kind::Episode { season, ep, title } => {
                let acc = shows
                    .entry(f.info.key.clone())
                    .or_insert_with(|| Acc { names: HashMap::new(), eps: BTreeMap::new() });
                *acc.names.entry(f.info.show.clone()).or_default() += 1;
                let e = acc.eps.entry((*season, *ep)).or_insert_with(|| (String::new(), vec![], f.pkey.clone()));
                if e.0.is_empty() {
                    e.0 = title.clone();
                }
                e.1.push(f.clone());
            }
            Kind::Movie => {
                movies
                    .entry(f.info.key.clone())
                    .or_insert_with(|| (f.info.show.clone(), f.pkey.clone(), vec![]))
                    .2
                    .push(f.clone());
            }
        }
    }

    let mut all = vec![];
    for (key, mut acc) in shows {
        let name = acc.names.iter().max_by_key(|(n, c)| (**c, std::cmp::Reverse((*n).clone()))).unwrap().0.clone();
        // Fill gaps in each season so missing episodes show up.
        let seasons: HashSet<u32> = acc.eps.keys().map(|k| k.0).filter(|s| *s > 0).collect();
        for s in seasons {
            let top = acc.eps.keys().filter(|k| k.0 == s).map(|k| k.1).max().unwrap_or(0);
            if top <= 60 {
                for n in 1..top {
                    acc.eps.entry((s, n)).or_insert_with(|| (String::new(), vec![], format!("{key}|S{s:02}E{n:03}")));
                }
            }
        }
        let mut list: Vec<Entry> =
            acc.eps.into_iter().map(|((s, e), (title, files, pk))| entry(pk, s, e, title, files, st)).collect();
        // You don't start a show at episode 9: anything before the furthest watched
        // episode that has no record of its own counts as watched too.
        if let Some(far) = list.iter().rposition(|e| e.watched) {
            for e in &mut list[..far] {
                if e.season > 0 && !st.progress.contains_key(&e.pkey) {
                    e.watched = true;
                }
            }
        }

        let last = list.iter().enumerate().filter(|(_, e)| e.updated > 0.0).max_by(|a, b| a.1.updated.total_cmp(&b.1.updated));
        let next = match last {
            None => list.iter().position(|e| e.season > 0).or(Some(0)),
            Some((i, e)) if !e.watched => Some(i),
            Some((i, _)) => list.iter().enumerate().skip(i + 1).find(|(_, e)| !e.watched).map(|(j, _)| j),
        };
        let all_files: Vec<VFile> = list.iter().flat_map(|e| e.files.iter().cloned()).collect();
        let target = target_dir(&all_files, &name);
        let strays = all_files.iter().filter(|f| f.path.parent() != Some(target.as_path())).cloned().collect();
        all.push(Show {
            watched: list.iter().filter(|e| e.watched).count(),
            last: last.map(|(_, e)| e.updated).unwrap_or(0.0),
            location: location(&all_files),
            key,
            name,
            list,
            next,
            target,
            strays,
        });
    }
    let (mut watching, mut finished): (Vec<Show>, Vec<Show>) = all.into_iter().partition(|s| s.next.is_some());
    watching.sort_by(|a, b| b.last.total_cmp(&a.last).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    finished.sort_by(|a, b| b.last.total_cmp(&a.last));

    let mut mlist: Vec<Movie> = movies
        .into_values()
        .map(|(name, pk, files)| {
            let e = entry(pk, 0, 0, name.clone(), files, st);
            Movie {
                location: location(&e.files),
                strays: e.files.iter().filter(|f| cfg().in_downloads(&f.path)).cloned().collect(),
                name,
                entry: e,
            }
        })
        .collect();
    let newest = |m: &Movie| m.entry.files.iter().map(|f| f.mtime).fold(0.0, f64::max);
    mlist.sort_by(|a, b| {
        (a.entry.watched, !a.entry.in_progress())
            .cmp(&(b.entry.watched, !b.entry.in_progress()))
            .then_with(|| newest(b).total_cmp(&newest(a)))
    });
    Model { watching, finished, movies: mlist }
}

/// Open `entries` in the player as a playlist; the first one resumes where you stopped.
pub fn play(entries: &[Entry]) {
    let entries: Vec<&Entry> = entries.iter().filter(|e| e.file().is_some()).collect();
    let Some(first) = entries.first() else { return };
    let mut cmd = Command::new(&cfg().player);
    let is_vlc = cfg().player.contains("vlc");
    if is_vlc {
        cmd.arg("--qt-continue=0");
    }
    cmd.arg(&first.file().unwrap().path);
    if is_vlc && first.in_progress() {
        cmd.arg(format!(":start-time={}", (first.pos / 1000).saturating_sub(5)));
    }
    for e in &entries[1..] {
        cmd.arg(&e.file().unwrap().path);
    }
    use std::os::unix::process::CommandExt;
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0);
    match cmd.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => eprintln!("upnext: couldn't start {}: {e}", cfg().player),
    }
}

/// Move files into `target`, across drives if needed. Returns (moved, skipped reasons).
pub fn move_files(files: &[VFile], target: &Path) -> std::io::Result<(usize, Vec<String>)> {
    std::fs::create_dir_all(target)?;
    let (mut moved, mut skipped) = (0, vec![]);
    for f in files {
        let name = f.path.file_name().unwrap_or_default();
        let dst = target.join(name);
        let age = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64() - f.mtime)
            .unwrap_or(1e9);
        if age < 60.0 {
            skipped.push(format!("{} is still downloading", name.to_string_lossy()));
        } else if dst.exists() {
            skipped.push(format!("{} is already there", name.to_string_lossy()));
        } else if std::fs::rename(&f.path, &dst).is_err() {
            // Different drive: copy, then delete the original.
            if let Err(e) = std::fs::copy(&f.path, &dst) {
                let _ = std::fs::remove_file(&dst);
                return Err(e);
            }
            std::fs::remove_file(&f.path)?;
            moved += 1;
        } else {
            moved += 1;
        }
    }
    Ok((moved, skipped))
}

/// `upnext --list`: print everything to the terminal.
pub fn cli_list() {
    let mut st = State::load();
    let files = scan(&mut st.durations);
    let durs: HashMap<String, u64> = files.iter().map(|f| (f.pkey.clone(), f.dur)).collect();
    st.sync_vlc(&durs);
    st.save();
    let m = build(&files, &st);
    for sh in m.watching.iter().chain(&m.finished) {
        println!("{}  [{}/{} watched · {}]", sh.name, sh.watched, sh.list.len(), sh.location);
        match sh.next {
            Some(i) => println!("   next: {} — {}", sh.list[i].label(), sh.list[i].status()),
            None => println!("   all caught up"),
        }
        if !sh.strays.is_empty() {
            println!("   {} file(s) to move → {}", sh.strays.len(), sh.target.display());
        }
    }
    println!("\nMovies:");
    for mv in &m.movies {
        println!("   {} — {} · {}", mv.name, mv.entry.status(), mv.location);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vfile(path: PathBuf, mtime: f64) -> VFile {
        let info = parse("Show.S01E01.mkv", "").unwrap();
        VFile { pkey: info.pkey(), path, size: 3, mtime, dur: 0, info }
    }

    #[test]
    fn moves_and_skips() {
        let base = std::env::temp_dir().join(format!("upnext-test-{}", std::process::id()));
        let (src, dst) = (base.join("dl"), base.join("lib/Show"));
        std::fs::create_dir_all(&src).unwrap();
        for n in ["a.mkv", "b.mkv", "c.mkv"] {
            std::fs::write(src.join(n), "abc").unwrap();
        }
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(dst.join("c.mkv"), "old").unwrap();
        let fresh = crate::state::now();
        let files = vec![vfile(src.join("a.mkv"), 0.0), vfile(src.join("b.mkv"), fresh), vfile(src.join("c.mkv"), 0.0)];
        let (moved, skipped) = move_files(&files, &dst).unwrap();
        assert_eq!(moved, 1);
        assert_eq!(skipped.len(), 2); // b still downloading, c already exists
        assert!(dst.join("a.mkv").exists() && !src.join("a.mkv").exists());
        assert!(src.join("b.mkv").exists());
        assert_eq!(std::fs::read_to_string(dst.join("c.mkv")).unwrap(), "old");
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn time_format() {
        assert_eq!(hms(1_462_000), "24:22");
        assert_eq!(hms(9_155_000), "2:32:35");
    }
}
