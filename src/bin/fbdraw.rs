use std::collections::HashMap;
use std::convert::TryFrom;
use std::fs::OpenOptions;
use std::os::fd::{AsFd, BorrowedFd};
use std::time::Instant;

use drm::Device as DrmDevice;
use drm::control::{
    Device as ControlDevice, Event, Mode, PageFlipFlags, connector, crtc, framebuffer,
};
use font8x8::legacy::BASIC_LEGACY;
use gbm::{AsRaw, BufferObject, BufferObjectFlags, Device as GbmDevice, Format};
use glow::{COLOR_BUFFER_BIT, HasContext, NativeBuffer, NativeProgram, NativeTexture};
use khronos_egl as egl;

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
const POST_VSHAD: &str = "attribute vec2 a_pos; attribute vec2 a_uv; varying vec2 v_uv; \
             void main() { gl_Position = vec4(a_pos, 0.0, 1.0); v_uv = a_uv; }";
const POST_FSHAD: &str = "precision mediump float; \
             uniform sampler2D u_tex; \
             uniform float u_time; \
             uniform float u_aspect; \
             uniform vec2 u_resolution; \
             varying vec2 v_uv; \
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
	                 uv.x += sin(uv.y * 42.0 + u_time * 18.0) * 0.0025 * burst; \
	                 uv = clamp(uv, vec2(0.0), vec2(1.0)); \
                 vec2 px = 1.0 / u_resolution; \
                 float aberration = 0.0007 + 0.0012 * r2; \
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
	                 color = mix(vec3(0.03, 0.028, 0.04), color, screen_mask); \
	                 gl_FragColor = vec4(color, 1.0); \
	             }";

// ---------------------------------------------------------------------------
// DRM device wrapper
// ---------------------------------------------------------------------------

