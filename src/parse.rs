//! Turns messy release filenames into (show, season, episode, title).

use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Episode { season: u32, ep: u32, title: String },
    Movie,
}

#[derive(Clone, Debug)]
pub struct Parsed {
    /// Display name ("Reacher", "The Wrecking Crew (1968)")
    pub show: String,
    /// Normalised grouping key ("reacher")
    pub key: String,
    pub kind: Kind,
}

impl Parsed {
    /// Stable progress key, independent of where the file lives.
    pub fn pkey(&self) -> String {
        match &self.kind {
            Kind::Episode { season, ep, .. } => format!("{}|S{season:02}E{ep:03}", self.key),
            Kind::Movie => format!("movie|{}", self.key),
        }
    }
}

fn re(s: &str) -> Regex {
    Regex::new(s).expect("valid regex")
}

static JUNK: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)\b(?:\d{3,4}p|web(?:[ -]?dl|rip)?|blu-?ray|brrip|hdrip|hdtv|dvdrip|x26[45]|h26[45]|hevc|avc|aac\d*|ac3|ddp?\d*|\dch|10 ?bits?|dual|multi|msubs|nf|amzn|dsnp|hmax|atvp|pahe\w*|rarbg|\d+mb)\b.*$")
});
static YEAR: LazyLock<Regex> = LazyLock::new(|| re(r"\b(19[3-9]\d|20\d\d)\b"));
static AT_PREFIX: LazyLock<Regex> = LazyLock::new(|| re(r"^@[^-\s]*-"));
static BRACKETS: LazyLock<Regex> = LazyLock::new(|| re(r"\[[^\]]*\]|\([^)]*\)|【[^】]*】"));
static LEAD_NUM: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*(\d{1,3})\s*[.-]\s+(\D.*)$"));
static SEPS: LazyLock<Regex> = LazyLock::new(|| re(r"[._]"));
static SPACES: LazyLock<Regex> = LazyLock::new(|| re(r"\s+"));
static SXXEYY: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\bS(\d{1,2}) ?E(\d{1,3})\b"));
static NXNN: LazyLock<Regex> = LazyLock::new(|| re(r"\b(\d{1,2})x(\d{2,3})\b"));
static EPWORD: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)(?:\bS(?:eason)? ?(\d{1,2}) )?\bEp(?:isode)?[ -]?(\d{1,3})\b(?: ?S(\d{1,2})\b)?")
});
static DASH_NUM: LazyLock<Regex> = LazyLock::new(|| re(r"^(.+?) - (\d{1,3})\b(.*)$"));

fn collapse(s: &str) -> String {
    SPACES.replace_all(s, " ").trim().to_string()
}

fn clean(s: &str) -> String {
    collapse(&JUNK.replace(s, ""))
        .trim_matches(|c: char| c == ' ' || c == '-' || c == '_' || c == '~')
        .to_string()
}

pub fn norm(s: &str) -> String {
    let k: String = s.to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    match k.strip_prefix("the") {
        Some(rest) if k.len() > 6 => rest.to_string(),
        _ => k,
    }
}

fn episode(show: &str, season: Option<&str>, num: &str, title: &str, folder: &str) -> Option<Parsed> {
    let mut show = collapse(&YEAR.replace_all(&clean(show), ""));
    show = show.trim_matches(|c: char| c == ' ' || c == '-').to_string();
    if show.is_empty() {
        show = if folder.is_empty() { "Unknown".into() } else { folder.into() };
    }
    Some(Parsed {
        key: norm(&show),
        kind: Kind::Episode {
            season: season.and_then(|s| s.parse().ok()).unwrap_or(1),
            ep: num.parse().ok()?,
            title: clean(title),
        },
        show,
    })
}

