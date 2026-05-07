use std::io;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread;

use crate::{AuthMessageType, Request, Response, recv, send};

pub enum GreetdCommand {
    Authenticate { username: String, password: String },
}

pub enum GreetdResult {
    AuthAccepted,
    AuthFailure(String),
    SessionStarted,
    SessionFailed(String),
}

pub fn spawn_greetd_worker(
    sock_path: String,
) -> (mpsc::Sender<GreetdCommand>, mpsc::Receiver<GreetdResult>) {
    let (command_tx, command_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();

    thread::spawn(move || {
        for command in command_rx {
            match command {
                GreetdCommand::Authenticate { username, password } => {
                    let result = match UnixStream::connect(&sock_path) {
                        Ok(mut stream) => {
                            match greetd_login(&mut stream, &username, &password, &result_tx) {
                                Ok(()) => None,
                                Err(e) => Some(GreetdResult::AuthFailure(e.to_string())),
                            }
                        }
                        Err(e) => Some(GreetdResult::AuthFailure(format!(
                            "connect GREETD_SOCK {sock_path:?}: {e}"
                        ))),
                    };

                    let Some(result) = result else {
                        continue;
                    };

                    if result_tx.send(result).is_err() {
                        break;
                    }
                }
            }
        }
    });

    (command_tx, result_rx)
}

fn greetd_login(
    stream: &mut UnixStream,
    username: &str,
    password: &str,
    result_tx: &mpsc::Sender<GreetdResult>,
) -> io::Result<()> {
    const SESSION_CMD: &str = "/bin/bash";

    send(
        stream,
        &Request::CreateSession {
            username: username.to_owned(),
        },
    )?;

    loop {
        match recv(stream)? {
            Response::Success => break,
            Response::AuthMessage {
                auth_message_type,
                auth_message,
            } => {
                eprintln!("greetd auth message: {auth_message_type:?}: {auth_message}");
                let response = match auth_message_type {
                    AuthMessageType::Secret | AuthMessageType::Visible => Some(password.to_owned()),
                    AuthMessageType::Info => None,
                    AuthMessageType::Error => {
                        let _ = send(stream, &Request::CancelSession);
                        let _ = result_tx.send(GreetdResult::AuthFailure(auth_message));
                        return Ok(());
                    }
                };
                send(stream, &Request::PostAuthMessageResponse { response })?;
            }
            Response::Error {
                error_type,
                description,
            } => {
                let _ = send(stream, &Request::CancelSession);
                let message = format!("{error_type:?}: {description}");
                let _ = result_tx.send(GreetdResult::AuthFailure(message));
                return Ok(());
            }
        }
    }

    result_tx
        .send(GreetdResult::AuthAccepted)
        .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))?;

    send(
        stream,
        &Request::StartSession {
            cmd: vec![SESSION_CMD.to_owned()],
            env: vec![],
        },
    )?;

    match recv(stream)? {
        Response::Success => {
            result_tx
                .send(GreetdResult::SessionStarted)
                .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))?;
            Ok(())
        }
        Response::Error {
            error_type,
            description,
        } => {
            let _ = send(stream, &Request::CancelSession);
            let message = format!("start_session failed: {error_type:?}: {description}");
            let _ = result_tx.send(GreetdResult::SessionFailed(message.clone()));
            Ok(())
        }
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected greetd response after start_session: {other:?}"),
        )),
    }
}
