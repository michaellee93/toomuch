use drm::Device as DrmDevice;
use drm::buffer::{
    Buffer as DrmBuffer, DrmFourcc, DrmModifier, Handle as BufferHandle,
    PlanarBuffer as DrmPlanarBuffer,
};
use drm::control::{
    Device as ControlDevice, Event, FbCmd2Flags, Mode, PageFlipFlags, connector, crtc, framebuffer,
};
use font8x8::legacy::BASIC_LEGACY;
use gbm::{AsRaw, BufferObjectFlags, Device as GbmDevice, Format};
use glow::{COLOR_BUFFER_BIT, HasContext, NativeBuffer, NativeProgram, NativeTexture};
use khronos_egl as egl;
use shlog::greetd_client::{GreetdCommand, GreetdResult, spawn_greetd_worker};
use shlog::keyboard::{KeyboardInput, find_keyboards, set_evdev_nonblocking};
use shlog::login::{LoginAction, LoginEvent, LoginState};
use std::collections::HashMap;
use std::convert::TryFrom;
use std::fs::{self, OpenOptions};
use std::io;
use std::num::NonZeroU32;
use std::os::fd::{AsFd, BorrowedFd};
use std::ptr::NonNull;
use std::thread;
use std::time::{Duration, Instant};

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

struct LockedFrontBuffer {
    surface: *mut gbm_sys::gbm_surface,
    bo: NonNull<gbm_sys::gbm_bo>,
}

impl LockedFrontBuffer {
    unsafe fn lock<T>(surface: &gbm::Surface<T>) -> Result<Self, Box<dyn std::error::Error>> {
        let surface = surface.as_raw_mut() as *mut gbm_sys::gbm_surface;
        let bo = unsafe { gbm_sys::gbm_surface_lock_front_buffer(surface) };
        let bo = NonNull::new(bo).ok_or("gbm_surface_lock_front_buffer returned null")?;
        Ok(Self { surface, bo })
    }

    fn raw_bo(&self) -> *mut gbm_sys::gbm_bo {
        self.bo.as_ptr()
    }

    fn width(&self) -> u32 {
        unsafe { gbm_sys::gbm_bo_get_width(self.raw_bo()) }
    }

    fn height(&self) -> u32 {
        unsafe { gbm_sys::gbm_bo_get_height(self.raw_bo()) }
    }

    fn stride(&self) -> u32 {
        unsafe { gbm_sys::gbm_bo_get_stride(self.raw_bo()) }
    }

    fn plane_count(&self) -> u32 {
        unsafe { gbm_sys::gbm_bo_get_plane_count(self.raw_bo()) as u32 }
    }
}

impl Drop for LockedFrontBuffer {
    fn drop(&mut self) {
        unsafe {
            gbm_sys::gbm_surface_release_buffer(self.surface, self.raw_bo());
        }
    }
}

impl DrmBuffer for LockedFrontBuffer {
    fn size(&self) -> (u32, u32) {
        (self.width(), self.height())
    }

    fn format(&self) -> DrmFourcc {
        DrmFourcc::try_from(unsafe { gbm_sys::gbm_bo_get_format(self.raw_bo()) })
            .expect("libgbm returned invalid buffer format")
    }

    fn pitch(&self) -> u32 {
        self.stride()
    }

    fn handle(&self) -> BufferHandle {
        let handle = unsafe { gbm_sys::gbm_bo_get_handle(self.raw_bo()).u32_ };
        BufferHandle::from(NonZeroU32::new(handle).expect("libgbm returned zero BO handle"))
    }
}

impl DrmPlanarBuffer for LockedFrontBuffer {
    fn size(&self) -> (u32, u32) {
        (self.width(), self.height())
    }

    fn format(&self) -> DrmFourcc {
        DrmFourcc::try_from(unsafe { gbm_sys::gbm_bo_get_format(self.raw_bo()) })
            .expect("libgbm returned invalid buffer format")
    }

    fn modifier(&self) -> Option<DrmModifier> {
        Some(DrmModifier::from(unsafe {
            gbm_sys::gbm_bo_get_modifier(self.raw_bo())
        }))
    }

