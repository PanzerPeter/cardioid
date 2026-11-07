//! Cardioid: a filtered virtual microphone for PipeWire. It reads one physical
//! mic and nothing else.
//!
//! `cardioid`               open the window, starting the service if needed
//! `cardioid --background`  start the service only (used at login)
//!
//! The service owns the tray icon and the `pipewire -c` child that runs the
//! filters. The window is a separate process, so closing it to the tray frees
//! all of its memory while the filters keep running.

mod chain;
mod pw;
mod ui;

use chain::Settings;
use std::fs::{self, File, TryLockError};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::time::Duration;
use std::{env, thread};

enum Cmd {
    Show,
    Quit,
}

struct Tray(Sender<Cmd>);

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "cardioid".into()
    }
    fn title(&self) -> String {
        "Cardioid".into()
    }
    fn icon_name(&self) -> String {
        "cardioid".into()
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.0.send(Cmd::Show);
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: "Open Cardioid".into(),
                activate: Box::new(|t: &mut Self| drop(t.0.send(Cmd::Show))),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|t: &mut Self| drop(t.0.send(Cmd::Quit))),
                ..Default::default()
            }
            .into(),
        ]
    }
}

fn run_filters(conf_path: &Path) {
    loop {
        let settings = Settings::load();
        match fs::write(conf_path, chain::conf(&settings)) {
            Ok(()) => match pw::spawn_tied(Command::new("pipewire").arg("-c").arg(conf_path)) {
                Ok(mut child) => drop(child.wait()),
                Err(e) => eprintln!("pipewire: {e}"),
            },
            Err(e) => eprintln!("{}: {e}", conf_path.display()),
        }
        // Reached when PipeWire restarts or the chain fails to load.
        thread::sleep(Duration::from_secs(2));
    }
}

fn show_window(window: &mut Option<Child>) {
    if let Some(child) = window
        && matches!(child.try_wait(), Ok(None))
    {
        // Any input asks the open window to raise itself.
        if let Some(stdin) = &mut child.stdin {
            let _ = stdin.write_all(b"\n");
        }
        return;
    }
    let spawned = env::current_exe()
        .and_then(|exe| pw::spawn_tied(Command::new(exe).arg("--window").stdin(Stdio::piped())));
    match spawned {
        Ok(child) => *window = Some(child),
        Err(e) => eprintln!("window: {e}"),
    }
}

fn service(open_window: bool) -> Result<(), Box<dyn std::error::Error>> {
    let dir = pw::runtime_dir();
    fs::create_dir_all(&dir)?;
    let socket = dir.join("socket");
    let lock = File::create(dir.join("lock"))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            if open_window {
                UnixStream::connect(&socket)?.write_all(b"show")?;
            }
            return Ok(());
        }
        Err(TryLockError::Error(e)) => return Err(e.into()),
    }

    let mut settings = Settings::load();
    if settings.device.is_empty()
        && let Some(mic) = pw::default_mic()
    {
        settings.device = mic;
        settings.save()?;
    }

    let conf_path = dir.join("chain.conf");
    thread::spawn(move || run_filters(&conf_path));

    let (tx, rx) = mpsc::channel();
    let _ = fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)?;
    let socket_tx = tx.clone();
    thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = String::new();
            if stream.read_to_string(&mut request).is_ok() && request == "show" {
                let _ = socket_tx.send(Cmd::Show);
            }
        }
    });

    let _tray =
        ksni::blocking::TrayMethods::assume_sni_available(Tray(tx.clone()), true).spawn()?;

    if open_window {
        tx.send(Cmd::Show)?;
    }
    let mut window = None;
    for cmd in rx {
        match cmd {
            Cmd::Show => show_window(&mut window),
            Cmd::Quit => break,
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match env::args().nth(1).as_deref() {
        None => service(true),
        Some("--background") => service(false),
        Some("--window") => Ok(ui::run()?),
        Some(other) => Err(format!("unknown option {other}, expected --background").into()),
    }
}
