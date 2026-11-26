//! GTK4 / libadwaita front end: a dark, cinematic "continue watching" screen.

use crate::art::{self, Rgb};
use crate::config::{cfg, vlc_history};
use crate::library::{self, build, hms, play, Entry, Model, Movie, Show, VFile};
use crate::state::{now, State};
use adw::prelude::*;
use gtk::{gio, glib, pango};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

const APP_ID: &str = "io.github.itsyaba.UpNext";
const STYLE: &str = include_str!("style.css");

// ───────────────────────── state ─────────────────────────

#[derive(Default, Clone)]
struct ArtSnap {
    tiles: HashMap<PathBuf, PathBuf>,
    heroes: HashMap<PathBuf, PathBuf>,
    colors: HashMap<PathBuf, Rgb>,
}

/// The file that represents a show: the one you're about to watch, else the first one on disk.
fn show_src(sh: &Show) -> Option<&VFile> {
    sh.next.and_then(|i| sh.list[i].file()).or_else(|| sh.list.iter().find_map(|e| e.file()))
}

impl ArtSnap {
    fn tile(&self, f: Option<&VFile>) -> Option<&Path> {
        f.and_then(|f| self.tiles.get(&f.path)).map(|p| p.as_path())
    }
    fn hero(&self, sh: &Show) -> Option<&Path> {
        show_src(sh).and_then(|f| self.heroes.get(&f.path).or(self.tiles.get(&f.path))).map(|p| p.as_path())
    }
    fn show_color(&self, sh: &Show) -> Option<Rgb> {
        show_src(sh)
            .and_then(|f| self.colors.get(&f.path).copied())
            .or_else(|| sh.list.iter().filter_map(|e| e.file()).find_map(|f| self.colors.get(&f.path).copied()))
    }
}

#[derive(Default)]
struct Inner {
    st: State,
    files: Option<Vec<VFile>>,
    art: ArtSnap,
    tried: HashSet<(PathBuf, bool)>,
    art_running: bool,
    scanning: bool,
    pending: HashSet<&'static str>,
    seasons: HashMap<String, u32>,
    animated: bool,
}

/// (display name, files, destination folder)
type MoveGroup = (String, Vec<VFile>, PathBuf);
/// A menu entry: (icon, label, action)
type MenuItem<'a> = (&'a str, &'a str, Box<dyn Fn()>);

struct Detail {
    key: String,
    page: adw::NavigationPage,
    scroller: gtk::ScrolledWindow,
    holder: adw::Bin,
}

#[derive(Clone)]
struct Ui {
    nav: adw::NavigationView,
    toasts: adw::ToastOverlay,
    stack: gtk::Stack,
    scroller: gtk::ScrolledWindow,
    holder: adw::Bin,
    css: gtk::CssProvider,
    detail: Rc<RefCell<Option<Detail>>>,
    inner: Rc<RefCell<Inner>>,
    monitors: Rc<RefCell<Vec<gio::FileMonitor>>>,
}

// ───────────────────────── widget helpers ─────────────────────────

/// Letter-spacing (px) for tracked-out mono labels. Done through Pango rather than CSS,
/// because GTK doesn't include CSS letter-spacing when measuring, which truncates the text.
fn tracking(classes: &[&str]) -> i32 {
    for (class, px) in [("brand-text", 5), ("eyebrow", 3), ("section-sub", 2), ("stat-label", 2), ("chip", 1)] {
        if classes.contains(&class) {
            return px;
        }
    }
    0
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::builder().label(text).xalign(0.0).build();
    let px = tracking(classes);
    if px > 0 {
        let attrs = pango::AttrList::new();
        attrs.insert(pango::AttrInt::new_letter_spacing(px * pango::SCALE));
        l.set_attributes(Some(&attrs));
    } else {
        l.set_ellipsize(pango::EllipsizeMode::End);
    }
    for c in classes {
        l.add_css_class(c);
    }
    l
}

fn wlabel(text: &str, classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::builder().label(text).xalign(0.0).wrap(true).wrap_mode(pango::WrapMode::WordChar).build();
    for c in classes {
        l.add_css_class(c);
    }
    l
}

fn chip(text: &str, extra: &[&str]) -> gtk::Label {
    let l = label(text, &["chip", "mono"]);
    for c in extra {
        l.add_css_class(c);
    }
    l
}

fn bx(o: gtk::Orientation, spacing: i32, classes: &[&str]) -> gtk::Box {
    let b = gtk::Box::new(o, spacing);
    for c in classes {
        b.add_css_class(c);
    }
    b
}
fn vbox(spacing: i32, classes: &[&str]) -> gtk::Box {
    bx(gtk::Orientation::Vertical, spacing, classes)
}
fn hbox(spacing: i32, classes: &[&str]) -> gtk::Box {
    bx(gtk::Orientation::Horizontal, spacing, classes)
}

fn button(icon: Option<&str>, text: &str, classes: &[&str]) -> gtk::Button {
    let b = gtk::Button::new();
    match (icon, text.is_empty()) {
        (Some(i), true) => b.set_icon_name(i),
        (Some(i), false) => b.set_child(Some(&adw::ButtonContent::builder().icon_name(i).label(text).build())),
        (None, _) => b.set_label(text),
    }
    for c in classes {
        b.add_css_class(c);
    }
    b
}

/// Fixed-size picture frame. The picture is an overlay child so it never dictates the size.
fn frame(w: i32, h: i32, img: Option<&Path>, img_classes: &[&str]) -> gtk::Overlay {
    let ov = gtk::Overlay::new();
    let base = gtk::Box::new(gtk::Orientation::Vertical, 0);
    base.set_size_request(w, h);
    ov.set_child(Some(&base));
    ov.set_overflow(gtk::Overflow::Hidden);
    if let Some(img) = img {
        let pic = gtk::Picture::builder().content_fit(gtk::ContentFit::Cover).can_shrink(true).build();
        pic.set_filename(Some(img));
        for c in img_classes {
            pic.add_css_class(c);
        }
        ov.add_overlay(&pic);
    }
    ov
}

fn layer(ov: &gtk::Overlay, class: &str) {
    let b = vbox(0, &[class]);
    b.set_can_target(false);
    ov.add_overlay(&b);
}

