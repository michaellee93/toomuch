use font8x8::legacy::BASIC_LEGACY;
use glow::{COLOR_BUFFER_BIT, HasContext, NativeBuffer, NativeProgram, NativeTexture};
use shlog::greetd_client::{GreetdCommand, GreetdResult, spawn_greetd_worker};
use shlog::keyboard::{KeyboardInput, find_keyboards, set_evdev_nonblocking};
use shlog::login::{LoginAction, LoginEvent, LoginState};
use std::collections::HashMap;
use std::io;
use std::time::{Duration, Instant};

use shlog::display::native::DrmDisplay;

// ---------------------------------------------------------------------------
// Software-render "Hello, world!" into an RGBA pixel buffer using font8x8.
// bit 0 of each glyph byte is the leftmost pixel (font8x8 convention).
// UV origin is at the top-left for the generated pixel buffer.
// ---------------------------------------------------------------------------

fn text_to_rgba(text: &str, scale: usize, w: usize, h: usize, bg: [u8; 4], fg: [u8; 4]) -> Vec<u8> {
    //    const BG: [u8; 4] = [0x1e, 0x1e, 0x2e, 0xff]; // catppuccin base
    //    const FG: [u8; 4] = [0xcd, 0xd6, 0xf4, 0xff]; // catppuccin text
    let mut buf: Vec<u8> = bg.iter().copied().cycle().take(w * h * 4).collect();
    let cw = 8 * scale;
    let ox = w.saturating_sub(text.len() * cw) / 2;
    let oy = h.saturating_sub(8 * scale) / 2;
    for (i, c) in text.chars().enumerate() {
        if !c.is_ascii() {
            continue;
        }
        let glyph = BASIC_LEGACY[c as usize];
        for (row, &byte) in glyph.iter().enumerate() {
            for col in 0..8usize {
                if (byte >> col) & 1 == 0 {
                    continue;
                }
                for sy in 0..scale {
                    for sx in 0..scale {
                        let px = ox + i * cw + col * scale + sx;
                        let py = oy + row * scale + sy;
                        if px < w && py < h {
                            let idx = (py * w + px) * 4;
                            buf[idx..idx + 4].copy_from_slice(&fg);
                        }
                    }
                }
            }
        }
    }
    buf
}

struct FontAtlas {
    rgba: Vec<u8>,
    w: usize, // pixels
    h: usize, // pixels
    glyph_map: HashMap<char, Glyph>,
}

impl FontAtlas {
    fn get_glyph(&self, c: char) -> Option<&Glyph> {
        self.glyph_map.get(&c)
    }
}

struct Glyph {
    w: usize,
    h: usize,
    advance_x: usize,
    u0: f32,
    u1: f32,
    v0: f32,
    v1: f32,
}

fn draw_glyph(
    buf: &mut [u8],
    buf_w: usize,
    buf_h: usize,
    glyph: [u8; 8],
    x: usize,
    y: usize,
    scale: usize,
    color: [u8; 4],
) {
    for (row, &byte) in glyph.iter().enumerate() {
        for col in 0..8usize {
            if (byte >> col) & 1 == 0 {
                continue;
            }

            for sy in 0..scale {
                for sx in 0..scale {
                    let px = x + col * scale + sx;
                    let py = y + row * scale + sy;
                    if px < buf_w && py < buf_h {
                        let idx = (py * buf_w + px) * 4;
                        buf[idx..idx + 4].copy_from_slice(&color);
                    }
                }
            }
        }
    }
}
/// scale is a scalar value that multiplies the 8x8 font by that so 3 = 24px
fn create_atlas(scale: usize) -> FontAtlas {
    const COLS: usize = 16;
    const ROWS: usize = 8;
    const GLYPH_SIZE: usize = 8;
    const FG: [u8; 4] = [0xff, 0xff, 0xff, 0xff];

    let cell = GLYPH_SIZE * scale;
    let w = COLS * cell;
    let h = ROWS * cell;
    let mut rgba = vec![0u8; w * h * 4];
    let mut glyph_map = HashMap::new();

    for code in 0u8..=127 {
        let ch = code as char;
        let atlas_col = code as usize % COLS;
        let atlas_row = code as usize / COLS;
        let x = atlas_col * cell;
        let y = atlas_row * cell;

        draw_glyph(
            &mut rgba,
            w,
            h,
            BASIC_LEGACY[code as usize],
            x,
            y,
            scale,
            FG,
        );

        glyph_map.insert(
            ch,
            Glyph {
                w: cell,
                h: cell,
                advance_x: cell,
                u0: x as f32 / w as f32,
                u1: (x + cell) as f32 / w as f32,
                v0: (y as f32 + 0.5) / h as f32,
                v1: ((y + cell) as f32 - 0.5) / h as f32,
            },
        );
    }

    FontAtlas {
        rgba,
        w,
        h,
        glyph_map,
    }
}

