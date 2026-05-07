use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub background: BackgroundConfig,
    pub input: InputConfig,
    pub post: PostConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            background: BackgroundConfig::default(),
            input: InputConfig::default(),
            post: PostConfig::default(),
        }
    }
}

impl Config {
    pub fn from_toml_str(toml_src: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(toml_src)
    }

    pub fn from_toml_file(path: impl AsRef<Path>) -> io::Result<Self> {
        let toml_src = fs::read_to_string(path)?;
        Self::from_toml_str(&toml_src).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct BackgroundConfig {
    pub kind: BackgroundKind,
    pub path: Option<String>,
    pub color: Option<Color>,
    pub effect: Option<ShaderConfig>,
}

impl Default for BackgroundConfig {
    fn default() -> Self {
        Self {
            kind: BackgroundKind::Shader,
            path: None,
            color: None,
            effect: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        parse_hex_color(&value).map_err(serde::de::Error::custom)
    }
}

fn parse_hex_color(value: &str) -> Result<Color, String> {
    let hex = value
        .strip_prefix('#')
        .ok_or_else(|| "color must start with '#'".to_owned())?;

    match hex.len() {
        3 => {
            let r = parse_repeated_hex_digit(&hex[0..1])?;
            let g = parse_repeated_hex_digit(&hex[1..2])?;
            let b = parse_repeated_hex_digit(&hex[2..3])?;
            Ok(Color { r, g, b, a: 255 })
        }
        6 => Ok(Color {
            r: parse_hex_byte(&hex[0..2])?,
            g: parse_hex_byte(&hex[2..4])?,
            b: parse_hex_byte(&hex[4..6])?,
            a: 255,
        }),
        8 => Ok(Color {
            r: parse_hex_byte(&hex[0..2])?,
            g: parse_hex_byte(&hex[2..4])?,
            b: parse_hex_byte(&hex[4..6])?,
            a: parse_hex_byte(&hex[6..8])?,
        }),
        _ => Err("color must be #rgb, #rrggbb, or #rrggbbaa".to_owned()),
    }
}

fn parse_repeated_hex_digit(value: &str) -> Result<u8, String> {
    parse_hex_byte(&format!("{value}{value}"))
}

fn parse_hex_byte(value: &str) -> Result<u8, String> {
    u8::from_str_radix(value, 16).map_err(|_| format!("invalid hex color byte '{value}'"))
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundKind {
    Color,
    Image,
    Video,
    Shader,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct InputConfig {
    #[serde(rename = "type")]
    pub r#type: InputType,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            r#type: InputType::Floating,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputType {
    Floating,
    Terminal,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ShaderConfig {
    pub path: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PostConfig {
    pub path: Option<String>,
}