fn on_click(w: &impl IsA<gtk::Widget>, f: impl Fn() + 'static) {
    let g = gtk::GestureClick::new();
    g.connect_released(move |g, _, _, _| {
        g.set_state(gtk::EventSequenceState::Claimed);
        f();
    });
    w.add_controller(g);
    w.set_cursor_from_name(Some("pointer"));
}

fn tint(key: &str) -> String {
    format!("t-{}", key.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>())
}

fn open_folder(p: &Path) {
    let uri = gio::File::for_path(p).uri();
    let _ = gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>);
}

fn durations(files: &[VFile]) -> HashMap<String, u64> {
    files.iter().map(|f| (f.pkey.clone(), f.dur)).collect()
}

fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1024f64.powi(3))
}

fn left(ms: u64) -> String {
    let m = ms / 60_000;
    if m >= 60 {
        format!("{}h {:02}m", m / 60, m % 60)
    } else {
        format!("{m}m")
    }
}

fn season_name(s: u32) -> String {
    if s == 0 {
        "Specials".into()
    } else {
        format!("Season {s}")
    }
}

/// Dynamic per-show styling: glow, accent buttons, progress bars and hover rings in the show's own colour.
fn tint_css(class: &str, c: Rgb) -> String {
    let [r, g, b] = c;
    let fg = art::on_color(c);
    format!(
        ".{class} .glow {{ background-image: radial-gradient(ellipse farthest-corner at 12% 100%, rgba({r},{g},{b},0.50), rgba({r},{g},{b},0) 62%); }}
.{class} .accent {{ background-color: rgb({r},{g},{b}); color: {fg}; }}
.{class} .accent:hover {{ background-color: rgba({r},{g},{b},0.85); }}
.{class} .accent-text {{ color: rgb({r},{g},{b}); }}
.{class} progressbar.thin progress {{ background-color: rgb({r},{g},{b}); }}
.{class}.tile:hover .tile-frame {{ box-shadow: 0 16px 40px rgba({r},{g},{b},0.38), 0 0 0 2px rgba({r},{g},{b},0.95); }}
.{class}.upnext .tile-frame {{ box-shadow: 0 0 0 2px rgb({r},{g},{b}); }}
.{class} .watch-toggle.on {{ background-color: rgb({r},{g},{b}); color: {fg}; }}
"
    )
}

// ───────────────────────── app ─────────────────────────