unsafe fn compile_shader(
    gl: &glow::Context,
    kind: u32,
    source: &str,
) -> Result<glow::Shader, Box<dyn std::error::Error>> {
    let shader = unsafe { gl.create_shader(kind)? };
    unsafe {
        gl.shader_source(shader, source);
        gl.compile_shader(shader);
    }

    if unsafe { !gl.get_shader_compile_status(shader) } {
        let log = unsafe { gl.get_shader_info_log(shader) };
        unsafe { gl.delete_shader(shader) };
        return Err(format!("shader compile failed:\n{log}").into());
    }

    Ok(shader)
}

unsafe fn link_program(
    gl: &glow::Context,
    vs: glow::Shader,
    fs: glow::Shader,
) -> Result<glow::Program, Box<dyn std::error::Error>> {
    let program = unsafe { gl.create_program()? };
    unsafe {
        gl.attach_shader(program, vs);
        gl.attach_shader(program, fs);
        gl.link_program(program);
    }

    if unsafe { !gl.get_program_link_status(program) } {
        let log = unsafe { gl.get_program_info_log(program) };
        unsafe { gl.delete_program(program) };
        return Err(format!("program link failed:\n{log}").into());
    }

    Ok(program)
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sock_path = std::env::var("GREETD_SOCK")
        .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "$GREETD_SOCK is not set"))?;

    let (greetd_tx, greetd_rx) = spawn_greetd_worker(sock_path);
    let mut keyboards = find_keyboards().map_err(|e| format!("find_keyboards: {e}"))?;
    for keyboard in &keyboards {
        set_evdev_nonblocking(keyboard).map_err(|e| format!("set_evdev_nonblocking: {e}"))?;
    }
    let mut kbd_in = KeyboardInput::new(std::mem::take(&mut keyboards));

    let mut display = DrmDisplay::new()?;
    let (w, h) = display.size();

    let bg_scene;
    let text_scene;
    let post_scene;
    {
        let gl = display.gl()?;
        bg_scene = BackgroundScene::new(&gl, w, h)?;
        text_scene = TextScene::new(&gl, 7)?;
        post_scene = PostprocScene::new(&gl)?;
    }
    let start = Instant::now();

    // app state
    let mut state = LoginState::default();
    let mut this_state = Instant::now();

    eprintln!("displaying animated shader — Ctrl-C to exit");
    'main: loop {
        while let Ok(result) = greetd_rx.try_recv() {
            let ev = match result {
                GreetdResult::AuthAccepted => LoginEvent::AuthAccepted,
                GreetdResult::AuthFailure(e) => {
                    eprintln!("greetd login failed: {e}");
                    LoginEvent::AuthFailure
                }
                GreetdResult::SessionStarted => LoginEvent::SessionStarted,
                GreetdResult::SessionFailed(e) => {
                    eprintln!("greetd session failed: {e}");
                    LoginEvent::SessionFailure
                }
            };

            let transition = state.update(ev);
            state = transition.state;
            if transition.reset_timer {
                this_state = Instant::now();
            }
            if let Some(LoginAction::Exit) = transition.action {
                break 'main Ok(());
            }
        }

        if let Ok(events) = kbd_in.read_events() {
            for ev in events {
                let transition = state.update(ev);
                state = transition.state;
                if transition.reset_timer {
                    this_state = Instant::now();
                }
                match transition.action {
                    Some(LoginAction::Exit) => {
                        break 'main Ok(());
                    }
                    Some(LoginAction::Authenticate { username, password }) => {
                        if let Err(e) =
                            greetd_tx.send(GreetdCommand::Authenticate { username, password })
                        {
                            eprintln!("greetd worker stopped: {e}");
                            let transition = state.update(LoginEvent::AuthFailure);
                            state = transition.state;
                            if transition.reset_timer {
                                this_state = Instant::now();
                            }
                        }
                    }
                    None => continue,
                }
            }
        }

        if this_state.elapsed() > Duration::from_millis(300) {
            let transition = state.update(LoginEvent::TimerOver);
            state = transition.state;
            if transition.reset_timer {
                this_state = Instant::now();
            }
            if let Some(LoginAction::Exit) = transition.action {
                //restore_crtc(&gbm, crtc_h, conn_h, &original_crtc, &mut current_bo)?;
                break 'main Ok(());
            }
        }

        let msg = state.message(this_state.elapsed());

        let t = start.elapsed().as_secs_f32();
        let state_time = this_state.elapsed().as_secs_f32();

        {
            let gl = display.gl()?;
            bg_scene.draw(&gl);
            text_scene.draw(&gl, bg_scene.scene_fbo, &msg, w, h, t)?;
            post_scene.draw(
                &gl,
                bg_scene.scene_tex,
                w as f32,
                h as f32,
                t,
                state.visual_state(),
                state_time,
            )?;
        }
        display.present()?;
    }
}