struct Card(std::fs::File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl DrmDevice for Card {}
impl ControlDevice for Card {}

// ---------------------------------------------------------------------------
// DRM setup: find the first connected connector, its preferred mode, and a
// compatible CRTC.
// ---------------------------------------------------------------------------

fn find_setup(dev: &impl ControlDevice) -> Option<(connector::Handle, Mode, crtc::Handle)> {
    let res = dev.resource_handles().ok()?;
    for &conn_h in res.connectors() {
        let Ok(conn) = dev.get_connector(conn_h, false) else {
            continue;
        };
        if conn.state() != connector::State::Connected {
            continue;
        }
        let Some(&mode) = conn.modes().first() else {
            continue;
        };
        for &enc_h in conn.encoders() {
            let Ok(enc) = dev.get_encoder(enc_h) else {
                continue;
            };
            if let Some(&crtc_h) = res.filter_crtcs(enc.possible_crtcs()).first() {
                return Some((conn_h, mode, crtc_h));
            }
        }
    }
    None
}

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
                v0: y as f32 / h as f32,
                v1: (y + cell) as f32 / h as f32,
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

fn framebuffer_depth(format: Format) -> u32 {
    match format {
        Format::Argb8888 | Format::Abgr8888 | Format::Rgba8888 | Format::Bgra8888 => 32,
        _ => 24,
    }
}

fn wait_for_page_flip(
    dev: &impl ControlDevice,
    crtc_h: crtc::Handle,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        for event in dev.receive_events()? {
            if let Event::PageFlip(event) = event
                && event.crtc == crtc_h
            {
                return Ok(());
            }
        }
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
    // --- DRM / GBM ---
    let card = Card(
        OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/dri/card0")?,
    );
    let gbm = GbmDevice::new(card)?;

    let (conn_h, mode, crtc_h) = find_setup(&gbm).expect("no connected display found");
    let (w, h) = (mode.size().0 as u32, mode.size().1 as u32);
    eprintln!("mode: {w}x{h}");

    // --- EGL ---
    let egl_api = egl::Instance::new(egl::Static);

    // eglGetDisplay(gbm_device*) is Mesa folklore that breaks on virgl.
    // The correct path is eglGetPlatformDisplayEXT(EGL_PLATFORM_GBM_KHR),
    // which tells Mesa the native handle is a gbm_device and that later
    // eglCreateWindowSurface will receive a gbm_surface*.
    const EGL_PLATFORM_GBM_KHR: u32 = 0x31D7;
    let disp = {
        type F =
            unsafe extern "C" fn(u32, *mut std::ffi::c_void, *const i32) -> *mut std::ffi::c_void;
        let f: F = unsafe {
            std::mem::transmute(
                egl_api
                    .get_proc_address("eglGetPlatformDisplayEXT")
                    .ok_or("eglGetPlatformDisplayEXT not available — Mesa too old?")?,
            )
        };
        let raw = unsafe {
            f(
                EGL_PLATFORM_GBM_KHR,
                gbm.as_raw_mut() as *mut _,
                std::ptr::null(),
            )
        };
        if raw.is_null() {
            return Err("eglGetPlatformDisplayEXT returned EGL_NO_DISPLAY".into());
        }
        // egl::Display is a newtype over *mut c_void with identical layout.
        unsafe { std::mem::transmute::<*mut std::ffi::c_void, egl::Display>(raw) }
    };
    eprintln!("egl display ok (GBM platform)");

    egl_api
        .initialize(disp)
        .map_err(|e| format!("eglInitialize: {e}"))?;
    eprintln!("egl initialised");

    egl_api
        .bind_api(egl::OPENGL_ES_API)
        .map_err(|e| format!("eglBindAPI: {e}"))?;

    let cfg = egl_api
        .choose_first_config(
            disp,
            &[
                egl::SURFACE_TYPE,
                egl::WINDOW_BIT,
                egl::RENDERABLE_TYPE,
                egl::OPENGL_ES2_BIT,
                egl::RED_SIZE,
                8,
                egl::GREEN_SIZE,
                8,
                egl::BLUE_SIZE,
                8,
                egl::NONE,
            ],
        )
        .map_err(|e| format!("eglChooseConfig: {e}"))?
        .ok_or("eglChooseConfig: no matching config")?;
    let native_visual = egl_api
        .get_config_attrib(disp, cfg, egl::NATIVE_VISUAL_ID)
        .map_err(|e| format!("eglGetConfigAttrib(NATIVE_VISUAL_ID): {e}"))?;
    let gbm_format = Format::try_from(native_visual as u32).map_err(|_| {
        format!("egl config returned unsupported native visual 0x{native_visual:08x}")
    })?;
    eprintln!("egl config ok: native visual {gbm_format:?}");

    let gbm_surf = gbm
        .create_surface::<()>(
            w,
            h,
            gbm_format,
            BufferObjectFlags::SCANOUT | BufferObjectFlags::RENDERING | BufferObjectFlags::LINEAR,
        )
        .map_err(|e| format!("gbm create_surface: {e}"))?;
    eprintln!("gbm surface ok");

    let ctx = egl_api
        .create_context(
            disp,
            cfg,
            None,
            &[egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE],
        )
        .map_err(|e| format!("eglCreateContext: {e}"))?;
    eprintln!("egl context ok");

    let surf = unsafe {
        egl_api.create_window_surface(
            disp,
            cfg,
            gbm_surf.as_raw_mut() as egl::NativeWindowType,
            Some(&[egl::NONE]),
        )
    }
    .map_err(|e| format!("eglCreateWindowSurface: {e}"))?;
    eprintln!("egl surface ok");

    egl_api
        .make_current(disp, Some(surf), Some(surf), Some(ctx))
        .map_err(|e| format!("eglMakeCurrent: {e}"))?;
    eprintln!("egl make_current ok");
    // --- OpenGL ES ---
    let gl = unsafe {
        glow::Context::from_loader_function(|s| {
            egl_api
                .get_proc_address(s)
                .map(|p| p as *const _)
                .unwrap_or(std::ptr::null())
        })
    };

    let bg_scene = BackgroundScene::new(&gl, w, h)?;
    let text_scene = TextScene::new(&gl, 9)?;
    let post_scene = PostprocScene::new(&gl)?;

    // --- Display via DRM ---
    let start = Instant::now();
    let mut current_bo: Option<(framebuffer::Handle, BufferObject<()>)> = None;
    let mut logged_bo = false;

    eprintln!("displaying animated shader — Ctrl-C to exit");
    loop {
        let t = start.elapsed().as_secs_f32();
        bg_scene.draw(&gl);
        text_scene.draw(&gl, bg_scene.scene_fbo, "Hello _", w, h, t)?;
        post_scene.draw(&gl, bg_scene.scene_tex, w as f32, h as f32, t)?;

        egl_api
            .swap_buffers(disp, surf)
            .map_err(|e| format!("eglSwapBuffers: {e}"))?;

        let bo = unsafe { gbm_surf.lock_front_buffer() }
            .map_err(|e| format!("lock_front_buffer: {e}"))?;

        if !logged_bo {
            eprintln!(
                "bo: {:?}x{:?} format={:?} stride={:?} modifier={:?}",
                bo.width(),
                bo.height(),
                bo.format(),
                bo.stride(),
                bo.modifier()
            );
            logged_bo = true;
        }

        let fb = gbm
            .add_framebuffer(&bo, framebuffer_depth(gbm_format), 32)
            .map_err(|e| format!("add_framebuffer: {e}"))?;

        if current_bo.is_none() {
            gbm.set_crtc(crtc_h, Some(fb), (0, 0), &[conn_h], Some(mode))
                .map_err(|e| format!("set_crtc: {e}"))?;
            current_bo = Some((fb, bo));
            continue;
        }

        gbm.page_flip(crtc_h, fb, PageFlipFlags::EVENT, None)
            .map_err(|e| format!("page_flip: {e}"))?;
        wait_for_page_flip(&gbm, crtc_h)?;

        if let Some((old_fb, _old_bo)) = current_bo.replace((fb, bo)) {
            gbm.destroy_framebuffer(old_fb)
                .map_err(|e| format!("destroy_framebuffer: {e}"))?;
        }
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

            let scene_vshad = compile_shader(&gl, glow::VERTEX_SHADER, BG_VSHAD)?;
            let scene_fshad = compile_shader(&gl, glow::FRAGMENT_SHADER, BG_FSHAD)?;
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
    fn new(gl: &glow::Context, scale: usize) -> Result<Self, Box<dyn std::error::Error>> {
        unsafe {
            // create text rendering prog
            let text_vshad = compile_shader(&gl, glow::VERTEX_SHADER, TEXT_VSHAD)?;
            let text_fshad = compile_shader(&gl, glow::FRAGMENT_SHADER, TEXT_FSHAD)?;
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
                ((w - width) / 2) as f32,
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
    pos: u32,
    uv: u32,
}

impl PostprocScene {
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
            let vs = compile_shader(&gl, glow::VERTEX_SHADER, POST_VSHAD)?;
            let fs = compile_shader(&gl, glow::FRAGMENT_SHADER, POST_FSHAD)?;
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
            Self {
                pos,
                uv,
                prog,
                vbo,
                tex_loc,
                time_loc,
                aspect_loc,
                resolution_loc,
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
            gl.clear(COLOR_BUFFER_BIT);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        }
        Ok(())
    }
}
