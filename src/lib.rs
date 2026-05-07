use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub mod display;
pub mod greetd_client;
pub mod keyboard;
pub mod login;
pub mod scene;

const POST_FSHAD: &str = "precision highp float; \
             uniform sampler2D u_tex; \
	             uniform float u_time; \
	             uniform float u_aspect; \
	             uniform vec2 u_resolution; \
	             uniform int u_login_state; \
	             uniform float u_state_time; \
	             varying vec2 v_uv; \
                 float hash(float n) { n = fract(n * 0.1031); n *= n + 33.33; n *= n + n; return fract(n); } \
	             void main() { \
                 vec2 centered = v_uv * 2.0 - 1.0; \
                 centered.x *= u_aspect; \
                 float r2 = dot(centered, centered); \
	                 vec2 curved = centered * (1.0 + 0.018 * r2); \
	                 curved.x /= u_aspect; \
	                 vec2 uv = curved * 0.5 + 0.5; \
	                 vec2 feather = 4.0 / u_resolution; \
	                 vec2 edge = smoothstep(vec2(0.0), feather, uv) * smoothstep(vec2(0.0), feather, 1.0 - uv); \
	                 float screen_mask = edge.x * edge.y; \
	                 uv = clamp(uv, vec2(0.0), vec2(1.0)); \
	                 float burst = pow(max(0.0, sin(u_time * 0.7 + sin(u_time * 0.19) * 2.4)), 18.0); \
                         float failure_hit = 0.0; \
                         if (u_login_state == 4) { failure_hit = exp(-u_state_time * 2.8); } \
                         float glitch_band = floor(uv.y * 48.0); \
                         float glitch_tick = mod(floor(u_time * 18.0), 64.0); \
                         float glitch_gate = step(0.72, hash(glitch_band * 17.0 + glitch_tick)); \
                         float glitch_offset = (hash(glitch_band * 61.0 + glitch_tick * 7.0) - 0.5) * 0.09 * failure_hit * glitch_gate; \
		                 uv.x += sin(uv.y * 42.0 + u_time * 18.0) * 0.0025 * burst + glitch_offset; \
                         uv.x = fract(uv.x); \
		                 uv = clamp(uv, vec2(0.0), vec2(1.0)); \
                 vec2 px = 1.0 / u_resolution; \
                 float aberration = 0.0007 + 0.0012 * r2 + 0.006 * failure_hit; \
                 float red = texture2D(u_tex, uv + vec2(aberration, 0.0)).r; \
                 float green = texture2D(u_tex, uv).g; \
                 float blue = texture2D(u_tex, uv - vec2(aberration, 0.0)).b; \
                 vec4 base = vec4(red, green, blue, 1.0); \
                 vec3 glow = texture2D(u_tex, uv + vec2(px.x * 2.0, 0.0)).rgb; \
                 glow += texture2D(u_tex, uv - vec2(px.x * 2.0, 0.0)).rgb; \
                 glow += texture2D(u_tex, uv + vec2(0.0, px.y * 2.0)).rgb; \
                 glow += texture2D(u_tex, uv - vec2(0.0, px.y * 2.0)).rgb; \
                 glow *= 0.25; \
                 float scanline = 0.965 + 0.035 * sin(uv.y * u_resolution.y * 3.14159); \
                 float vignette = smoothstep(1.18, 0.35, length(centered)); \
                 float luma = dot(base.rgb, vec3(0.299, 0.587, 0.114)); \
                 float white_variation = 1.0 + 0.045 * sin(u_time * 1.7 + uv.y * 17.0 + uv.x * 5.0); \
                 vec3 color = base.rgb + glow * 0.22; \
                 color *= mix(1.0, white_variation, smoothstep(0.45, 0.85, luma)); \
		                 color *= scanline; \
		                 color *= 0.88 + 0.12 * vignette; \
                         color += vec3(0.02, 0.025, 0.04) * burst; \
                         color = mix(color, vec3(color.r + 0.45, color.g * 0.55, color.b * 0.55), failure_hit); \
		                 color = mix(vec3(0.03, 0.028, 0.04), color, screen_mask); \
	                 gl_FragColor = vec4(color, 1.0); \
	             }";

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
    pub effect: Option<BackgroundEffect>,
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
    pub font_size: usize,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            r#type: InputType::Terminal,
            font_size: 72,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputType {
    Floating,
    Terminal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BackgroundEffect {
    pub path: String,
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

impl PostConfig {
    pub fn shader_src(&self) -> Result<String, std::io::Error> {
        match &self.path {
            Some(path) => fs::read_to_string(path),
            None => Ok(POST_FSHAD.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_effect_is_optional() {
        let config = Config::from_toml_str("").unwrap();
        assert!(config.background.effect.is_none());
    }

    #[test]
    fn background_effect_is_fragment_shader_path() {
        let config = Config::from_toml_str(
            r#"
            [background.effect]
            path = "fragment.glsl"
            "#,
        )
        .unwrap();

        assert_eq!(
            config.background.effect,
            Some(BackgroundEffect {
                path: "fragment.glsl".to_owned(),
            }),
        );
    }
}
