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
use glow::{COLOR_BUFFER_BIT, HasContext};
use khronos_egl as egl;

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

fn create_atlas() {}

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

    let (prog, time_loc) = unsafe {
        // Upload texture (full-framebuffer RGBA image with text pre-rendered).
        const BG: [u8; 4] = [0x1e, 0x1e, 0x2e, 0xff]; // catppuccin base
        const FG: [u8; 4] = [0xcd, 0xd6, 0xf4, 0xff]; // catppuccin text
        let pixels = text_to_rgba("Hello, world!", 4, w as usize, h as usize, BG, FG);
        let tex = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA as i32,
            w as i32,
            h as i32,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            Some(&pixels),
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

        // Minimal GLSL ES shader: fullscreen textured quad.
        let vs = gl.create_shader(glow::VERTEX_SHADER)?;
        gl.shader_source(
            vs,
            "attribute vec2 a_pos; attribute vec2 a_uv; varying vec2 v_uv; \
             void main() { gl_Position = vec4(a_pos, 0.0, 1.0); v_uv = a_uv; }",
        );
        gl.compile_shader(vs);

        let fs = gl.create_shader(glow::FRAGMENT_SHADER)?;
        gl.shader_source(
            fs,
             "precision mediump float; \
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
                 if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) { \
                     gl_FragColor = vec4(0.03, 0.028, 0.04, 1.0); \
                     return; \
                 } \
                 float burst = pow(max(0.0, sin(u_time * 0.7 + sin(u_time * 0.19) * 2.4)), 18.0); \
                 uv.x += sin(uv.y * 42.0 + u_time * 18.0) * 0.0025 * burst; \
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
                 gl_FragColor = vec4(color, 1.0); \
             }",
        );
        gl.compile_shader(fs);

        let prog = gl.create_program()?;
        gl.attach_shader(prog, vs);
        gl.attach_shader(prog, fs);
        gl.link_program(prog);
        gl.use_program(Some(prog));
        gl.delete_shader(vs);
        gl.delete_shader(fs);

        // Fullscreen quad. Keep V in the same top-to-bottom order as the
        // generated pixel buffer; this matches the GBM/KMS scanout path here.
        #[rustfmt::skip]
        let verts: [f32; 16] = [
            -1.0,  1.0,  0.0, 0.0,
             1.0,  1.0,  1.0, 0.0,
            -1.0, -1.0,  0.0, 1.0,
             1.0, -1.0,  1.0, 1.0,
        ];
        let verts_bytes = std::slice::from_raw_parts(
            verts.as_ptr() as *const u8,
            verts.len() * std::mem::size_of::<f32>(),
        );
        let vbo = gl.create_buffer()?;
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, verts_bytes, glow::STATIC_DRAW);

        let pos = gl.get_attrib_location(prog, "a_pos").expect("a_pos");
        let uv = gl.get_attrib_location(prog, "a_uv").expect("a_uv");
        gl.enable_vertex_attrib_array(pos);
        gl.vertex_attrib_pointer_f32(pos, 2, glow::FLOAT, false, 16, 0);
        gl.enable_vertex_attrib_array(uv);
        gl.vertex_attrib_pointer_f32(uv, 2, glow::FLOAT, false, 16, 8);

        let tex_loc = gl.get_uniform_location(prog, "u_tex").expect("u_tex");
        gl.uniform_1_i32(Some(&tex_loc), 0);

        let time_loc = gl.get_uniform_location(prog, "u_time").expect("u_time");
        let aspect_loc = gl.get_uniform_location(prog, "u_aspect").expect("u_aspect");
        let resolution_loc = gl
            .get_uniform_location(prog, "u_resolution")
            .expect("u_resolution");
        gl.uniform_1_f32(Some(&aspect_loc), w as f32 / h as f32);
        gl.uniform_2_f32(Some(&resolution_loc), w as f32, h as f32);
        (prog, time_loc)
    };

    // --- Display via DRM ---
    let start = Instant::now();
    let mut current_bo: Option<(framebuffer::Handle, BufferObject<()>)> = None;
    let mut logged_bo = false;

    eprintln!("displaying animated shader — Ctrl-C to exit");
    loop {
        unsafe {
            let t = start.elapsed().as_secs_f32();
            gl.viewport(0, 0, w as i32, h as i32);
            gl.use_program(Some(prog));
            gl.uniform_1_f32(Some(&time_loc), t);
            gl.clear(COLOR_BUFFER_BIT);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        }

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
