use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{HtmlCanvasElement, KeyboardEvent, WebGl2RenderingContext};

use crate::Config;
use crate::login::{LoginAction, LoginApp, LoginEvent};
use crate::scene::Scene;

struct Runtime {
    gl: glow::Context,
    scene: Scene,
    app: LoginApp,
    w: u32,
    h: u32,
}

#[wasm_bindgen]
pub struct WebApp {
    _keydown: Closure<dyn FnMut(KeyboardEvent)>,
    _frame: Rc<RefCell<Option<Closure<dyn FnMut(f64)>>>>,
    runtime: Rc<RefCell<Runtime>>,
}

#[wasm_bindgen]
impl WebApp {
    #[wasm_bindgen(constructor)]
    pub fn new(canvas_id: &str, config_toml: &str) -> Result<WebApp, JsValue> {
        install_panic_hook();

        let window = web_sys::window().ok_or_else(|| js_err("window unavailable"))?;
        let document = window
            .document()
            .ok_or_else(|| js_err("document unavailable"))?;
        let canvas = document
            .get_element_by_id(canvas_id)
            .ok_or_else(|| js_err(format!("canvas #{canvas_id} not found")))?
            .dyn_into::<HtmlCanvasElement>()?;

        let (w, h) = resize_canvas(&window, &canvas);
        let webgl = canvas
            .get_context("webgl2")?
            .ok_or_else(|| js_err("WebGL2 unavailable"))?
            .dyn_into::<WebGl2RenderingContext>()?;
        let gl = glow::Context::from_webgl2_context(webgl);

        let cfg = parse_config(config_toml)?;
        let scene = Scene::new(&gl, &cfg, w, h).map_err(js_err)?;
        let runtime = Rc::new(RefCell::new(Runtime {
            gl,
            scene,
            app: LoginApp::new(),
            w,
            h,
        }));

        let key_runtime = runtime.clone();
        let keydown = Closure::wrap(Box::new(move |event: KeyboardEvent| {
            let Some(login_event) = map_key_event(&event) else {
                return;
            };

            event.prevent_default();

            let mut runtime = key_runtime.borrow_mut();
            match runtime.app.handle_event(login_event) {
                Some(LoginAction::Authenticate { username, password })
                    if username == "user" && password == "password" =>
                {
                    runtime.app.handle_event(LoginEvent::AuthAccepted);
                    runtime.app.handle_event(LoginEvent::SessionStarted);
                }
                Some(LoginAction::Authenticate { .. }) => {
                    runtime.app.handle_event(LoginEvent::AuthFailure);
                }
                Some(LoginAction::Exit) | None => {}
            }
        }) as Box<dyn FnMut(_)>);

        document.add_event_listener_with_callback("keydown", keydown.as_ref().unchecked_ref())?;

        let frame = Rc::new(RefCell::new(None));
        let next_frame = frame.clone();
        let frame_runtime = runtime.clone();
        let frame_window = window.clone();

        *next_frame.borrow_mut() = Some(Closure::wrap(Box::new(move |_time: f64| {
            {
                let mut runtime = frame_runtime.borrow_mut();
                runtime.app.tick();
                let view = runtime.app.view();
                if let Err(err) = runtime.scene.draw(&runtime.gl, &view) {
                    web_sys::console::error_1(&JsValue::from_str(&err.to_string()));
                }
            }

            if let Some(frame) = frame.borrow().as_ref() {
                let _ = request_animation_frame(&frame_window, frame);
            }
        }) as Box<dyn FnMut(f64)>));

        request_animation_frame(
            &window,
            next_frame
                .borrow()
                .as_ref()
                .ok_or_else(|| js_err("animation frame unavailable"))?,
        )?;

        Ok(WebApp {
            _keydown: keydown,
            _frame: next_frame,
            runtime,
        })
    }

    pub fn reload_config(&self, config_toml: &str) -> Result<(), JsValue> {
        let cfg = parse_config(config_toml)?;
        let mut runtime = self.runtime.borrow_mut();
        runtime.scene = Scene::new(&runtime.gl, &cfg, runtime.w, runtime.h).map_err(js_err)?;
        Ok(())
    }
}

#[wasm_bindgen]
pub fn start(canvas_id: &str, config_toml: &str) -> Result<WebApp, JsValue> {
    WebApp::new(canvas_id, config_toml)
}

#[wasm_bindgen]
pub fn start_default(canvas_id: &str) -> Result<WebApp, JsValue> {
    WebApp::new(canvas_id, "")
}

fn parse_config(config_toml: &str) -> Result<Config, JsValue> {
    if config_toml.trim().is_empty() {
        return Ok(Config::default());
    }

    Config::from_toml_str(config_toml).map_err(js_err)
}

fn resize_canvas(window: &web_sys::Window, canvas: &HtmlCanvasElement) -> (u32, u32) {
    let dpr = window.device_pixel_ratio().max(1.0);
    let css_w = canvas.client_width().max(1) as f64;
    let css_h = canvas.client_height().max(1) as f64;
    let w = (css_w * dpr).round() as u32;
    let h = (css_h * dpr).round() as u32;
    canvas.set_width(w);
    canvas.set_height(h);
    (w, h)
}

fn request_animation_frame(
    window: &web_sys::Window,
    frame: &Closure<dyn FnMut(f64)>,
) -> Result<i32, JsValue> {
    window.request_animation_frame(frame.as_ref().unchecked_ref())
}

fn map_key_event(event: &KeyboardEvent) -> Option<LoginEvent> {
    match event.key().as_str() {
        "Enter" => Some(LoginEvent::Submit),
        "Backspace" => Some(LoginEvent::Delete),
        "ArrowUp" | "Escape" => Some(LoginEvent::PreviousField),
        " " => Some(LoginEvent::Char(' ')),
        key if key.chars().count() == 1 => key.chars().next().map(LoginEvent::Char),
        _ => None,
    }
}

fn js_err(error: impl ToString) -> JsValue {
    JsValue::from_str(&error.to_string())
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        web_sys::console::error_1(&JsValue::from_str(&info.to_string()));
    }));
}