    fn pitches(&self) -> [u32; 4] {
        let num = self.plane_count();
        [
            unsafe { gbm_sys::gbm_bo_get_stride_for_plane(self.raw_bo(), 0) },
            if num > 1 {
                unsafe { gbm_sys::gbm_bo_get_stride_for_plane(self.raw_bo(), 1) }
            } else {
                0
            },
            if num > 2 {
                unsafe { gbm_sys::gbm_bo_get_stride_for_plane(self.raw_bo(), 2) }
            } else {
                0
            },
            if num > 3 {
                unsafe { gbm_sys::gbm_bo_get_stride_for_plane(self.raw_bo(), 3) }
            } else {
                0
            },
        ]
    }

    fn handles(&self) -> [Option<BufferHandle>; 4] {
        let num = self.plane_count();
        [
            Some(buffer_handle_for_plane(self.raw_bo(), 0)),
            if num > 1 {
                Some(buffer_handle_for_plane(self.raw_bo(), 1))
            } else {
                None
            },
            if num > 2 {
                Some(buffer_handle_for_plane(self.raw_bo(), 2))
            } else {
                None
            },
            if num > 3 {
                Some(buffer_handle_for_plane(self.raw_bo(), 3))
            } else {
                None
            },
        ]
    }

    fn offsets(&self) -> [u32; 4] {
        let num = self.plane_count();
        [
            unsafe { gbm_sys::gbm_bo_get_offset(self.raw_bo(), 0) },
            if num > 1 {
                unsafe { gbm_sys::gbm_bo_get_offset(self.raw_bo(), 1) }
            } else {
                0
            },
            if num > 2 {
                unsafe { gbm_sys::gbm_bo_get_offset(self.raw_bo(), 2) }
            } else {
                0
            },
            if num > 3 {
                unsafe { gbm_sys::gbm_bo_get_offset(self.raw_bo(), 3) }
            } else {
                0
            },
        ]
    }
}

fn buffer_handle_for_plane(bo: *mut gbm_sys::gbm_bo, plane: i32) -> BufferHandle {
    let handle = unsafe { gbm_sys::gbm_bo_get_handle_for_plane(bo, plane).u32_ };
    BufferHandle::from(NonZeroU32::new(handle).expect("libgbm returned zero BO handle"))
}

fn open_card(path: &str) -> Result<GbmDevice<Card>, Box<dyn std::error::Error>> {
    let card = Card(
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| format!("open {path}: {e}"))?,
    );
    Ok(GbmDevice::new(card)?)
}

fn sorted_drm_card_paths() -> io::Result<Vec<String>> {
    let mut paths = fs::read_dir("/dev/dri")?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("card"))
        })
        .filter_map(|path| path.to_str().map(str::to_owned))
        .collect::<Vec<_>>();

    paths.sort();
    Ok(paths)
}

fn open_display_card() -> Result<(String, GbmDevice<Card>), Box<dyn std::error::Error>> {
    if let Ok(path) = std::env::var("SHLOG_DRM_CARD") {
        let gbm = open_card(&path)?;
        return Ok((path, gbm));
    }

    let mut errors = Vec::new();

    for path in sorted_drm_card_paths()? {
        match open_card(&path) {
            Ok(gbm) if find_setup(&gbm).is_some() => return Ok((path, gbm)),
            Ok(_) => errors.push(format!("{path}: no connected display setup")),
            Err(e) => errors.push(format!("{path}: {e}")),
        }
    }

    Err(format!("no usable DRM card found: {}", errors.join("; ")).into())
}

