use std::io::{self, Write};
use std::os::unix::net::UnixStream;

use shlog::{AuthMessageType, Request, Response, recv, send};

fn prompt(label: &str) -> String {
    print!("{label}");
    io::stdout().flush().unwrap();
    let mut line = String::new();
    io::stdin().read_line(&mut line).unwrap();
    line.trim_end_matches('\n').to_owned()
}

fn prompt_secret(label: &str) -> String {
    use std::os::fd::AsRawFd;
    let fd = io::stdin().as_raw_fd();
    let mut old: libc::termios = unsafe { std::mem::zeroed() };
    unsafe { libc::tcgetattr(fd, &mut old) };
    let mut raw = old;
    raw.c_lflag &= !libc::ECHO;
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) };

    let value = prompt(label);

    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &old) };
    println!();
    value
}

fn run() -> io::Result<()> {
    let sock_path = std::env::var("GREETD_SOCK")
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "$GREETD_SOCK is not set"))?;

    let mut stream = UnixStream::connect(&sock_path)?;

    let username = prompt("Username: ");
    send(&mut stream, &Request::CreateSession { username })?;

    loop {
        match recv(&mut stream)? {
            Response::Success => break,

            Response::AuthMessage {
                auth_message_type,
                auth_message,
            } => {
                let label = format!("{auth_message}: ");
                let response = match auth_message_type {
                    AuthMessageType::Secret => Some(prompt_secret(&label)),
                    AuthMessageType::Visible => Some(prompt(&label)),
                    AuthMessageType::Info => {
                        println!("Info: {auth_message}");
                        None
                    }
                    AuthMessageType::Error => {
                        eprintln!("Auth error: {auth_message}");
                        None
                    }
                };
                send(&mut stream, &Request::PostAuthMessageResponse { response })?;
            }

            Response::Error {
                error_type,
                description,
            } => {
                send(&mut stream, &Request::CancelSession)?;
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("{error_type:?}: {description}"),
                ));
            }
        }
    }

    let cmd = vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())];
    send(&mut stream, &Request::StartSession { cmd, env: vec![] })?;

    match recv(&mut stream)? {
        Response::Success => Ok(()),
        Response::Error { description, .. } => Err(io::Error::new(
            io::ErrorKind::Other,
            format!("start_session failed: {description}"),
        )),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected response: {other:?}"),
        )),
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("greet: {e}");
        std::process::exit(1);
    }
}
