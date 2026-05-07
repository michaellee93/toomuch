use std::io;
use std::os::unix::net::{UnixListener, UnixStream};

use shlog::{AuthMessageType, ErrorType, Request, Response, recv, send};

const DEFAULT_SOCK: &str = "/tmp/greetd-fake.sock";

fn handle(mut stream: UnixStream) -> io::Result<()> {
    let mut username: Option<String> = None;

    loop {
        let req: Request = match recv(&mut stream) {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        };

        eprintln!("<- {req:?}");

        let resp = match req {
            Request::CreateSession { username: u } => {
                username = Some(u);
                Response::AuthMessage {
                    auth_message_type: AuthMessageType::Secret,
                    auth_message: "Password".into(),
                }
            }

            Request::PostAuthMessageResponse { response } => {
                match response.as_deref() {
                    // Accept anything non-empty; reject empty/None.
                    Some("password") => Response::Success,
                    _ => Response::Error {
                        error_type: ErrorType::AuthError,
                        description: "Authentication failure".into(),
                    },
                }
            }

            Request::StartSession { cmd, env } => {
                eprintln!(
                    "   would start session for {:?}: cmd={cmd:?} env={env:?}",
                    username.as_deref().unwrap_or("<unknown>")
                );
                Response::Success
            }

            Request::CancelSession => {
                eprintln!("   session cancelled");
                return Ok(());
            }
        };

        eprintln!("-> {resp:?}");
        send(&mut stream, &resp)?;
    }
}

fn main() {
    let sock_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_SOCK.into());

    // Remove stale socket if present.
    let _ = std::fs::remove_file(&sock_path);

    let listener = UnixListener::bind(&sock_path).unwrap_or_else(|e| {
        eprintln!("greetd: bind {sock_path}: {e}");
        std::process::exit(1);
    });

    eprintln!("greetd: listening on {sock_path}");
    eprintln!("greetd: set GREETD_SOCK={sock_path} before running greet");

    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                if let Err(e) = handle(stream) {
                    eprintln!("greetd: connection error: {e}");
                }
            }
            Err(e) => eprintln!("greetd: accept error: {e}"),
        }
    }
}