struct BackgroundScene {
    vbo: glow::NativeBuffer,
    scene_tex: glow::NativeTexture,
    scene_fbo: glow::NativeFramebuffer,
    w: u32,
    h: u32,
    scene_program: glow::NativeProgram,
    scene_uv: u32,
    scene_pos: u32,
}

impl BackgroundScene {
    const BG_VSHAD: &str = "attribute vec2 a_pos; \
    attribute vec2 a_uv; \
    varying vec2 v_uv; \
    void main() { \
        gl_Position = vec4(a_pos, 0.0, 1.0); \
        v_uv = a_uv; \
    }";

    //gl_FragColor = vec4(0.117647, 0.117647, 0.180392 + ((v_uv.x + u_time) / 10000.0) , 1.0); \
    //gl_FragColor = vec4(v_uv, 0.4 + sin(u_time), 1.0);
    const BG_FSHAD: &str = "precision mediump float; \
    varying vec2 v_uv; \
    uniform float u_time; \
    void main() { \
        gl_FragColor = vec4(0.117647, 0.117647, 0.180392 + ((v_uv.x + u_time) / 10000.0) , 1.0); \
    }";

    fn new(gl: &glow::Context, w: u32, h: u32) -> Result<Self, Box<dyn std::error::Error>> {
        #[rustfmt::skip]
        let verts: [f32; 16] = [
            -1.0,  1.0,  0.0, 1.0,
             1.0,  1.0,  1.0, 1.0,
            -1.0, -1.0,  0.0, 0.0,
             1.0, -1.0,  1.0, 0.0,
        ];

        let bg = unsafe {
            let verts_bytes = std::slice::from_raw_parts(
                verts.as_ptr() as *const u8,
                verts.len() * std::mem::size_of::<f32>(),
            );
            let vbo = gl.create_buffer()?;
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, verts_bytes, glow::STATIC_DRAW);

            // construct fb on the gpu now
            let scene_tex = gl.create_texture()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(scene_tex));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA as i32,
                w as i32,
                h as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                None,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::NEAREST as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::NEAREST as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );

            let scene_fbo = gl.create_framebuffer()?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(scene_fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(scene_tex),
                0,
            );
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            assert_eq!(status, glow::FRAMEBUFFER_COMPLETE);

            let scene_vshad = compile_shader(&gl, glow::VERTEX_SHADER, BackgroundScene::BG_VSHAD)?;
            let scene_fshad =
                compile_shader(&gl, glow::FRAGMENT_SHADER, BackgroundScene::BG_FSHAD)?;
            let scene_program = link_program(&gl, scene_vshad, scene_fshad)?;
            gl.delete_shader(scene_vshad);
            gl.delete_shader(scene_fshad);

            let scene_pos = gl
                .get_attrib_location(scene_program, "a_pos")
                .expect("a_pos");
            let scene_uv = gl.get_attrib_location(scene_program, "a_uv").expect("a_uv");
            gl.enable_vertex_attrib_array(scene_pos);
            gl.vertex_attrib_pointer_f32(scene_pos, 2, glow::FLOAT, false, 16, 0);
            gl.enable_vertex_attrib_array(scene_uv);
            gl.vertex_attrib_pointer_f32(scene_uv, 2, glow::FLOAT, false, 16, 8);
            Ok(Self {
                scene_fbo,
                scene_program,
                scene_tex,
                vbo,
                h,
                w,
                scene_pos,
                scene_uv,
            })
        };
        bg
    }

    fn draw(&self, gl: &glow::Context) {
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.scene_fbo));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            gl.enable_vertex_attrib_array(self.scene_pos);
            gl.vertex_attrib_pointer_f32(self.scene_pos, 2, glow::FLOAT, false, 16, 0);
            gl.enable_vertex_attrib_array(self.scene_uv);
            gl.vertex_attrib_pointer_f32(self.scene_uv, 2, glow::FLOAT, false, 16, 8);
            // draw the base scene
            gl.viewport(0, 0, self.w as i32, self.h as i32);
            gl.use_program(Some(self.scene_program));
            gl.clear(COLOR_BUFFER_BIT);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        }
    }
}