fn fourcc_string(format: u32) -> String {
    let bytes = format.to_le_bytes();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn framebuffer_depth(format: Format) -> u32 {
    match format {
        Format::Argb8888 | Format::Abgr8888 | Format::Rgba8888 | Format::Bgra8888 => 32,
        _ => 24,
    }
}

fn primary_plane_formats(dev: &impl ControlDevice, crtc_h: crtc::Handle) -> Vec<u32> {
    let Ok(res) = dev.resource_handles() else {
        return Vec::new();
    };
    let Ok(planes) = dev.plane_handles() else {
        return Vec::new();
    };

    let mut formats = Vec::new();
    for plane_h in planes {
        let Ok(plane) = dev.get_plane(plane_h) else {
            continue;
        };
        let possible = res.filter_crtcs(plane.possible_crtcs());
        if !possible.contains(&crtc_h) {
            continue;
        }
        eprintln!(
            "plane {plane_h:?}: crtc={:?} fb={:?} possible_crtcs={possible:?} formats={:?}",
            plane.crtc(),
            plane.framebuffer(),
            plane
                .formats()
                .iter()
                .map(|&f| fourcc_string(f))
                .collect::<Vec<_>>(),
        );
        for &format in plane.formats() {
            if !formats.contains(&format) {
                formats.push(format);
            }
        }
    }

    formats
}

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
        eprintln!(
            "connector {conn_h:?}: {:?}-{} state={:?} current_encoder={:?} encoders={:?}",
            conn.interface(),
            conn.interface_id(),
            conn.state(),
            conn.current_encoder(),
            conn.encoders(),
        );
        if conn.state() != connector::State::Connected {
            continue;
        }
        let Some(&mode) = conn.modes().first() else {
            continue;
        };

        if let Some(enc_h) = conn.current_encoder()
            && let Ok(enc) = dev.get_encoder(enc_h)
        {
            let possible = res.filter_crtcs(enc.possible_crtcs());
            eprintln!(
                "  current encoder {enc_h:?}: current_crtc={:?} possible_crtcs={possible:?}",
                enc.crtc(),
            );
            if let Some(crtc_h) = enc.crtc()
                && possible.contains(&crtc_h)
            {
                eprintln!(
                    "  selected current path: connector={conn_h:?} encoder={enc_h:?} crtc={crtc_h:?} mode={}",
                    mode.name().to_string_lossy(),
                );
                return Some((conn_h, mode, crtc_h));
            }
        }

        for &enc_h in conn.encoders() {
            let Ok(enc) = dev.get_encoder(enc_h) else {
                continue;
            };
            let possible = res.filter_crtcs(enc.possible_crtcs());
            eprintln!(
                "  encoder {enc_h:?}: current_crtc={:?} possible_crtcs={possible:?}",
                enc.crtc(),
            );
            if let Some(&crtc_h) = possible.first() {
                eprintln!(
                    "  selected possible path: connector={conn_h:?} encoder={enc_h:?} crtc={crtc_h:?} mode={}",
                    mode.name().to_string_lossy(),
                );
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

fn choose_egl_config(
    egl_api: &egl::Instance<egl::Static>,
    disp: egl::Display,
    supported_scanout_formats: &[u32],
) -> Result<(egl::Config, Format), Box<dyn std::error::Error>> {
    let attribs = [
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
    ];

    let mut configs = Vec::with_capacity(64);
    egl_api
        .choose_config(disp, &attribs, &mut configs)
        .map_err(|e| format!("eglChooseConfig: {e}"))?;

    let mut fallback = None;
    for cfg in configs {
        let native_visual = egl_api
            .get_config_attrib(disp, cfg, egl::NATIVE_VISUAL_ID)
            .map_err(|e| format!("eglGetConfigAttrib(NATIVE_VISUAL_ID): {e}"))?;
        let native_visual = native_visual as u32;
        let Ok(format) = Format::try_from(native_visual) else {
            eprintln!(
                "egl config ignored: unsupported native visual 0x{native_visual:08x} ({})",
                fourcc_string(native_visual),
            );
            continue;
        };

        eprintln!(
            "egl config candidate: native visual {format:?} ({})",
            fourcc_string(native_visual),
        );

        if supported_scanout_formats.contains(&native_visual) {
            return Ok((cfg, format));
        }
        fallback.get_or_insert((cfg, format));
    }

    fallback.ok_or_else(|| "eglChooseConfig: no usable config".into())
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

fn restore_crtc(
    dev: &impl ControlDevice,
    crtc_h: crtc::Handle,
    conn_h: connector::Handle,
    original: &crtc::Info,
    current_bo: &mut Option<(framebuffer::Handle, LockedFrontBuffer)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let conns = if original.mode().is_some() {
        &[conn_h][..]
    } else {
        &[][..]
    };

    dev.set_crtc(
        crtc_h,
        original.framebuffer(),
        original.position(),
        conns,
        original.mode(),
    )
    .map_err(|e| format!("restore set_crtc: {e}"))?;

    if let Some((fb, _bo)) = current_bo.take() {
        dev.destroy_framebuffer(fb)
            .map_err(|e| format!("restore destroy_framebuffer: {e}"))?;
    }

    Ok(())
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

    //    let mut input = String::new();

    // --- DRM / GBM ---
    let (card_path, gbm) = open_display_card()?;
    match gbm.set_client_capability(drm::ClientCapability::UniversalPlanes, true) {
        Ok(()) => eprintln!("drm client cap: universal planes enabled"),
        Err(e) => eprintln!("drm client cap: universal planes unavailable: {e}"),
    }
    match gbm.acquire_master_lock() {
        Ok(()) => eprintln!("drm master: acquired explicitly on {card_path}"),
        Err(e) => eprintln!("drm master: explicit acquire failed on {card_path}: {e}"),
    }

    let (conn_h, mode, crtc_h) = find_setup(&gbm).expect("no connected display found");
    let original_crtc = gbm
        .get_crtc(crtc_h)
        .map_err(|e| format!("get_crtc before modeset: {e}"))?;
    let (w, h) = (mode.size().0 as u32, mode.size().1 as u32);
    eprintln!(
        "drm setup: connector={conn_h:?} crtc={crtc_h:?} mode={} {w}x{h} original_fb={:?} original_mode={:?}",
        mode.name().to_string_lossy(),
        original_crtc.framebuffer(),
        original_crtc
            .mode()
            .map(|m| m.name().to_string_lossy().into_owned()),
    );
    let scanout_formats = primary_plane_formats(&gbm, crtc_h);
    eprintln!(
        "scanout format candidates for {crtc_h:?}: {:?}",
        scanout_formats
            .iter()
            .map(|&f| fourcc_string(f))
            .collect::<Vec<_>>(),
    );

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

    let (cfg, gbm_format) = choose_egl_config(&egl_api, disp, &scanout_formats)?;
    eprintln!("egl config ok: native visual {gbm_format:?}");

    let gbm_surf = gbm
        .create_surface::<()>(
            w,
            h,
            gbm_format,
            BufferObjectFlags::SCANOUT | BufferObjectFlags::RENDERING,
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
    let text_scene = TextScene::new(&gl, 7)?;
    let post_scene = PostprocScene::new(&gl)?;

    // --- Display via DRM ---
    let start = Instant::now();
    let mut current_bo: Option<(framebuffer::Handle, LockedFrontBuffer)> = None;
    let mut logged_bo = false;
    let mut logged_legacy_fb_fallback = false;

    // app state
    let mut state = LoginState::default();
    let mut this_state = Instant::now();
    let target_frame_time = Duration::from_millis(16);

    eprintln!("displaying animated shader — Ctrl-C to exit");
    'main: loop {
        let frame_start = Instant::now();

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
                restore_crtc(&gbm, crtc_h, conn_h, &original_crtc, &mut current_bo)?;
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
                        restore_crtc(&gbm, crtc_h, conn_h, &original_crtc, &mut current_bo)?;
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
                restore_crtc(&gbm, crtc_h, conn_h, &original_crtc, &mut current_bo)?;
                break 'main Ok(());
            }
        }

        let msg = state.message(this_state.elapsed());

        let t = start.elapsed().as_secs_f32();
        let state_time = this_state.elapsed().as_secs_f32();
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

        egl_api
            .swap_buffers(disp, surf)
            .map_err(|e| format!("eglSwapBuffers: {e}"))?;

        let bo = unsafe { LockedFrontBuffer::lock(&gbm_surf) }
            .map_err(|e| format!("lock_front_buffer: {e}"))?;

        if !logged_bo {
            eprintln!(
                "bo: {}x{} format={:?} stride={} modifier={:?}",
                bo.width(),
                bo.height(),
                DrmPlanarBuffer::format(&bo),
                bo.stride(),
                bo.modifier()
            );
            logged_bo = true;
        }

        let fb = match gbm.add_planar_framebuffer(&bo, FbCmd2Flags::MODIFIERS) {
            Ok(fb) => fb,
            Err(planar_error) => {
                if !logged_legacy_fb_fallback {
                    eprintln!(
                        "add_planar_framebuffer failed, falling back to add_framebuffer: {planar_error}"
                    );
                    logged_legacy_fb_fallback = true;
                }
                gbm.add_framebuffer(&bo, framebuffer_depth(gbm_format), 32)
                    .map_err(|e| {
                        format!(
                            "add_planar_framebuffer: {planar_error}; add_framebuffer fallback: {e}"
                        )
                    })?
            }
        };

        if current_bo.is_none() {
            eprintln!(
                "set_crtc: connector={conn_h:?} crtc={crtc_h:?} fb={fb:?} mode={} {}x{}",
                mode.name().to_string_lossy(),
                mode.size().0,
                mode.size().1,
            );
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

        let frame_time = frame_start.elapsed();
        if frame_time < target_frame_time {
            thread::sleep(target_frame_time - frame_time);
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
