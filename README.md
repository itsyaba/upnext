# Up Next

A GTK4 / libadwaita app, written in Rust, for people who watch shows from downloaded files and keep forgetting which episode they were on.

It's styled like a streaming service. The home screen opens on a full-width backdrop of the episode you're in the middle of. Each show gets an **accent color taken from its own artwork**, used for its glow, buttons, progress bars and hover rings. Episodes appear as frame-grab tiles with resume bars, finished shows fade to grayscale until you hover over them, and an Inbox lists everything that's still sitting in your downloads.

- **Continue watching**: every show you're in the middle of, with the episode you stopped on and how far into it you got. One click resumes it in VLC, and the rest of the season is queued behind it.
- **Understands messy filenames**: `Reacher.S03E01.Persuader.720p…`, `Reacher_S03E03_…`, `[T4TSA.cc] Cowboy Bebop Episode 01…`, `[ΛV] Ep-09 S1 Overgeared…`, `01. Cyberpunk Edgerunners…`, `Show - 05` all group into the right show.
- **Learns from VLC**: no plugin needed. It reads VLC's own resume history (`~/.config/vlc/vlc-qt-interface.conf`) and updates the moment VLC closes. Existing history is imported on first run.
- **Show pages**: a season switcher and an episode grid with thumbnails, watched toggles, "up next" and "not downloaded" tiles.
- **Spots missing episodes**: a gap in a season shows as "Not downloaded".
- **Move to library**: moves a show's downloads (e.g. from Telegram's download folder) into its folder on your storage drive. It reuses the folder the show already lives in, or creates one. Progress survives the move because it's tracked per episode, not per file.

## Install

Needs Rust, GTK 4.14+, libadwaita 1.6+, `ffmpeg` (for `ffprobe`) and `vlc`. `ffmpegthumbnailer` is optional, for show thumbnails.

```sh
# Arch / CachyOS
sudo pacman -S --needed rustup gtk4 libadwaita ffmpeg ffmpegthumbnailer vlc
rustup default stable

git clone https://github.com/itsyaba/upnext && cd upnext
./install.sh
```

## Configure

The first run writes `~/.config/upnext/config.json`:

```json
{
  "downloads": "/home/you/Downloads/Telegram Desktop",
  "library": "/mnt/storage/Movies",
  "player": "vlc"
}
```

- `downloads`: where new episodes land. Files here get a **Move to library** button.
- `library`: long-term storage. Both folders are scanned recursively.

## How progress works

| What you do | What Up Next records |
|---|---|
| Close VLC partway through | Resume point for that episode |
| Watch past 90%, or to within 3 minutes of the end | Watched, so the next episode is up |
| Tick or untick the checkbox | Manual override |
| Watch an episode with earlier ones untracked | Earlier ones count as watched |

Progress lives in `~/.local/share/upnext/state.json`.

`upnext --list` prints the same overview in the terminal. Press `Ctrl+R` (or `F5`) to rescan.

## License

MIT
