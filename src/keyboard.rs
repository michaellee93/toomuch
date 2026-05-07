use std::fs;
use std::io;
use std::os::fd::AsRawFd;

use evdev::{Device as EvDevice, EventType, Key};

use crate::login::LoginEvent;

pub fn find_keyboards() -> io::Result<Vec<EvDevice>> {
    let mut paths = fs::read_dir("/dev/input")?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("event"))
        })
        .collect::<Vec<_>>();

    paths.sort();

    let mut keyboards = Vec::new();

    for path in paths {
        let dev = match EvDevice::open(&path) {
            Ok(dev) => dev,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                eprintln!("skip input {path:?}: disappeared");
                continue;
            }
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                eprintln!("skip input {path:?}: permission denied");
                continue;
            }
            Err(e) => {
                eprintln!("skip input {path:?}: {e}");
                continue;
            }
        };

        if dev.supported_keys().is_some_and(|keys| {
            keys.contains(Key::KEY_A)
                && keys.contains(Key::KEY_Z)
                && keys.contains(Key::KEY_ENTER)
                && keys.contains(Key::KEY_BACKSPACE)
        }) {
            eprintln!(
                "keyboard: {:?} name={:?} phys={:?}",
                path,
                dev.name(),
                dev.physical_path(),
            );
            keyboards.push(dev);
        }
    }

    if keyboards.is_empty() {
        Err(io::Error::new(io::ErrorKind::NotFound, "no keyboard found"))
    } else {
        Ok(keyboards)
    }
}

pub fn set_evdev_nonblocking(dev: &evdev::Device) -> io::Result<()> {
    let fd = dev.as_raw_fd();

    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }

        if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

pub struct KeyboardInput {
    keyboards: Vec<EvDevice>,
    shift: bool,
    caps_lock: bool,
}

impl KeyboardInput {
    pub fn new(keyboards: Vec<EvDevice>) -> Self {
        let caps_lock = keyboards.iter().any(|keyboard| {
            keyboard
                .get_led_state()
                .is_ok_and(|leds| leds.contains(evdev::LedType::LED_CAPSL))
        });

        Self {
            keyboards,
            shift: false,
            caps_lock,
        }
    }

    pub fn read_events(&mut self) -> io::Result<Vec<LoginEvent>> {
        let mut login_events = Vec::new();

        for idx in 0..self.keyboards.len() {
            let key_events = match self.keyboards[idx].fetch_events() {
                Ok(events) => events
                    .filter(|ev| ev.event_type() == EventType::KEY)
                    .map(|ev| (Key(ev.code()), ev.value()))
                    .collect::<Vec<_>>(),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) if e.raw_os_error() == Some(libc::ENODEV) => {
                    eprintln!("keyboard disappeared: {e}");
                    continue;
                }
                Err(e) => return Err(e),
            };

            for (key, value) in key_events {
                self.update_modifiers(key, value);

                if value == 0 {
                    continue;
                }

                if let Some(login_event) = Self::map_key(key, self.shift, self.caps_lock) {
                    login_events.push(login_event);
                }
            }
        }