struct TextScene {
    text_program: NativeProgram,
    text_vbo: NativeBuffer,
    atlas_text: NativeTexture,
    atlas: FontAtlas,
    scale: usize,
    text_res_loc: glow::UniformLocation,
    text_font_loc: glow::UniformLocation,
    text_color_loc: glow::UniformLocation,
    text_translate_loc: glow::UniformLocation,
    text_time_loc: glow::UniformLocation,
}
impl TextScene {
    const TEXT_VSHAD: &str = "precision mediump float; \
    attribute vec2 a_pos; \
    attribute vec2 a_uv; \
    attribute float a_blink; \
    varying vec2 v_uv; \
    varying float v_blink; \
    uniform vec2 u_resolution; \
    uniform vec2 u_translate; \
    void main() { \
        v_blink = a_blink; \
        vec2 clip = vec2( \
            (u_translate.x + a_pos.x) / u_resolution.x * 2.0 - 1.0, \
            (u_translate.y + 1.0 - a_pos.y) / u_resolution.y * 2.0 \
        ); \
        gl_Position = vec4(clip, 0.0, 1.0); \
        v_uv = a_uv; \
    } \
";
    const TEXT_FSHAD: &str = "precision mediump float; \
    uniform sampler2D u_font; \
    uniform vec4 u_color; \
    uniform float u_time; \
    varying vec2 v_uv; \
    varying float v_blink; \
    void main() { \
       float alpha = texture2D(u_font, v_uv).a; \
        if (v_blink > 0.0) {

    alpha *= step(0.5, fract(u_time * 1.50));
    }
       gl_FragColor = vec4(u_color.rgb, u_color.a * alpha ); \
    } \
