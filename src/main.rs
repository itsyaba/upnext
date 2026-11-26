mod art;
mod config;
mod library;
mod parse;
mod state;
mod ui;

fn main() -> gtk::glib::ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("--list") => {
            library::cli_list();
            gtk::glib::ExitCode::SUCCESS
        }
        Some("--help" | "-h") => {
            println!("upnext          open the window\nupnext --list   print shows and what's up next");
            gtk::glib::ExitCode::SUCCESS
        }
        _ => ui::run(),
    }
}
