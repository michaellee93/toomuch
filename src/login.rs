use std::time::Duration;

const DOT_ANIMATION_INTERVAL: Duration = Duration::from_millis(180);
const DOT_ANIMATION_STEPS: u128 = 4;

#[derive(Debug)]
pub enum LoginState {
    UsernameInput { username: String },
    PasswordInput { username: String, password: String },
    Authenticating { username: String },
    StartingSession { username: String },
    Failure { username: String },
    Success,
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
    pub fn visual_state(&self) -> i32 {
        match self {
            LoginState::UsernameInput { .. } => 0,
            LoginState::PasswordInput { .. } => 1,
            LoginState::Authenticating { .. } => 2,
            LoginState::StartingSession { .. } => 3,
            LoginState::Failure { .. } => 4,
            LoginState::Success => 5,
        }
    }

    pub fn message(&self, elapsed: Duration) -> String {
        match self {
            LoginState::UsernameInput { username } => format!("Hello {}_", username),
            LoginState::PasswordInput { username, password } => format!(
                "Hello {}\n{}_",
                username,
                password.chars().map(|_| '*').collect::<String>()
            ),
            LoginState::Authenticating { username } => {
                format!(
                    "Hello {}\nauthenticating{}",
                    username,
                    animated_dots(elapsed)
                )
            }
            LoginState::StartingSession { username } => {
                format!(
                    "Hello {}\nstarting session{}",
                    username,
                    animated_dots(elapsed)
                )
            }
            LoginState::Failure { username } => format!("Hello {}\nlogin failed", username),
            LoginState::Success => String::new(),
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

fn animated_dots(elapsed: Duration) -> String {
    let step = elapsed.as_millis() / DOT_ANIMATION_INTERVAL.as_millis();
    ".".repeat((step % DOT_ANIMATION_STEPS) as usize)
}