impl Ui {
    fn new(app: &adw::Application) -> (Ui, adw::ApplicationWindow) {
        let toasts = adw::ToastOverlay::new();
        let nav = adw::NavigationView::new();

        // Home page
        let hb = adw::HeaderBar::new();
        hb.add_css_class("clear-bar");
        let brand = hbox(8, &["brand"]);
        let dot = vbox(0, &["brand-dot"]);
        dot.set_valign(gtk::Align::Center);
        brand.append(&dot);
        brand.append(&label("UP NEXT", &["mono", "brand-text"]));
        hb.set_title_widget(Some(&brand));
        let refresh = button(Some("view-refresh-symbolic"), "", &["glass-icon"]);
        refresh.set_tooltip_text(Some("Rescan (Ctrl+R)"));
        let downloads = button(Some("folder-download-symbolic"), "", &["glass-icon"]);
        downloads.set_tooltip_text(Some("Open downloads folder"));
        hb.pack_end(&refresh);
        hb.pack_start(&downloads);

        let stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).build();
        let loading = vbox(18, &[]);
        loading.set_valign(gtk::Align::Center);
        loading.set_halign(gtk::Align::Center);
        loading.append(&adw::Spinner::builder().width_request(56).height_request(56).build());
        loading.append(&label("SCANNING YOUR LIBRARY", &["mono", "eyebrow"]));
        stack.add_named(&loading, Some("loading"));
        let holder = adw::Bin::new();
        let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&holder).build();
        stack.add_named(&scroller, Some("content"));

        let tv = adw::ToolbarView::new();
        tv.set_extend_content_to_top_edge(true);
        tv.set_top_bar_style(adw::ToolbarStyle::Flat);
        tv.add_top_bar(&hb);
        tv.set_content(Some(&stack));
        nav.push(&adw::NavigationPage::builder().title("Up Next").tag("home").child(&tv).build());
        toasts.set_child(Some(&nav));

        let win = adw::ApplicationWindow::builder()
            .application(app)
            .title("Up Next")
            .default_width(1200)
            .default_height(860)
            .width_request(420)
            .height_request(500)
            .content(&toasts)
            .build();
        win.add_css_class("cinema");

        let css = gtk::CssProvider::new();
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1);
        }

        let ui = Ui {
            nav,
            toasts,
            stack,
            scroller,
            holder,
            css,
            detail: Rc::default(),
            inner: Rc::new(RefCell::new(Inner { st: State::load(), ..Default::default() })),
            monitors: Rc::default(),
        };

        let u = ui.clone();
        refresh.connect_clicked(move |_| u.rescan());
        downloads.connect_clicked(|_| open_folder(&cfg().downloads));
        let act = gio::SimpleAction::new("refresh", None);
        let u = ui.clone();
        act.connect_activate(move |_, _| u.rescan());
        win.add_action(&act);
        app.set_accels_for_action("win.refresh", &["<Control>r", "F5"]);

        let u = ui.clone();
        ui.nav.connect_popped(move |_, page| {
            let mut d = u.detail.borrow_mut();
            if d.as_ref().is_some_and(|d| &d.page == page) {
                *d = None;
            }
        });

        if let Ok(m) = gio::File::for_path(vlc_history()).monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE) {
            let u = ui.clone();
            m.connect_changed(move |_, _, _, _| u.later("vlc", 1, Ui::on_vlc));
            ui.monitors.borrow_mut().push(m);
        }
        for dir in cfg().sources() {
            if let Ok(m) = gio::File::for_path(dir).monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE) {
                let u = ui.clone();
                m.connect_changed(move |_, _, _, _| u.later("scan", 5, Ui::rescan));
                ui.monitors.borrow_mut().push(m);
            }
        }
        (ui, win)
    }

    fn later(&self, name: &'static str, secs: u32, f: fn(&Ui)) {
        if !self.inner.borrow_mut().pending.insert(name) {
            return;
        }
        let u = self.clone();
        glib::timeout_add_seconds_local_once(secs, move || {
            u.inner.borrow_mut().pending.remove(name);
            f(&u);
        });
    }

    fn toast(&self, msg: &str) {
        self.toasts.add_toast(adw::Toast::builder().title(glib::markup_escape_text(msg)).timeout(5).build());
    }

    // ── data ──

    fn rescan(&self) {
        {
            let mut i = self.inner.borrow_mut();
            if i.scanning {
                return;
            }
            i.scanning = true;
            if i.files.is_none() {
                self.stack.set_visible_child_name("loading");
            }
        }
        let mut cache = self.inner.borrow().st.durations.clone();
        let u = self.clone();
        glib::spawn_future_local(async move {
            let res = gio::spawn_blocking(move || {
                let files = library::scan(&mut cache);
                (files, cache)
            })
            .await;
            {
                let mut i = u.inner.borrow_mut();
                i.scanning = false;
                if let Ok((files, cache)) = res {
                    i.st.durations = cache;
                    let d = durations(&files);
                    i.st.sync_vlc(&d);
                    i.st.save();
                    i.files = Some(files);
                }
            }
            u.render();
        });
    }

    /// Generate any thumbnails/backdrops the current view needs, then re-render once.
    fn ensure_art(&self, m: &Model) {
        let jobs: Vec<(PathBuf, u64, bool)> = {
            let mut i = self.inner.borrow_mut();
            if i.art_running {
                return;
            }
            let mut want = vec![];
            for sh in m.watching.iter().chain(&m.finished) {
                if let Some(f) = show_src(sh) {
                    want.push((f.path.clone(), f.size, true));
                }
                for f in sh.list.iter().filter_map(|e| e.file()) {
                    want.push((f.path.clone(), f.size, false));
                }
            }
            for mv in &m.movies {
                if let Some(f) = mv.entry.file() {
                    want.push((f.path.clone(), f.size, false));
                }
            }
            want.retain(|(p, _, h)| i.tried.insert((p.clone(), *h)));
            if want.is_empty() {
                return;
            }
            i.art_running = true;
            want
        };
        let u = self.clone();
        glib::spawn_future_local(async move {
            let res = gio::spawn_blocking(move || {
                jobs.into_iter()
                    .map(|(p, size, hero)| {
                        let (img, color) = art::make(&p, size, hero);
                        (p, hero, img, color)
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
            {
                let mut i = u.inner.borrow_mut();
                i.art_running = false;
                for (p, hero, img, color) in res {
                    if let Some(img) = img {
                        if hero {
                            i.art.heroes.insert(p.clone(), img);
                        } else {
                            i.art.tiles.insert(p.clone(), img);
                        }
                    }
                    if let Some(c) = color {
                        if hero || !i.art.colors.contains_key(&p) {
                            i.art.colors.insert(p, c);
                        }
                    }
                }
            }
            u.render();
        });
    }

    fn on_vlc(&self) {
        let changed = {
            let mut i = self.inner.borrow_mut();
            let Some(files) = &i.files else { return };
            let d = durations(files);
            let changed = i.st.sync_vlc(&d);
            if changed {
                i.st.save();
            }
            changed
        };
        if changed {
            self.render();
        }
    }

    fn set_watched(&self, pkeys: Vec<String>, val: bool) {
        {
            let mut i = self.inner.borrow_mut();
            let t = now();
            for k in pkeys {
                let r = i.st.progress.entry(k).or_default();
                r.watched = val;
                r.pos = 0;
                if val || r.updated == 0.0 {
                    r.updated = t;
                }
            }
            i.st.save();
        }
        self.render_soon();
    }

    fn reset(&self, pkeys: Vec<String>) {
        {
            let mut i = self.inner.borrow_mut();
            for k in pkeys {
                i.st.progress.remove(&k);
            }
            i.st.save();
        }
        self.render_soon();
    }

    fn render_soon(&self) {
        let u = self.clone();
        glib::idle_add_local_once(move || u.render());
    }

    // ── rendering ──

    fn render(&self) {
        let (model, art, seasons, animate) = {
            let mut i = self.inner.borrow_mut();
            let Some(files) = &i.files else { return };
            let model = build(files, &i.st);
            let animate = !i.animated;
            i.animated = true;
            (model, i.art.clone(), i.seasons.clone(), animate)
        };

        let mut css = String::new();
        for sh in model.watching.iter().chain(&model.finished) {
            if let Some(c) = art.show_color(sh) {
                css += &tint_css(&tint(&sh.key), c);
            }
        }
        for mv in &model.movies {
            if let Some(c) = mv.entry.file().and_then(|f| art.colors.get(&f.path)) {
                css += &tint_css(&tint(&mv.entry.pkey), *c);
            }
        }
        self.css.load_from_string(&css);

        let adj = self.scroller.vadjustment();
        let pos = adj.value();
        self.holder.set_child(Some(&self.home(&model, &art, animate)));
        self.stack.set_visible_child_name("content");
        glib::idle_add_local_once(move || adj.set_value(pos));

        let open = self.detail.borrow().as_ref().map(|d| d.key.clone());
        if let Some(key) = open {
            match model.watching.iter().chain(&model.finished).find(|s| s.key == key) {
                Some(sh) => {
                    let d = self.detail.borrow();
                    let d = d.as_ref().unwrap();
                    let adj = d.scroller.vadjustment();
                    let pos = adj.value();
                    let season = seasons.get(&key).copied();
                    d.holder.set_child(Some(&self.detail_body(sh, &art, season)));
                    glib::idle_add_local_once(move || adj.set_value(pos));
                }
                None => {
                    self.nav.pop();
                }
            }
        }
        self.ensure_art(&model);
    }

    fn home(&self, m: &Model, art: &ArtSnap, animate: bool) -> gtk::Box {
        let page = vbox(0, &[]);
        if animate {
            page.add_css_class("intro");
        }
        let all: Vec<&Show> = m.watching.iter().chain(&m.finished).collect();
        if all.is_empty() && m.movies.is_empty() {
            let sp = adw::StatusPage::builder()
                .icon_name("video-x-generic-symbolic")
                .title("Nothing to watch yet")
                .description(glib::markup_escape_text(&format!(
                    "Up Next looks in {} and {}.\nChange the folders in {}",
                    cfg().downloads.display(),
                    cfg().library.display(),
                    crate::config::config_file().display()
                )))
                .vexpand(true)
                .build();
            sp.set_margin_top(80);
            page.append(&sp);
            return page;
        }

        page.append(&self.hero(m.watching.first(), art));

        let body = vbox(44, &["home-body"]);
        let clamp = adw::Clamp::builder().maximum_size(4000).tightening_threshold(4000).child(&body).build();
        page.append(&clamp);

        body.append(&self.stats(m));

        if !m.watching.is_empty() {
            let sec = vbox(16, &[]);
            sec.append(&section("Continue watching", &format!("{} SHOWS IN PROGRESS", m.watching.len())));
            let row = hbox(18, &["carousel"]);
            for (i, sh) in m.watching.iter().enumerate() {
                row.append(&self.show_card(sh, art, i));
            }
            let sc = gtk::ScrolledWindow::builder()
                .vscrollbar_policy(gtk::PolicyType::Never)
                .propagate_natural_height(true)
                .child(&row)
                .build();
            sec.append(&sc);
            body.append(&sec);
        }

        if let Some(inbox) = self.inbox(m, art) {
            body.append(&inbox);
        }

        if !m.movies.is_empty() {
            let sec = vbox(16, &[]);
            let watched = m.movies.iter().filter(|x| x.entry.watched).count();
            sec.append(&section("Movies", &format!("{} FILMS · {} WATCHED", m.movies.len(), watched)));
            let fb = grid();
            for (i, mv) in m.movies.iter().enumerate() {
                fb.append(&self.movie_tile(mv, art, i));
            }
            sec.append(&fb);
            body.append(&sec);
        }

        if !m.finished.is_empty() {
            let sec = vbox(16, &[]);
            sec.append(&section("Finished", "CAUGHT UP · HOVER TO WAKE"));
            let fb = grid();
            for (i, sh) in m.finished.iter().enumerate() {
                let w = self.show_card(sh, art, i);
                w.add_css_class("finished");
                fb.append(&w);
            }
            sec.append(&fb);
            body.append(&sec);
        }
        page
    }

    fn hero(&self, sh: Option<&Show>, art: &ArtSnap) -> gtk::Overlay {
        let ov = frame(-1, 520, sh.and_then(|s| art.hero(s)), &["hero-img"]);
        ov.add_css_class("hero");
        layer(&ov, "glow");
        layer(&ov, "hero-scrim");
        let Some(sh) = sh else {
            let c = vbox(10, &["hero-content"]);
            c.append(&label("ALL CAUGHT UP", &["mono", "eyebrow"]));
            c.append(&wlabel("Nothing in progress", &["hero-title"]));
            ov.add_overlay(&c);
            return ov;
        };
        ov.add_css_class(&tint(&sh.key));
        let n = &sh.list[sh.next.unwrap_or(0)];

        let c = vbox(14, &["hero-content"]);
        c.set_valign(gtk::Align::End);
        c.set_halign(gtk::Align::Start);
        let eyebrow = if n.in_progress() {
            "CONTINUE WATCHING"
        } else if sh.last > 0.0 {
            "UP NEXT"
        } else {
            "START WATCHING"
        };
        let eb = hbox(10, &[]);
        eb.append(&label(eyebrow, &["mono", "eyebrow", "accent-text"]));
        eb.append(&label(&format!("· {}/{} WATCHED · {}", sh.watched, sh.list.len(), sh.location.to_uppercase()), &["mono", "eyebrow"]));
        c.append(&eb);
        let t = wlabel(&sh.name, &["hero-title"]);
        t.set_max_width_chars(22);
        c.append(&t);

        let line = hbox(12, &[]);
        line.append(&chip(&n.code(), &["chip-light"]));
        if !n.title.is_empty() {
            line.append(&label(&n.title, &["hero-sub"]));
        }
        let rem = match (n.file().is_some(), n.in_progress()) {
            (false, _) => "not downloaded yet".to_string(),
            (true, true) => format!("{} left", hms(n.dur().saturating_sub(n.pos))),
            (true, false) if n.dur() > 0 => hms(n.dur()),
            _ => String::new(),
        };
        if !rem.is_empty() {
            line.append(&label(&rem, &["mono", "dim"]));
        }
        c.append(&line);

        if n.in_progress() && n.dur() > 0 {
            let pb = gtk::ProgressBar::builder().fraction((n.pos as f64 / n.dur() as f64).min(1.0)).width_request(420).build();
            pb.set_halign(gtk::Align::Start);
            pb.add_css_class("thin");
            c.append(&pb);
        }

        let actions = hbox(12, &["hero-actions"]);
        if let Some(i) = sh.next {
            let play_btn = button(
                Some("media-playback-start-symbolic"),
                if n.file().is_none() { "Not downloaded" } else if n.in_progress() { "Resume" } else { "Play" },
                &["btn-primary", "accent"],
            );
            play_btn.set_sensitive(n.file().is_some());
            let rest = sh.list[i..].to_vec();
            play_btn.connect_clicked(move |_| play(&rest));
            actions.append(&play_btn);
        }
        let eps = button(Some("view-grid-symbolic"), "Episodes", &["btn-glass"]);
        let (u, key, name) = (self.clone(), sh.key.clone(), sh.name.clone());
        eps.connect_clicked(move |_| u.open_detail(&key, &name));
        actions.append(&eps);
        c.append(&actions);
        ov.add_overlay(&c);
        ov
    }

    fn stats(&self, m: &Model) -> gtk::Box {
        let row = hbox(12, &["stats"]);
        let left_ms: u64 = m
            .watching
            .iter()
            .map(|s| {
                s.list[s.next.unwrap_or(0)..]
                    .iter()
                    .filter(|e| !e.watched && e.file().is_some())
                    .map(|e| e.dur().saturating_sub(e.pos))
                    .sum::<u64>()
            })
            .sum();
        let in_dl: u64 = m
            .watching
            .iter()
            .chain(&m.finished)
            .flat_map(|s| s.list.iter().flat_map(|e| &e.files))
            .chain(m.movies.iter().flat_map(|mv| &mv.entry.files))
            .filter(|f| cfg().in_downloads(&f.path))
            .map(|f| f.size)
            .sum();
        let missing = m.watching.iter().chain(&m.finished).flat_map(|s| &s.list).filter(|e| e.file().is_none()).count();
        let movies_left = m.movies.iter().filter(|x| !x.entry.watched).count();
        for (num, what) in [
            (m.watching.len().to_string(), "SHOWS ON THE GO"),
            (left(left_ms), "LEFT TO WATCH"),
            (movies_left.to_string(), "MOVIES UNWATCHED"),
            (gb(in_dl), "IN DOWNLOADS"),
            (missing.to_string(), if missing == 1 { "MISSING EPISODE" } else { "MISSING EPISODES" }),
        ] {
            let s = vbox(2, &["stat"]);
            s.append(&label(&num, &["stat-num"]));
            s.append(&label(what, &["mono", "stat-label"]));
            row.append(&s);
        }
        let fb = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .min_children_per_line(2)
            .max_children_per_line(5)
            .homogeneous(true)
            .column_spacing(12)
            .row_spacing(12)
            .build();
        while let Some(ch) = row.first_child() {
            row.remove(&ch);
            fb.append(&ch);
        }
        let wrap = vbox(0, &[]);
        wrap.append(&fb);
        wrap
    }

    fn show_card(&self, sh: &Show, art: &ArtSnap, idx: usize) -> gtk::Box {
        let card = vbox(10, &["tile", &format!("d{}", idx.min(9))]);
        card.add_css_class(&tint(&sh.key));
        let n = sh.next.map(|i| &sh.list[i]);
        let src = n.and_then(|n| n.file()).or_else(|| show_src(sh));
        let fr = frame(320, 180, art.tile(src), &["tile-img"]);
        fr.add_css_class("tile-frame");
        layer(&fr, "tile-scrim");

        let top = hbox(6, &[]);
        top.set_valign(gtk::Align::Start);
        top.set_halign(gtk::Align::Start);
        top.set_margin_top(10);
        top.set_margin_start(10);
        match n {
            Some(n) => top.append(&chip(&n.code(), &[])),
            None => top.append(&chip("FINISHED", &[])),
        }
        top.set_can_target(false);
        fr.add_overlay(&top);

        if let (Some(n), Some(i)) = (n, sh.next) {
            if n.file().is_some() {
                let pb = button(Some("media-playback-start-symbolic"), "", &["tile-play"]);
                pb.set_halign(gtk::Align::Center);
                pb.set_valign(gtk::Align::Center);
                pb.set_tooltip_text(Some(if n.in_progress() { "Resume" } else { "Play" }));
                let rest = sh.list[i..].to_vec();
                pb.connect_clicked(move |_| play(&rest));
                fr.add_overlay(&pb);
            }
            let br = hbox(0, &[]);
            br.set_valign(gtk::Align::End);
            br.set_halign(gtk::Align::End);
            br.set_margin_end(10);
            br.set_margin_bottom(12);
            br.set_can_target(false);
            let txt = if n.file().is_none() {
                "NOT DOWNLOADED".to_string()
            } else if n.in_progress() {
                format!("{} LEFT", hms(n.dur().saturating_sub(n.pos)))
            } else if n.dur() > 0 {
                hms(n.dur())
            } else {
                String::new()
            };
            if !txt.is_empty() {
                br.append(&chip(&txt, &[]));
            }
            fr.add_overlay(&br);
            if n.in_progress() && n.dur() > 0 {
                let p = gtk::ProgressBar::builder().fraction((n.pos as f64 / n.dur() as f64).min(1.0)).valign(gtk::Align::End).build();
                p.add_css_class("thin");
                p.set_can_target(false);
                fr.add_overlay(&p);
            }
        }
        let (u, key, name) = (self.clone(), sh.key.clone(), sh.name.clone());
        on_click(&fr, move || u.open_detail(&key, &name));
        card.append(&fr);

        let meta = vbox(3, &["tile-meta"]);
        meta.append(&label(&sh.name, &["tile-title"]));
        let sub = match n {
            Some(n) if !n.title.is_empty() => n.title.clone(),
            Some(n) => format!("Episode {}", n.ep),
            None => format!("{} episodes watched", sh.watched),
        };
        meta.append(&label(&sub, &["tile-sub"]));
        meta.set_size_request(320, -1);
        card.append(&meta);
        card
    }

    fn movie_tile(&self, mv: &Movie, art: &ArtSnap, idx: usize) -> gtk::Box {
        let e = &mv.entry;
        let card = vbox(10, &["tile", &format!("d{}", idx.min(9))]);
        card.add_css_class(&tint(&e.pkey));
        if e.watched {
            card.add_css_class("is-watched");
        }
        let fr = frame(260, 146, art.tile(e.file()), &["tile-img"]);
        fr.add_css_class("tile-frame");
        layer(&fr, "tile-scrim");
        fr.add_overlay(&self.watch_toggle(e));
        let pb = button(Some("media-playback-start-symbolic"), "", &["tile-play"]);
        pb.set_halign(gtk::Align::Center);
        pb.set_valign(gtk::Align::Center);
        let entry = e.clone();
        pb.connect_clicked(move |_| play(std::slice::from_ref(&entry)));
        fr.add_overlay(&pb);
        if let Some(f) = e.file() {
            let br = hbox(0, &[]);
            br.set_valign(gtk::Align::End);
            br.set_halign(gtk::Align::End);
            br.set_margin_end(10);
            br.set_margin_bottom(12);
            br.set_can_target(false);
            let t = if e.in_progress() { format!("{} LEFT", hms(f.dur.saturating_sub(e.pos))) } else { hms(f.dur) };
            br.append(&chip(&t, &[]));
            fr.add_overlay(&br);
        }
        if e.in_progress() && e.dur() > 0 {
            let p = gtk::ProgressBar::builder().fraction((e.pos as f64 / e.dur() as f64).min(1.0)).valign(gtk::Align::End).build();
            p.add_css_class("thin");
            p.set_can_target(false);
            fr.add_overlay(&p);
        }
        let entry = e.clone();
        on_click(&fr, move || play(std::slice::from_ref(&entry)));
        card.append(&fr);
        let meta = vbox(3, &["tile-meta"]);
        meta.append(&label(&mv.name, &["tile-title"]));
        let status = if e.watched { "Watched".to_string() } else { e.status() };
        meta.append(&label(&format!("{status} · {}", mv.location), &["tile-sub"]));
        meta.set_size_request(260, -1);
        card.append(&meta);
        card
    }

    fn watch_toggle(&self, e: &Entry) -> gtk::Button {
        let b = button(Some("object-select-symbolic"), "", &["watch-toggle"]);
        if e.watched {
            b.add_css_class("on");
        }
        b.set_tooltip_text(Some(if e.watched { "Mark as unwatched" } else { "Mark as watched" }));
        b.set_halign(gtk::Align::End);
        b.set_valign(gtk::Align::Start);
        b.set_margin_top(10);
        b.set_margin_end(10);
        let (u, k, w) = (self.clone(), e.pkey.clone(), e.watched);
        b.connect_clicked(move |_| u.set_watched(vec![k.clone()], !w));
        b
    }

    /// Everything that isn't where it belongs yet, with one-click moves.
    fn inbox(&self, m: &Model, art: &ArtSnap) -> Option<gtk::Box> {
        // (name, tint class, files, destination, thumbnail)
        #[allow(clippy::type_complexity)]
        let mut groups: Vec<(String, String, Vec<VFile>, PathBuf, Option<&Path>)> = vec![];
        for sh in m.watching.iter().chain(&m.finished) {
            if !sh.strays.is_empty() {
                groups.push((sh.name.clone(), tint(&sh.key), sh.strays.clone(), sh.target.clone(), art.tile(sh.strays.first())));
            }
        }
        for mv in &m.movies {
            if !mv.strays.is_empty() {
                groups.push((mv.name.clone(), tint(&mv.entry.pkey), mv.strays.clone(), cfg().library.clone(), art.tile(mv.strays.first())));
            }
        }
        if groups.is_empty() || !cfg().library.is_dir() {
            return None;
        }
        let total: u64 = groups.iter().flat_map(|g| &g.2).map(|f| f.size).sum();
        let count: usize = groups.iter().map(|g| g.2.len()).sum();

        let sec = vbox(16, &[]);
        let head = hbox(12, &[]);
        let titles = section("Inbox", &format!("{count} FILES · {} OUTSIDE YOUR LIBRARY", gb(total)));
        titles.set_hexpand(true);
        head.append(&titles);
        let all = button(Some("drive-harddisk-symbolic"), "Move everything", &["btn-primary", "light"]);
        all.set_valign(gtk::Align::End);
        let (u, gs) = (self.clone(), groups.iter().map(|g| (g.0.clone(), g.2.clone(), g.3.clone())).collect::<Vec<_>>());
        all.connect_clicked(move |b| u.move_groups(b, gs.clone()));
        head.append(&all);
        sec.append(&head);

        let panel = vbox(0, &["inbox"]);
        for (name, tclass, files, target, img) in groups {
            let row = hbox(16, &["inbox-row", &tclass]);
            let fr = frame(112, 63, img, &["tile-img"]);
            fr.add_css_class("mini-frame");
            row.append(&fr);
            let info = vbox(3, &[]);
            info.set_hexpand(true);
            info.set_valign(gtk::Align::Center);
            info.append(&label(&name, &["inbox-name"]));
            let size: u64 = files.iter().map(|f| f.size).sum();
            info.append(&label(&format!("{} FILE{} · {}", files.len(), if files.len() == 1 { "" } else { "S" }, gb(size)), &["mono", "dim", "small"]));
            let rel = target.strip_prefix(&cfg().library).map(|p| p.display().to_string()).unwrap_or_default();
            let dest = cfg().library.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            info.append(&label(&format!("→ {dest}/{rel}"), &["mono", "small", "accent-text"]));
            row.append(&info);
            let mb = button(Some("go-next-symbolic"), "Move", &["btn-glass", "small-btn"]);
            mb.set_valign(gtk::Align::Center);
            let (u, n2) = (self.clone(), name.clone());
            mb.connect_clicked(move |b| u.move_groups(b, vec![(n2.clone(), files.clone(), target.clone())]));
            row.append(&mb);
            panel.append(&row);
        }
        sec.append(&panel);
        Some(sec)
    }

    // ── show page ──

    fn open_detail(&self, key: &str, name: &str) {
        if self.detail.borrow().as_ref().is_some_and(|d| d.key == key) {
            return;
        }
        let hb = adw::HeaderBar::new();
        hb.add_css_class("clear-bar");
        hb.set_show_title(false);
        let holder = adw::Bin::new();
        let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&holder).build();
        let tv = adw::ToolbarView::new();
        tv.set_extend_content_to_top_edge(true);
        tv.set_top_bar_style(adw::ToolbarStyle::Flat);
        tv.add_top_bar(&hb);
        tv.set_content(Some(&scroller));
        let page = adw::NavigationPage::builder().title(name).child(&tv).build();
        *self.detail.borrow_mut() = Some(Detail { key: key.to_string(), page: page.clone(), scroller, holder });
        self.render();
        self.nav.push(&page);
    }

    fn detail_body(&self, sh: &Show, art: &ArtSnap, season: Option<u32>) -> gtk::Box {
        let page = vbox(0, &[&tint(&sh.key)]);
        let ov = frame(-1, 440, art.hero(sh), &["hero-img"]);
        ov.add_css_class("hero");
        layer(&ov, "glow");
        layer(&ov, "hero-scrim");
        let c = vbox(14, &["hero-content"]);
        c.set_valign(gtk::Align::End);
        c.set_halign(gtk::Align::Start);
        let seasons: BTreeSet<u32> = sh.list.iter().map(|e| e.season).collect();
        let real = seasons.iter().filter(|s| **s > 0).count();
        c.append(&label(
            &format!(
                "SERIES · {real} SEASON{} · {} EPISODES · {} WATCHED",
                if real == 1 { "" } else { "S" },
                sh.list.len(),
                sh.watched
            ),
            &["mono", "eyebrow"],
        ));
        let t = wlabel(&sh.name, &["hero-title"]);
        t.set_max_width_chars(22);
        c.append(&t);
        let next = sh.next.map(|i| &sh.list[i]);
        let line = match next {
            Some(n) => format!("{} {} — {}", if n.in_progress() { "Resume" } else { "Up next:" }, n.label(), n.status().to_lowercase()),
            None => "You're all caught up.".into(),
        };
        c.append(&label(&line, &["hero-sub"]));
        let pct = if sh.list.is_empty() { 0.0 } else { sh.watched as f64 / sh.list.len() as f64 };
        let pb = gtk::ProgressBar::builder().fraction(pct).width_request(420).build();
        pb.set_halign(gtk::Align::Start);
        pb.add_css_class("thin");
        c.append(&pb);

        let actions = hbox(12, &["hero-actions"]);
        if let (Some(n), Some(i)) = (next, sh.next) {
            let b = button(
                Some("media-playback-start-symbolic"),
                &if n.file().is_none() { "Not downloaded".to_string() } else { format!("{} {}", if n.in_progress() { "Resume" } else { "Play" }, n.code()) },
                &["btn-primary", "accent"],
            );
            b.set_sensitive(n.file().is_some());
            let rest = sh.list[i..].to_vec();
            b.connect_clicked(move |_| play(&rest));
            actions.append(&b);
        }
        if !sh.strays.is_empty() && cfg().library.is_dir() {
            let b = button(Some("drive-harddisk-symbolic"), "Move to library", &["btn-glass"]);
            b.set_tooltip_text(Some(&format!("{} file(s) → {}", sh.strays.len(), sh.target.display())));
            let (u, n, f, t) = (self.clone(), sh.name.clone(), sh.strays.clone(), sh.target.clone());
            b.connect_clicked(move |b| u.move_groups(b, vec![(n.clone(), f.clone(), t.clone())]));
            actions.append(&b);
        }
        actions.append(&self.show_menu(sh));
        c.append(&actions);
        ov.add_overlay(&c);
        page.append(&ov);

        let body = vbox(22, &["home-body"]);
        let clamp = adw::Clamp::builder().maximum_size(4000).tightening_threshold(4000).child(&body).build();
        page.append(&clamp);

        let default = next.map(|n| n.season).or_else(|| seasons.iter().copied().find(|s| *s > 0)).unwrap_or(0);
        let sel = season.filter(|s| seasons.contains(s)).unwrap_or(default);
        if seasons.len() > 1 {
            let sw = hbox(6, &["season-switch"]);
            let mut first: Option<gtk::ToggleButton> = None;
            for s in seasons.iter().copied() {
                let watched = sh.list.iter().filter(|e| e.season == s && e.watched).count();
                let total = sh.list.iter().filter(|e| e.season == s).count();
                let tb = gtk::ToggleButton::builder().active(s == sel).build();
                let inner = hbox(8, &[]);
                inner.append(&label(&season_name(s), &[]));
                inner.append(&label(&format!("{watched}/{total}"), &["mono", "season-count"]));
                tb.set_child(Some(&inner));
                if let Some(f) = &first {
                    tb.set_group(Some(f));
                } else {
                    first = Some(tb.clone());
                }
                let (u, key) = (self.clone(), sh.key.clone());
                tb.connect_toggled(move |b| {
                    if b.is_active() {
                        u.inner.borrow_mut().seasons.insert(key.clone(), s);
                        u.render_soon();
                    }
                });
                sw.append(&tb);
            }
            body.append(&sw);
        } else {
            body.append(&section(&season_name(sel), &format!("{} EPISODES", sh.list.len())));
        }

        let fb = grid();
        for (i, e) in sh.list.iter().enumerate().filter(|(_, e)| e.season == sel) {
            fb.append(&self.episode_tile(sh, e, i, art));
        }
        body.append(&fb);
        page
    }

    fn show_menu(&self, sh: &Show) -> gtk::MenuButton {
        let menu = vbox(2, &["menu-pop"]);
        let pop = gtk::Popover::builder().child(&menu).build();
        let all: Vec<String> = sh.list.iter().map(|e| e.pkey.clone()).collect();
        let folder = show_src(sh).and_then(|f| f.path.parent().map(|p| p.to_path_buf())).unwrap_or_else(|| sh.target.clone());
        let items: Vec<MenuItem> = vec![
            ("object-select-symbolic", "Mark all watched", {
                let (u, k) = (self.clone(), all.clone());
                Box::new(move || u.set_watched(k.clone(), true))
            }),
            ("edit-undo-symbolic", "Reset progress", {
                let (u, k) = (self.clone(), all);
                Box::new(move || u.reset(k.clone()))
            }),
            ("folder-open-symbolic", "Open folder", Box::new(move || open_folder(&folder))),
        ];
        for (icon, text, f) in items {
            let b = button(Some(icon), text, &["flat"]);
            let p = pop.clone();
            b.connect_clicked(move |_| {
                p.popdown();
                f();
            });
            menu.append(&b);
        }
        let mb = gtk::MenuButton::builder().icon_name("view-more-symbolic").popover(&pop).tooltip_text("More").build();
        mb.add_css_class("btn-glass-round");
        mb
    }

    fn episode_tile(&self, sh: &Show, e: &Entry, i: usize, art: &ArtSnap) -> gtk::Box {
        let card = vbox(10, &["tile", &format!("d{}", (i % 10))]);
        card.add_css_class(&tint(&sh.key));
        let is_next = sh.next == Some(i);
        if is_next {
            card.add_css_class("upnext");
        }
        if e.watched {
            card.add_css_class("is-watched");
        }
        let fr = match e.file() {
            Some(_) => {
                let fr = frame(280, 158, art.tile(e.file()), &["tile-img"]);
                fr.add_css_class("tile-frame");
                layer(&fr, "tile-scrim");
                fr
            }
            None => {
                let fr = frame(280, 158, None, &[]);
                fr.add_css_class("tile-frame");
                fr.add_css_class("missing-frame");
                let m = vbox(8, &[]);
                m.set_halign(gtk::Align::Center);
                m.set_valign(gtk::Align::Center);
                m.set_can_target(false);
                let ic = gtk::Image::from_icon_name("folder-download-symbolic");
                ic.set_pixel_size(28);
                ic.add_css_class("dim");
                m.append(&ic);
                m.append(&label("NOT DOWNLOADED", &["mono", "dim", "small"]));
                fr.add_overlay(&m);
                fr
            }
        };
        let top = hbox(6, &[]);
        top.set_valign(gtk::Align::Start);
        top.set_halign(gtk::Align::Start);
        top.set_margin_top(10);
        top.set_margin_start(10);
        top.set_can_target(false);
        top.append(&chip(&format!("E{:02}", e.ep), &[]));
        if is_next {
            top.append(&chip("UP NEXT", &["accent"]));
        }
        fr.add_overlay(&top);
        fr.add_overlay(&self.watch_toggle(e));

        if e.file().is_some() {
            let rest = sh.list[i..].to_vec();
            let pb = button(Some("media-playback-start-symbolic"), "", &["tile-play"]);
            pb.set_halign(gtk::Align::Center);
            pb.set_valign(gtk::Align::Center);
            let r2 = rest.clone();
            pb.connect_clicked(move |_| play(&r2));
            fr.add_overlay(&pb);
            if e.in_progress() && e.dur() > 0 {
                let p = gtk::ProgressBar::builder().fraction((e.pos as f64 / e.dur() as f64).min(1.0)).valign(gtk::Align::End).build();
                p.add_css_class("thin");
                p.set_can_target(false);
                fr.add_overlay(&p);
            }
            if e.dur() > 0 {
                let br = hbox(0, &[]);
                br.set_valign(gtk::Align::End);
                br.set_halign(gtk::Align::End);
                br.set_margin_end(10);
                br.set_margin_bottom(12);
                br.set_can_target(false);
                let t = if e.in_progress() { format!("{} LEFT", hms(e.dur().saturating_sub(e.pos))) } else { hms(e.dur()) };
                br.append(&chip(&t, &[]));
                fr.add_overlay(&br);
            }
            on_click(&fr, move || play(&rest));
        }
        card.append(&fr);
        let meta = vbox(3, &["tile-meta"]);
        let title = if e.title.is_empty() { format!("Episode {}", e.ep) } else { e.title.clone() };
        meta.append(&label(&title, &["tile-title"]));
        let mut sub = e.status();
        if let Some(f) = e.file() {
            if cfg().in_downloads(&f.path) {
                sub += " · Downloads";
            }
        }
        meta.append(&label(&sub, &["tile-sub"]));
        meta.set_size_request(280, -1);
        card.append(&meta);
        card
    }

    // ── moving ──

    fn move_groups(&self, btn: &gtk::Button, groups: Vec<MoveGroup>) {
        let need: u64 = groups.iter().flat_map(|g| &g.1).map(|f| f.size).sum();
        let free = gio::File::for_path(&cfg().library)
            .query_filesystem_info("filesystem::free", gio::Cancellable::NONE)
            .map(|i| i.attribute_uint64("filesystem::free"))
            .unwrap_or(u64::MAX);
        if need > free {
            return self.toast("Not enough free space in the library folder");
        }
        btn.set_sensitive(false);
        let what = if groups.len() == 1 { groups[0].0.clone() } else { format!("{} titles", groups.len()) };
        self.toast(&format!("Moving {what} ({})…", gb(need)));
        let u = self.clone();
        glib::spawn_future_local(async move {
            let res = gio::spawn_blocking(move || {
                let (mut moved, mut skipped, mut err) = (0, vec![], None);
                for (_, files, target) in &groups {
                    match library::move_files(files, target) {
                        Ok((m, s)) => {
                            moved += m;
                            skipped.extend(s);
                        }
                        Err(e) => {
                            err = Some(e.to_string());
                            break;
                        }
                    }
                }
                (moved, skipped, err)
            })
            .await;
            match res {
                Ok((moved, skipped, None)) => {
                    let mut msg = format!("Moved {moved} file{} into your library", if moved == 1 { "" } else { "s" });
                    if let Some(first) = skipped.first() {
                        msg += &format!(" · skipped {} ({first})", skipped.len());
                    }
                    u.toast(&msg);
                }
                Ok((moved, _, Some(e))) => u.toast(&format!("Moved {moved}, then failed: {e}")),
                Err(_) => u.toast("Move failed"),
            }
            u.rescan();
        });
    }
}

fn section(title: &str, sub: &str) -> gtk::Box {
    let b = vbox(4, &[]);
    b.append(&label(title, &["section-title"]));
    if !sub.is_empty() {
        b.append(&label(sub, &["mono", "section-sub"]));
    }
    b
}

fn grid() -> gtk::FlowBox {
    gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(true)
        .min_children_per_line(1)
        .max_children_per_line(8)
        .column_spacing(18)
        .row_spacing(26)
        .valign(gtk::Align::Start)
        .build()
}

pub fn run() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| {
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        let css = gtk::CssProvider::new();
        css.load_from_string(STYLE);
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
    });
    app.connect_activate(|app| {
        if let Some(w) = app.active_window() {
            w.present();
            return;
        }
        let (ui, win) = Ui::new(app);
        win.present();
        ui.rescan();
        // Dev aid: UPNEXT_SNAPSHOT=out.png [UPNEXT_SNAPSHOT_SHOW=<key>] renders the window to a PNG and quits.
        if let Ok(out) = std::env::var("UPNEXT_SNAPSHOT") {
            let show = std::env::var("UPNEXT_SNAPSHOT_SHOW").ok();
            let scroll: f64 = std::env::var("UPNEXT_SNAPSHOT_SCROLL").ok().and_then(|v| v.parse().ok()).unwrap_or(0.0);
            let (u, w, a) = (ui.clone(), win.clone(), app.clone());
            glib::timeout_add_seconds_local_once(6, move || {
                if let Some(key) = show {
                    u.open_detail(&key, &key);
                }
                u.scroller.vadjustment().set_value(scroll);
                glib::timeout_add_seconds_local_once(3, move || {
                    let (width, height) = (w.width(), w.height());
                    let snap = gtk::Snapshot::new();
                    gtk::WidgetPaintable::new(Some(&w)).snapshot(&snap, width as f64, height as f64);
                    if let (Some(node), Some(r)) = (snap.to_node(), w.native().and_then(|n| n.renderer())) {
                        let _ = r.render_texture(node, None).save_to_png(&out);
                    }
                    a.quit();
                });
            });
        }
    });
    let argv0 = std::env::args().next().unwrap_or_default();
    app.run_with_args(&[argv0])
}