/// `folder` is the parent directory name, used when the filename has no show name.
pub fn parse(filename: &str, folder: &str) -> Option<Parsed> {
    let stem = Path::new(filename).file_stem()?.to_string_lossy().to_string();
    let stem = AT_PREFIX.replace(&stem, "");
    let stem = BRACKETS.replace_all(&stem, " ");

    // "01. Cyberpunk Edgerunners"
    if let Some(c) = LEAD_NUM.captures(&stem) {
        return episode(&SEPS.replace_all(&c[2], " "), None, &c[1], "", folder);
    }
    let s = collapse(&SEPS.replace_all(&stem, " "));

    // "Reacher S03E01 Persuader", "Show 1x05"
    for rx in [&*SXXEYY, &*NXNN] {
        if let Some(c) = rx.captures(&s) {
            let m = c.get(0)?;
            return episode(&s[..m.start()], Some(&c[1]), &c[2], &s[m.end()..], folder);
        }
    }
    // "Cowboy Bebop Episode 01 Asteroid Blues", "Ep-09 S1 Overgeared"
    if let Some(c) = EPWORD.captures(&s) {
        let m = c.get(0)?;
        let (before, after) = (&s[..m.start()], &s[m.end()..]);
        let (show, title) = if before.trim().is_empty() { (after, "") } else { (before, after) };
        let season = c.get(1).or(c.get(3)).map(|m| m.as_str());
        return episode(show, season, &c[2], title, folder);
    }
    // "Show - 05"
    if let Some(c) = DASH_NUM.captures(&s) {
        return episode(&c[1], None, &c[2], &c[3], folder);
    }

    // Movie: title up to the first year that isn't at the very start.
    let year = YEAR.find_iter(&s).find(|m| m.start() > 0);
    let (title, year) = match year {
        Some(y) => (&s[..y.start()], y.as_str()),
        None => (s.as_str(), ""),
    };
    let title = clean(title);
    if title.is_empty() {
        return None;
    }
    Some(Parsed {
        show: if year.is_empty() { title.clone() } else { format!("{title} ({year})") },
        key: format!("{}{year}", norm(&title)),
        kind: Kind::Movie,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(name: &str) -> (String, u32, u32, String) {
        let p = parse(name, "").unwrap();
        match p.kind {
            Kind::Episode { season, ep, title } => (p.show, season, ep, title),
            Kind::Movie => panic!("{name} parsed as movie"),
        }
    }

    #[test]
    fn dotted_sxxeyy() {
        assert_eq!(
            ep("Reacher.S03E01.Persuader.720p.WEB-DL.x264.450MB-Pahe.in.mkv"),
            ("Reacher".into(), 3, 1, "Persuader".into())
        );
        assert_eq!(
            ep("Percy.Jackson.and.the.Olympians.S01E04.I.Plunge.to.My.Death..mkv"),
            ("Percy Jackson and the Olympians".into(), 1, 4, "I Plunge to My Death".into())
        );
        assert_eq!(ep("Percy.Jackson.and.the.Olympians.S00E01.A.Heros.Journey.The.M.mkv").1, 0);
    }

    #[test]
    fn underscored() {
        assert_eq!(
            ep("Reacher_S03E03_Number_2_with_a_Bullet_720p_WEB_DL_x264_450MB_Pahe.mkv"),
            ("Reacher".into(), 3, 3, "Number 2 with a Bullet".into())
        );
    }

    #[test]
    fn episode_word() {
        assert_eq!(
            ep("[T4TSA.cc] Cowboy Bebop Episode 01 Asteroid Blues (C) 1080p.mkv"),
            ("Cowboy Bebop".into(), 1, 1, "Asteroid Blues".into())
        );
        assert_eq!(ep("[ΛV] Ep-09 S1 Overgeared [Dual] [1080p].mkv"), ("Overgeared".into(), 1, 9, "".into()));
    }

    #[test]
    fn leading_number() {
        assert_eq!(ep("01. Cyberpunk Edgerunners [T4TSA.cc].mkv"), ("Cyberpunk Edgerunners".into(), 1, 1, "".into()));
    }

    #[test]
    fn dash_number() {
        assert_eq!(ep("[SubsPlease] Frieren - 05 (1080p).mkv").0, "Frieren");
    }

    #[test]
    fn movies() {
        let m = parse("@R_A_R_B_G-The.Wrecking.Crew.1968.1080p.BluRay.x265-RARBG.mp4", "").unwrap();
        assert_eq!(m.kind, Kind::Movie);
        assert_eq!(m.show, "The Wrecking Crew (1968)");
        let m2 = parse("The.Wrecking.Crew.2026.1080p.WEB-DL.x265.6CH-Pahe.in.mkv", "").unwrap();
        assert_ne!(m.key, m2.key);
        assert_eq!(parse("Spider-Man.Brand.New.Day.2026.1080p.WEBRip.mkv", "").unwrap().show, "Spider-Man Brand New Day (2026)");
        assert_eq!(parse("Playdate 2025 1080p WEB-DL HEVC x265 5.1 BONE.mkv", "").unwrap().show, "Playdate (2025)");
    }

    #[test]
    fn same_show_same_key() {
        let a = parse("Reacher.S01E01.Welcome.to.Margrave.720p.BluRay.x264-Pahe.ph.mkv", "").unwrap();
        let b = parse("Reacher_S02E02_What_Happens_in_Atlantic_City_720p_BluRay_x264_Pahe.mkv", "").unwrap();
        assert_eq!(a.key, b.key);
    }
}
