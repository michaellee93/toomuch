use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
type ClockInstant = std::time::Instant;

#[cfg(target_arch = "wasm32")]
type ClockInstant = f64;

#[derive(Debug)]
pub enum LoginState {
    UsernameInput { username: String },
    PasswordInput { username: String, password: String },
    Authenticating { username: String },
    StartingSession { username: String },
    Failure { username: String },
    Success,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoginVisualState {
    UsernameInput,
    PasswordInput,
    Authenticating,
    StartingSession,
    Failure,
    Success,
}

impl LoginVisualState {
    pub fn shader_id(self) -> i32 {
        match self {
            Self::UsernameInput => 0,
            Self::PasswordInput => 1,
            Self::Authenticating => 2,
            Self::StartingSession => 3,
            Self::Failure => 4,
            Self::Success => 5,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum LoginEvent {
    Char(char),
    Delete,
    Submit,
    PreviousField,
    AuthAccepted,
    AuthFailure,
    SessionStarted,
    SessionFailure,
    TimerOver,
}

#[derive(Debug)]
pub enum LoginAction {
    Authenticate { username: String, password: String },
    Exit,
}

pub struct LoginTransition {
    pub state: LoginState,
    pub action: Option<LoginAction>,
    pub reset_timer: bool,
}

impl LoginTransition {
    fn stay(state: LoginState) -> Self {
        Self {
            state,
            action: None,
            reset_timer: false,
        }
    }

    fn change(state: LoginState) -> Self {
        Self {
            state,
            action: None,
            reset_timer: true,
        }
    }

    fn change_with_action(state: LoginState, action: LoginAction) -> Self {
        Self {
            state,
            action: Some(action),
            reset_timer: true,
        }
    }
}

impl Default for LoginState {
    fn default() -> Self {
        Self::UsernameInput {
            username: String::with_capacity(256),
        }
    }
}

impl LoginState {
    pub fn visual_state(&self) -> LoginVisualState {
        match self {
            LoginState::UsernameInput { .. } => LoginVisualState::UsernameInput,
            LoginState::PasswordInput { .. } => LoginVisualState::PasswordInput,
            LoginState::Authenticating { .. } => LoginVisualState::Authenticating,
            LoginState::StartingSession { .. } => LoginVisualState::StartingSession,
            LoginState::Failure { .. } => LoginVisualState::Failure,
            LoginState::Success => LoginVisualState::Success,
        }
    }

    pub fn update(self, ev: LoginEvent) -> LoginTransition {
        match (self, ev) {
            (LoginState::UsernameInput { mut username }, LoginEvent::Char(c)) => {
                username.push(c);
                LoginTransition::stay(LoginState::UsernameInput { username })
            }
            (LoginState::UsernameInput { mut username }, LoginEvent::Delete) => {
                username.pop();
                LoginTransition::stay(LoginState::UsernameInput { username })
            }
            (LoginState::UsernameInput { username }, LoginEvent::Submit)
                if !username.is_empty() =>
            {
                LoginTransition::change(LoginState::PasswordInput {
                    username,
                    password: String::new(),
                })
            }
            (
                LoginState::PasswordInput {
                    username,
                    mut password,
                },
                LoginEvent::Char(c),
            ) => {
                password.push(c);
                LoginTransition::stay(LoginState::PasswordInput { username, password })
            }
            (
                LoginState::PasswordInput {
                    username,
                    mut password,
                },
                LoginEvent::Delete,
            ) => {
                password.pop();
                LoginTransition::stay(LoginState::PasswordInput { username, password })
            }
            (LoginState::PasswordInput { username, password }, LoginEvent::Submit) => {
                LoginTransition::change_with_action(
                    LoginState::Authenticating {
                        username: username.clone(),
                    },
                    LoginAction::Authenticate { username, password },
                )
            }
            (
                LoginState::PasswordInput {
                    username,
                    password: _,
                },
                LoginEvent::PreviousField,
            ) => LoginTransition::change(LoginState::UsernameInput { username }),
            (LoginState::Authenticating { username }, LoginEvent::AuthAccepted) => {
                LoginTransition::change(LoginState::StartingSession { username })
            }
            (LoginState::StartingSession { username: _ }, LoginEvent::SessionStarted) => {
                LoginTransition::change(LoginState::Success)
            }
            (LoginState::Authenticating { username }, LoginEvent::AuthFailure) => {
                LoginTransition::change(LoginState::Failure { username })
            }
            (LoginState::StartingSession { username }, LoginEvent::SessionFailure) => {
                LoginTransition::change(LoginState::Failure { username })
            }
            (LoginState::Failure { username }, LoginEvent::Char(c)) => {
                let password = String::from(c);
                LoginTransition::change(LoginState::PasswordInput { username, password })
            }
            (LoginState::Failure { username }, LoginEvent::PreviousField) => {
                LoginTransition::change(LoginState::UsernameInput { username })
            }
            (LoginState::Success, LoginEvent::TimerOver) => {
                LoginTransition::change_with_action(LoginState::Success, LoginAction::Exit)
            }
            (s, _) => LoginTransition::stay(s),
        }
    }
}

pub struct LoginApp {
    state: LoginState,
    start: ClockInstant,
    state_start: ClockInstant,
}

impl LoginApp {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn handle_event(&mut self, event: LoginEvent) -> Option<LoginAction> {
        let state = std::mem::take(&mut self.state);
        let transition = state.update(event);
        self.state = transition.state;
        if transition.reset_timer {
            self.state_start = clock_now();
        }
        transition.action
    }

    pub fn tick(&mut self) -> Option<LoginAction> {
        if clock_elapsed(self.state_start) > Duration::from_millis(300) {
            self.handle_event(LoginEvent::TimerOver)
        } else {
            None
        }
    }

    pub fn view(&self) -> LoginView {
        let state_elapsed = clock_elapsed(self.state_start);
        let (username, password_mask) = match &self.state {
            LoginState::UsernameInput { username }
            | LoginState::Authenticating { username }
            | LoginState::StartingSession { username }
            | LoginState::Failure { username } => (username.clone(), String::new()),
            LoginState::PasswordInput { username, password } => (
                username.clone(),
                password.chars().map(|_| '*').collect::<String>(),
            ),
            LoginState::Success => (String::new(), String::new()),
        };
        LoginView {
            username,
            password_mask,
            time: clock_elapsed(self.start).as_secs_f32(),
            state: self.state.visual_state(),
            state_time: state_elapsed.as_secs_f32(),
        }
    }
}

impl Default for LoginApp {
    fn default() -> Self {
        Self {
            state: LoginState::default(),
            start: clock_now(),
            state_start: clock_now(),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn clock_now() -> ClockInstant {
    std::time::Instant::now()
}

#[cfg(not(target_arch = "wasm32"))]
fn clock_elapsed(start: ClockInstant) -> Duration {
    start.elapsed()
}

#[cfg(target_arch = "wasm32")]
fn clock_now() -> ClockInstant {
    js_sys::Date::now() / 1000.0
}

#[cfg(target_arch = "wasm32")]
fn clock_elapsed(start: ClockInstant) -> Duration {
    Duration::from_secs_f64((clock_now() - start).max(0.0))
}

pub struct LoginView {
    pub username: String,
    pub password_mask: String,
    pub time: f32,
    pub state: LoginVisualState,
    pub state_time: f32,
}
