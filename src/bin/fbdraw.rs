use shlog::greetd_client::{GreetdCommand, GreetdResult, spawn_greetd_worker};
use shlog::keyboard::{KeyboardInput, find_keyboards, set_evdev_nonblocking};
use shlog::login::{LoginAction, LoginApp, LoginEvent};
use std::io;

use shlog::display::native::DrmDisplay;
use shlog::scene::Scene;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sock_path = std::env::var("GREETD_SOCK")
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "$GREETD_SOCK is not set"))?;

    let (greetd_tx, greetd_rx) = spawn_greetd_worker(sock_path);
    let mut keyboards = find_keyboards().map_err(|e| format!("find_keyboards: {e}"))?;
    for keyboard in &keyboards {
        set_evdev_nonblocking(keyboard).map_err(|e| format!("set_evdev_nonblocking: {e}"))?;
    }
    let mut kbd_in = KeyboardInput::new(std::mem::take(&mut keyboards));

    let mut display = DrmDisplay::new()?;
    let (w, h) = display.size();
    let cfg = shlog::Config::from_toml_file("./config.toml").unwrap_or_default();

    let scene;
    {
        let gl = display.gl()?;
        scene = Scene::new(&gl, &cfg, w, h)?;
    }
    let mut app = LoginApp::new();

    eprintln!("displaying animated shader — Ctrl-C to exit");
    'main: loop {
        while let Ok(result) = greetd_rx.try_recv() {
            let ev = match result {
                GreetdResult::AuthAccepted => LoginEvent::AuthAccepted,
                GreetdResult::AuthFailure(e) => {
                    eprintln!("greetd login failed: {e}");
                    LoginEvent::AuthFailure
                }
                GreetdResult::SessionStarted => LoginEvent::SessionStarted,
                GreetdResult::SessionFailed(e) => {
                    eprintln!("greetd session failed: {e}");
                    LoginEvent::SessionFailure
                }
            };

            if let Some(LoginAction::Exit) = app.handle_event(ev) {
                break 'main Ok(());
            }
        }

        if let Ok(events) = kbd_in.read_events() {
            for ev in events {
                match app.handle_event(ev) {
                    Some(LoginAction::Exit) => {
                        break 'main Ok(());
                    }
                    Some(LoginAction::Authenticate { username, password }) => {
                        if let Err(e) =
                            greetd_tx.send(GreetdCommand::Authenticate { username, password })
                        {
                            eprintln!("greetd worker stopped: {e}");
                            app.handle_event(LoginEvent::AuthFailure);
                        }
                    }
                    None => continue,
                }
            }
        }

        if let Some(LoginAction::Exit) = app.tick() {
            break 'main Ok(());
        }

        let view = app.view();

        {
            let gl = display.gl()?;
            scene.draw(&gl, &view)?;
        }
        display.present()?;
    }
}