";

    fn new(gl: &glow::Context, scale: usize) -> Result<Self, Box<dyn std::error::Error>> {
        unsafe {
            // create text rendering prog
            let text_vshad = compile_shader(&gl, glow::VERTEX_SHADER, TextScene::TEXT_VSHAD)?;
            let text_fshad = compile_shader(&gl, glow::FRAGMENT_SHADER, TextScene::TEXT_FSHAD)?;
            let text_program = link_program(&gl, text_vshad, text_fshad)?;
            gl.delete_shader(text_vshad);
            gl.delete_shader(text_fshad);

            let atlas = create_atlas(scale);
            let atlas_text = gl.create_texture()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(atlas_text));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA as i32,
                atlas.w as i32,
                atlas.h as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                Some(&atlas.rgba),
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::NEAREST as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::NEAREST as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            //  // resolution
            let text_res_loc = gl
                .get_uniform_location(text_program, "u_resolution")
                .expect("couldn't get uniform");
            let text_font_loc = gl
                .get_uniform_location(text_program, "u_font")
                .expect("u_font");
            let text_color_loc = gl
                .get_uniform_location(text_program, "u_color")
                .expect("u_color");
            let text_translate_loc = gl
                .get_uniform_location(text_program, "u_translate")
                .expect("u_translate");
            let text_time_loc = gl
                .get_uniform_location(text_program, "u_time")
                .expect("u_time");

            let text_vbo = gl.create_buffer()?;

            Ok(Self {
                atlas_text,
                text_vbo,
                scale,
                atlas,
                text_program,
                text_res_loc,
                text_font_loc,
                text_color_loc,
                text_translate_loc,
                text_time_loc,
            })
        }
    }

    fn draw(
        &self,
        gl: &glow::Context,
        scene_fbo: glow::NativeFramebuffer,
        text: &str,
        w: u32,
        h: u32,
        t: f32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(scene_fbo));
            //  // construct the vertices from the
            let display_text: Vec<char> =
                //"Welcome back cuz\nEnter your password _".chars().collect();
                text.chars().collect();
            let mut text_verts: Vec<f32> = Vec::with_capacity(display_text.len() * 6 * 4);
            let line_height: usize = self.scale * 8 + 20;
            let mut cursor_x = 0;
            let mut cursor_y = 0;
            let mut width = 0;
            let mut height = 0;
            for c in display_text.iter() {
                if *c == '\n' {
                    cursor_x = 0;
                    cursor_y += line_height as usize;
                    continue;
                }
                let blink = if *c == '_' { 1.0 } else { 0.0 };

                let glyph = self.atlas.get_glyph(*c).expect("UNRENDERABLE CHAR");
                let x0 = cursor_x as f32;
                let x1 = (cursor_x + glyph.w) as f32;
                let y0 = cursor_y as f32;
                let y1 = (cursor_y + glyph.h) as f32;
                width = width.max(x1 as u32);
                height = y1 as u32;

                #[rustfmt::skip]
              text_verts.extend_from_slice(&[
                  x0, y0, glyph.u0, glyph.v0, blink,
                  x1, y0, glyph.u1, glyph.v0, blink,
                  x0, y1, glyph.u0, glyph.v1, blink,

                  x1, y0, glyph.u1, glyph.v0, blink,
                  x1, y1, glyph.u1, glyph.v1, blink,
                  x0, y1, glyph.u0, glyph.v1, blink,
              ]);

                cursor_x += glyph.advance_x;
            }
            let text_verts_bytes = std::slice::from_raw_parts(
                text_verts.as_ptr() as *const u8,
                text_verts.len() * std::mem::size_of::<f32>(),
            );
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.text_vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, text_verts_bytes, glow::STATIC_DRAW);

            gl.use_program(Some(self.text_program));
            //  // attr4ibuts
            let text_pos = gl
                .get_attrib_location(self.text_program, "a_pos")
                .expect("a_pos");
            let text_uv = gl
                .get_attrib_location(self.text_program, "a_uv")
                .expect("a_uv");
            let blink_loc = gl
                .get_attrib_location(self.text_program, "a_blink")
                .expect("a_blink");
            gl.enable_vertex_attrib_array(text_pos);
            gl.vertex_attrib_pointer_f32(text_pos, 2, glow::FLOAT, false, 20, 0);
            gl.enable_vertex_attrib_array(text_uv);
            gl.vertex_attrib_pointer_f32(text_uv, 2, glow::FLOAT, false, 20, 8);
            gl.enable_vertex_attrib_array(blink_loc);
            gl.vertex_attrib_pointer_f32(blink_loc, 1, glow::FLOAT, false, 20, 16);

            gl.uniform_2_f32(
                Some(&self.text_translate_loc),
                (w.saturating_sub(width) / 2) as f32,
                height as f32 / 2.0,
            );

            gl.uniform_2_f32(Some(&self.text_res_loc), w as f32, h as f32);

            gl.viewport(0, 0, w as i32, h as i32);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.atlas_text));
            gl.uniform_1_i32(Some(&self.text_font_loc), 0);
            gl.uniform_1_f32(Some(&self.text_time_loc), t);
            gl.uniform_4_f32(
                Some(&self.text_color_loc),
                0.8039216,
                0.8392157,
                0.95686275,
                1.0,
            );
            gl.enable(glow::BLEND);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
            gl.draw_arrays(glow::TRIANGLES, 0, (text_verts.len() / 5) as i32);
            gl.disable(glow::BLEND);
        }
        Ok(())
    }
}

