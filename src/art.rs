//! Frame grabs (tiles + hero backdrops) and per-show accent colours.

use crate::config::thumb_dir;
use gtk::prelude::*;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub type Rgb = [u8; 3];

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

/// Cached image for a video. `hero` = wide backdrop, otherwise a card thumbnail.
pub fn art_path(src: &Path, size: u64, hero: bool) -> PathBuf {
    let kind = if hero { "hero" } else { "tile" };
    thumb_dir().join(format!("{:016x}-{kind}.jpg", fnv(&format!("{}|{size}", src.display()))))
}

/// Grab a frame (generating it if needed) and work out its accent colour.
pub fn make(src: &Path, size: u64, hero: bool) -> (Option<PathBuf>, Option<Rgb>) {
    let out = art_path(src, size, hero);
    if !out.exists() {
        let _ = std::fs::create_dir_all(thumb_dir());
        let (width, at) = if hero { ("1280", "30%") } else { ("400", "22%") };
        let _ = Command::new("ffmpegthumbnailer")
            .arg("-i")
            .arg(src)
            .arg("-o")
            .arg(&out)
            .args(["-s", width, "-t", at, "-q", "9"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    if !out.exists() {
        return (None, None);
    }
    let color = accent(&out);
    (Some(out), color)
}

/// A vivid colour that represents the image: saturated pixels count far more than greys.
fn accent(path: &Path) -> Option<Rgb> {
    let tex = gtk::gdk::Texture::from_filename(path).ok()?;
    let (w, h) = (tex.width() as usize, tex.height() as usize);
    let stride = w * 4;
    let mut buf = vec![0u8; stride * h];
    tex.download(&mut buf, stride);
    let (mut r, mut g, mut b, mut wsum) = (0f64, 0f64, 0f64, 0f64);
    for px in buf.as_chunks::<4>().0.iter().step_by(3) {
        let (pb, pg, pr) = (px[0] as f64, px[1] as f64, px[2] as f64); // BGRA
        let max = pr.max(pg).max(pb);
        let min = pr.min(pg).min(pb);
        if max < 25.0 {
            continue;
        }
        let sat = (max - min) / max;
        let weight = sat * sat * (max / 255.0) + 0.01;
        r += pr * weight;
        g += pg * weight;
        b += pb * weight;
        wsum += weight;
    }
    if wsum == 0.0 {
        return None;
    }
    let (h, s, _) = rgb_to_hsv(r / wsum, g / wsum, b / wsum);
    Some(hsv_to_rgb(h, (s * 1.35).clamp(0.45, 0.85), 0.92))
}

fn rgb_to_hsv(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let (r, g, b) = (r / 255.0, g / 255.0, b / 255.0);
    let max = r.max(g).max(b);
    let d = max - r.min(g).min(b);
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, if max == 0.0 { 0.0 } else { d / max }, max)
}

fn hsv_to_rgb(h: f64, s: f64, v: f64) -> Rgb {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [((r + m) * 255.0) as u8, ((g + m) * 255.0) as u8, ((b + m) * 255.0) as u8]
}

/// Readable text colour on top of `c`.
pub fn on_color(c: Rgb) -> &'static str {
    let l = 0.2126 * c[0] as f64 + 0.7152 * c[1] as f64 + 0.0722 * c[2] as f64;
    if l > 150.0 {
        "#0b0b10"
    } else {
        "#ffffff"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_roundtrip() {
        let (h, s, v) = rgb_to_hsv(200.0, 60.0, 30.0);
        let c = hsv_to_rgb(h, s, v);
        assert!((c[0] as i32 - 200).abs() <= 1 && (c[1] as i32 - 60).abs() <= 1 && (c[2] as i32 - 30).abs() <= 1);
    }

    #[test]
    fn stable_names() {
        assert_eq!(art_path(Path::new("/a.mkv"), 1, true), art_path(Path::new("/a.mkv"), 1, true));
        assert_ne!(art_path(Path::new("/a.mkv"), 1, true), art_path(Path::new("/a.mkv"), 1, false));
    }
}