        Ok(login_events)
    }

    fn update_modifiers(&mut self, key: Key, value: i32) {
        match key {
            Key::KEY_LEFTSHIFT | Key::KEY_RIGHTSHIFT => {
                self.shift = value != 0;
            }
            Key::KEY_CAPSLOCK if value == 1 => {
                self.caps_lock = !self.caps_lock;
            }
            _ => {}
        }
    }

    fn map_key(key: Key, shift: bool, caps_lock: bool) -> Option<LoginEvent> {
        match key {
            Key::KEY_ENTER => Some(LoginEvent::Submit),
            Key::KEY_KPENTER => Some(LoginEvent::Submit),
            Key::KEY_BACKSPACE => Some(LoginEvent::Delete),
            Key::KEY_UP => Some(LoginEvent::PreviousField),
            Key::KEY_ESC => Some(LoginEvent::PreviousField),
            Key::KEY_SPACE => Some(LoginEvent::Char(' ')),
            key => Self::map_printable_key(key, shift, caps_lock).map(LoginEvent::Char),
        }
    }

    fn map_printable_key(key: Key, shift: bool, caps_lock: bool) -> Option<char> {
        if let Some(c) = Self::map_letter_key(key) {
            let upper = shift ^ caps_lock;
            return Some(if upper { c.to_ascii_uppercase() } else { c });
        }

        let c = match key {
            Key::KEY_1 => {
                if shift {
                    '!'
                } else {
                    '1'
                }
            }
            Key::KEY_2 => {
                if shift {
                    '@'
                } else {
                    '2'
                }
            }
            Key::KEY_3 => {
                if shift {
                    '#'
                } else {
                    '3'
                }
            }
            Key::KEY_4 => {
                if shift {
                    '$'
                } else {
                    '4'
                }
            }
            Key::KEY_5 => {
                if shift {
                    '%'
                } else {
                    '5'
                }
            }
            Key::KEY_6 => {
                if shift {
                    '^'
                } else {
                    '6'
                }
            }
            Key::KEY_7 => {
                if shift {
                    '&'
                } else {
                    '7'
                }
            }
            Key::KEY_8 => {
                if shift {
                    '*'
                } else {
                    '8'
                }
            }
            Key::KEY_9 => {
                if shift {
                    '('
                } else {
                    '9'
                }
            }
            Key::KEY_0 => {
                if shift {
                    ')'
                } else {
                    '0'
                }
            }
            Key::KEY_MINUS => {
                if shift {
                    '_'
                } else {
                    '-'
                }
            }
            Key::KEY_EQUAL => {
                if shift {
                    '+'
                } else {
                    '='
                }
            }
            Key::KEY_LEFTBRACE => {
                if shift {
                    '{'
                } else {
                    '['
                }
            }
            Key::KEY_RIGHTBRACE => {
                if shift {
                    '}'
                } else {
                    ']'
                }
            }
            Key::KEY_BACKSLASH => {
                if shift {
                    '|'
                } else {
                    '\\'
                }
            }
            Key::KEY_SEMICOLON => {
                if shift {
                    ':'
                } else {
                    ';'
                }
            }
            Key::KEY_APOSTROPHE => {
                if shift {
                    '"'
                } else {
                    '\''
                }
            }
            Key::KEY_GRAVE => {
                if shift {
                    '~'
                } else {
                    '`'
                }
            }
            Key::KEY_COMMA => {
                if shift {
                    '<'
                } else {
                    ','
                }
            }
            Key::KEY_DOT => {
                if shift {
                    '>'
                } else {
                    '.'
                }
            }
            Key::KEY_SLASH => {
                if shift {
                    '?'
                } else {
                    '/'
                }
            }
            Key::KEY_KP0 => '0',
            Key::KEY_KP1 => '1',
            Key::KEY_KP2 => '2',
            Key::KEY_KP3 => '3',
            Key::KEY_KP4 => '4',
            Key::KEY_KP5 => '5',
            Key::KEY_KP6 => '6',
            Key::KEY_KP7 => '7',
            Key::KEY_KP8 => '8',
            Key::KEY_KP9 => '9',
            Key::KEY_KPMINUS => '-',
            Key::KEY_KPPLUS => '+',
            Key::KEY_KPASTERISK => '*',
            Key::KEY_KPSLASH => '/',
            Key::KEY_KPDOT => '.',
            _ => return None,
        };

        Some(c)
    }

    fn map_letter_key(key: Key) -> Option<char> {
        const LETTERS: &[(Key, char)] = &[
            (Key::KEY_A, 'a'),
            (Key::KEY_B, 'b'),
            (Key::KEY_C, 'c'),
            (Key::KEY_D, 'd'),
            (Key::KEY_E, 'e'),
            (Key::KEY_F, 'f'),
            (Key::KEY_G, 'g'),
            (Key::KEY_H, 'h'),
            (Key::KEY_I, 'i'),
            (Key::KEY_J, 'j'),
            (Key::KEY_K, 'k'),
            (Key::KEY_L, 'l'),
            (Key::KEY_M, 'm'),
            (Key::KEY_N, 'n'),
            (Key::KEY_O, 'o'),
            (Key::KEY_P, 'p'),
            (Key::KEY_Q, 'q'),
            (Key::KEY_R, 'r'),
            (Key::KEY_S, 's'),
            (Key::KEY_T, 't'),
            (Key::KEY_U, 'u'),
            (Key::KEY_V, 'v'),
            (Key::KEY_W, 'w'),
            (Key::KEY_X, 'x'),
            (Key::KEY_Y, 'y'),
            (Key::KEY_Z, 'z'),
        ];

        LETTERS
            .iter()
            .find_map(|(letter_key, c)| (*letter_key == key).then_some(*c))
    }
}