struct PostprocScene {
    prog: glow::NativeProgram,
    vbo: glow::NativeBuffer,
    tex_loc: glow::UniformLocation,
    time_loc: glow::UniformLocation,
    aspect_loc: glow::UniformLocation,
    resolution_loc: glow::UniformLocation,
    login_state_loc: glow::UniformLocation,
    state_time_loc: glow::UniformLocation,
    pos: u32,
    uv: u32,
}

impl PostprocScene {
    const POST_VSHAD: &str = "attribute vec2 a_pos; attribute vec2 a_uv; varying vec2 v_uv; \
             void main() { gl_Position = vec4(a_pos, 0.0, 1.0); v_uv = a_uv; }";
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
    fn new(gl: &glow::Context) -> Result<Self, Box<dyn std::error::Error>> {
        let verts: [f32; 16] = [
            -1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0, -1.0, -1.0, 0.0, 0.0, 1.0, -1.0, 1.0, 0.0,
        ];

        let pp_scene = unsafe {
            let verts_bytes = std::slice::from_raw_parts(
                verts.as_ptr() as *const u8,
                verts.len() * std::mem::size_of::<f32>(),
            );
            let vbo = gl.create_buffer()?;
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, verts_bytes, glow::STATIC_DRAW);
            // restore the fbo to default and bind texture to the scene_tex we rendered into

            // Minimal GLSL ES shader: fullscreen textured quad.
            let vs = compile_shader(&gl, glow::VERTEX_SHADER, PostprocScene::POST_VSHAD)?;
            let fs = compile_shader(&gl, glow::FRAGMENT_SHADER, PostprocScene::POST_FSHAD)?;
            let prog = link_program(&gl, vs, fs)?;
            gl.use_program(Some(prog));
            gl.delete_shader(vs);
            gl.delete_shader(fs);

            let pos = gl.get_attrib_location(prog, "a_pos").expect("a_pos");
            let uv = gl.get_attrib_location(prog, "a_uv").expect("a_uv");

            let tex_loc = gl.get_uniform_location(prog, "u_tex").expect("u_tex");

            let time_loc = gl.get_uniform_location(prog, "u_time").expect("u_time");
            let aspect_loc = gl.get_uniform_location(prog, "u_aspect").expect("u_aspect");
            let resolution_loc = gl
                .get_uniform_location(prog, "u_resolution")
                .expect("u_resolution");
            let login_state_loc = gl
                .get_uniform_location(prog, "u_login_state")
                .expect("u_login_state");
            let state_time_loc = gl
                .get_uniform_location(prog, "u_state_time")
                .expect("u_state_time");
            Self {
                pos,
                uv,
                prog,
                vbo,
                tex_loc,
                time_loc,
                aspect_loc,
                resolution_loc,
                login_state_loc,
                state_time_loc,
            }
        };
        Ok(pp_scene)
    }

    fn draw(
        &self,
        gl: &glow::Context,
        texture: glow::NativeTexture,
        w: f32,
        h: f32,
        t: f32,
        login_state: i32,
        state_time: f32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        unsafe {
            gl.viewport(0, 0, w as i32, h as i32);
            gl.use_program(Some(self.prog));
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));

            gl.enable_vertex_attrib_array(self.pos);
            gl.vertex_attrib_pointer_f32(self.pos, 2, glow::FLOAT, false, 16, 0);
            gl.enable_vertex_attrib_array(self.uv);
            gl.vertex_attrib_pointer_f32(self.uv, 2, glow::FLOAT, false, 16, 8);

            gl.uniform_1_i32(Some(&self.tex_loc), 0);
            gl.uniform_1_f32(Some(&self.aspect_loc), w / h);
            gl.uniform_2_f32(Some(&self.resolution_loc), w, h);

            gl.uniform_1_f32(Some(&self.time_loc), t);
            gl.uniform_1_i32(Some(&self.login_state_loc), login_state);
            gl.uniform_1_f32(Some(&self.state_time_loc), state_time);
            gl.clear(COLOR_BUFFER_BIT);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        }
        Ok(())
    }
}
