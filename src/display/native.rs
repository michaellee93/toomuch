use drm::Device as DrmDevice;
use drm::buffer::{
    Buffer as DrmBuffer, DrmFourcc, DrmModifier, Handle as BufferHandle,
    PlanarBuffer as DrmPlanarBuffer,
};
use drm::control::crtc::Info;
use drm::control::{
    Device as ControlDevice, Event, FbCmd2Flags, Mode, PageFlipFlags, connector, crtc, framebuffer,
};
use gbm::{AsRaw, BufferObjectFlags, Device as GbmDevice, Format, Surface as GbmSurface};
use khronos_egl::{self as egl, Display, Surface};
use std::convert::TryFrom;
use std::fs::{self, OpenOptions};
use std::io;
use std::num::NonZeroU32;
use std::os::fd::{AsFd, BorrowedFd};
use std::ptr::NonNull;

pub struct DrmDisplay {
    egl_api: egl::Instance<egl::Static>,
    gl: glow::Context,
    disp: Display,
    surf: Surface,
    gbm_surf: GbmSurface<()>,

    card_path: String,
    gbm: GbmDevice<Card>,
    gbm_format: Format,
    mode: Mode,
    conn_h: connector::Handle,
    crtc_h: crtc::Handle,
    original_crtc: Info,

    w: u32,
    h: u32,

    current_bo: Option<(framebuffer::Handle, LockedFrontBuffer)>,
    logged_bo: bool,
    logged_legacy_fb_fallback: bool,
}

impl DrmDisplay {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
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
            type F = unsafe extern "C" fn(
                u32,
                *mut std::ffi::c_void,
                *const i32,
            ) -> *mut std::ffi::c_void;
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

        // bo vars
        let logged_bo = false;
        let logged_legacy_fb_fallback = false;

        Ok(Self {
            original_crtc,
            egl_api,
            gl,
            gbm_surf,
            surf,
            disp,
            card_path,
            gbm,
            crtc_h,
            conn_h,
            w,
            h,
            mode,
            gbm_format,
            current_bo: None,
            logged_bo,
            logged_legacy_fb_fallback,
        })
    }

    pub fn gl(&mut self) -> Result<&mut glow::Context, Box<dyn std::error::Error>> {
        Ok(&mut self.gl)
    }

    pub fn present(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.egl_api
            .swap_buffers(self.disp, self.surf)
            .map_err(|e| format!("eglSwapBuffers: {e}"))?;

        let bo = unsafe { LockedFrontBuffer::lock(&self.gbm_surf) }
            .map_err(|e| format!("lock_front_buffer: {e}"))?;

        if !self.logged_bo {
            eprintln!(
                "bo: {}x{} format={:?} stride={} modifier={:?}",
                bo.width(),
                bo.height(),
                DrmPlanarBuffer::format(&bo),
                bo.stride(),
                bo.modifier()
            );
            self.logged_bo = true;
        }

        let fb = match self.gbm.add_planar_framebuffer(&bo, FbCmd2Flags::MODIFIERS) {
            Ok(fb) => fb,
            Err(planar_error) => {
                if !self.logged_legacy_fb_fallback {
                    eprintln!(
                        "add_planar_framebuffer failed, falling back to add_framebuffer: {planar_error}"
                    );
                    self.logged_legacy_fb_fallback = true;
                }
                self.gbm
                    .add_framebuffer(&bo, framebuffer_depth(self.gbm_format), 32)
                    .map_err(|e| {
                        format!(
                            "add_planar_framebuffer: {planar_error}; add_framebuffer fallback: {e}"
                        )
                    })?
            }
        };

        if self.current_bo.is_none() {
            let conn_h = self.conn_h;
            let crtc_h = self.crtc_h;
            eprintln!(
                "set_crtc: connector={conn_h:?} crtc={crtc_h:?} fb={fb:?} mode={} {}x{}",
                self.mode.name().to_string_lossy(),
                self.mode.size().0,
                self.mode.size().1,
            );
            self.gbm
                .set_crtc(
                    self.crtc_h,
                    Some(fb),
                    (0, 0),
                    &[self.conn_h],
                    Some(self.mode),
                )
                .map_err(|e| format!("set_crtc: {e}"))?;
            self.current_bo = Some((fb, bo));
            return Ok(());
        }

        self.gbm
            .page_flip(self.crtc_h, fb, PageFlipFlags::EVENT, None)
            .map_err(|e| format!("page_flip: {e}"))?;
        wait_for_page_flip(&self.gbm, self.crtc_h)?;

        if let Some((old_fb, _old_bo)) = self.current_bo.replace((fb, bo)) {
            self.gbm
                .destroy_framebuffer(old_fb)
                .map_err(|e| format!("destroy_framebuffer: {e}"))?;
        }

        Ok(())
    }

    pub fn size(&self) -> (u32, u32) {
        (self.w, self.h)
    }

    fn restore(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        restore_crtc(
            &self.gbm,
            self.crtc_h,
            self.conn_h,
            &self.original_crtc,
            &mut self.current_bo,
        )?;
        Ok(())
    }
}

impl Drop for DrmDisplay {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

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
    if let Ok(path) = std::env::var("TOOMUCH_DRM_CARD") {
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
