use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// Sent by the greeter (client) to greetd (server).
#[derive(Deserialize, Serialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    CreateSession { username: String },
    PostAuthMessageResponse { response: Option<String> },
    StartSession { cmd: Vec<String>, env: Vec<String> },
    CancelSession,
}

/// Sent by greetd (server) to the greeter (client).
#[derive(Deserialize, Serialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Success,
    Error {
        error_type: ErrorType,
        description: String,
    },
    AuthMessage {
        auth_message_type: AuthMessageType,
        auth_message: String,
    },
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ErrorType {
    AuthError,
    Error,
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum AuthMessageType {
    Visible,
    Secret,
    Info,
    Error,
}

pub fn send<T: Serialize>(stream: &mut UnixStream, msg: &T) -> io::Result<()> {
    let payload = serde_json::to_vec(msg).expect("serialisation failed");
    let len = payload.len() as u32;
    stream.write_all(&len.to_le_bytes())?;
    stream.write_all(&payload)
}

pub fn recv<T: DeserializeOwned>(stream: &mut UnixStream) -> io::Result<T> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    serde_json::from_slice(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
