use crate::{BackgroundConfig, BackgroundEffect, Config, InputConfig, PostConfig, login::LoginView};
use font8x8::legacy::BASIC_LEGACY;
use glow::{COLOR_BUFFER_BIT, HasContext};
use std::collections::HashMap;
use std::fs;
use std::io;

type GlBuffer = <glow::Context as HasContext>::Buffer;
type GlFramebuffer = <glow::Context as HasContext>::Framebuffer;
type GlProgram = <glow::Context as HasContext>::Program;
type GlShader = <glow::Context as HasContext>::Shader;
type GlTexture = <glow::Context as HasContext>::Texture;

fn load_shader(path: &str) -> io::Result<String> {
    fs::read_to_string(path)
}

fn background_effect_shader_src(effect: &BackgroundEffect) -> Result<String, io::Error> {
    if let Some(shader) = &effect.shader {
        return Ok(shader.clone());
    }

    if let Some(path) = &effect.path {
        return load_shader(path);
    }

    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "background.effect requires path or shader",
    ))
}

pub struct Scene {
    background: BackgroundScene,
    background_fx: Option<BackgroundFxScene>,
    text: TextScene,
    post: PostprocScene,
}

impl Scene {
    pub fn new(
        gl: &glow::Context,
        cfg: &Config,
        w: u32,
        h: u32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            background: BackgroundScene::new(gl, &cfg.background, w, h)?,
            background_fx: cfg
                .background
                .effect
                .as_ref()
                .map(|effect| BackgroundFxScene::new(gl, effect, w, h))
                .transpose()?,
            text: TextScene::new(gl, &cfg.input, w, h)?,
            post: PostprocScene::new(gl, &cfg.post, w, h)?,
        })
    }

    pub fn draw(
        &self,
        gl: &glow::Context,
        view: &LoginView,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.background.draw(gl, view);

        let (texture, fbo) = if let Some(background_fx) = &self.background_fx {
            background_fx.draw(gl, self.background.output_texture(), view)?;
            (background_fx.output_texture(), background_fx.output_fbo())
        } else {
            (
                self.background.output_texture(),
                self.background.output_fbo(),
            )
        };

        self.text.draw(gl, fbo, view)?;
        self.post.draw(gl, texture, view)?;

        Ok(())
    }
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
) -> Result<GlShader, Box<dyn std::error::Error>> {
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
    vs: GlShader,
    fs: GlShader,
) -> Result<GlProgram, Box<dyn std::error::Error>> {
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

unsafe fn check_framebuffer(gl: &glow::Context) -> Result<(), Box<dyn std::error::Error>> {
    let status = unsafe { gl.check_framebuffer_status(glow::FRAMEBUFFER) };
    if status != glow::FRAMEBUFFER_COMPLETE {
        return Err(format!("framebuffer incomplete: status 0x{status:x}").into());
    }
    Ok(())
}

unsafe fn required_attrib(
    gl: &glow::Context,
    program: GlProgram,
    name: &str,
) -> Result<u32, Box<dyn std::error::Error>> {
    unsafe { gl.get_attrib_location(program, name) }
        .ok_or_else(|| format!("missing shader attribute {name}").into())
}

unsafe fn required_uniform(
    gl: &glow::Context,
    program: GlProgram,
    name: &str,
) -> Result<glow::UniformLocation, Box<dyn std::error::Error>> {
    unsafe { gl.get_uniform_location(program, name) }
        .ok_or_else(|| format!("missing shader uniform {name}").into())
}

pub struct BackgroundScene {
    vbo: GlBuffer,
    scene_tex: GlTexture,
    scene_fbo: GlFramebuffer,
    w: u32,
    h: u32,
    scene_program: GlProgram,
    scene_uv: Option<u32>,
    scene_pos: u32,
    time_loc: Option<glow::UniformLocation>,
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

    pub fn output_texture(&self) -> GlTexture {
        self.scene_tex
    }
    pub fn output_fbo(&self) -> GlFramebuffer {
        self.scene_fbo
    }

    fn shader_src(cfg: &BackgroundConfig) -> String {
        if let Some(shader) = &cfg.shader {
            return shader.clone();
        }

        cfg.path
            .as_deref()
            .map(load_shader)
            .unwrap_or_else(|| Ok(Self::BG_FSHAD.into()))
            .unwrap_or_else(|_| Self::BG_FSHAD.into())
    }

    pub fn new(gl: &glow::Context, cfg: &BackgroundConfig, w: u32, h: u32) -> Result<Self, Box<dyn std::error::Error>> {
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
            let empty_pixels = vec![0u8; (w as usize) * (h as usize) * 4];
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA as i32,
                w as i32,
                h as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                Some(&empty_pixels),
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
            check_framebuffer(gl)?;

            let scene_vshad = compile_shader(&gl, glow::VERTEX_SHADER, BackgroundScene::BG_VSHAD)?;
            let scene_fshad = compile_shader(&gl, glow::FRAGMENT_SHADER, &Self::shader_src(cfg))?;
            let scene_program = link_program(&gl, scene_vshad, scene_fshad)?;
            gl.delete_shader(scene_vshad);
            gl.delete_shader(scene_fshad);

            let scene_pos = required_attrib(gl, scene_program, "a_pos")?;
            let scene_uv = gl.get_attrib_location(scene_program, "a_uv");
            gl.enable_vertex_attrib_array(scene_pos);
            gl.vertex_attrib_pointer_f32(scene_pos, 2, glow::FLOAT, false, 16, 0);
            if let Some(uv) = scene_uv {
                gl.enable_vertex_attrib_array(uv);
                gl.vertex_attrib_pointer_f32(uv, 2, glow::FLOAT, false, 16, 8);
            }
            let time_loc = gl.get_uniform_location(scene_program, "u_time");
            Ok(Self {
                scene_fbo,
                scene_program,
                scene_tex,
                vbo,
                h,
                w,
                scene_pos,
                scene_uv,
                time_loc,
            })
        };
        bg
    }

    pub fn draw(&self, gl: &glow::Context, view: &LoginView) {
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.scene_fbo));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            gl.enable_vertex_attrib_array(self.scene_pos);
            gl.vertex_attrib_pointer_f32(self.scene_pos, 2, glow::FLOAT, false, 16, 0);
            if let Some(uv) = self.scene_uv {
                gl.enable_vertex_attrib_array(uv);
                gl.vertex_attrib_pointer_f32(uv, 2, glow::FLOAT, false, 16, 8);
            }
            gl.viewport(0, 0, self.w as i32, self.h as i32);
            gl.use_program(Some(self.scene_program));
            if let Some(loc) = &self.time_loc {
                gl.uniform_1_f32(Some(loc), view.time);
            }
            gl.clear(COLOR_BUFFER_BIT);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        }
    }
}

pub struct TextScene {
    text_program: GlProgram,
    text_vbo: GlBuffer,
    atlas_text: GlTexture,
    atlas: FontAtlas,
    scale: usize,
    w: u32,
    h: u32,
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

    pub fn new(
        gl: &glow::Context,
        cfg: &InputConfig,
        w: u32,
        h: u32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        unsafe {
            // create text rendering prog
            let text_vshad = compile_shader(&gl, glow::VERTEX_SHADER, TextScene::TEXT_VSHAD)?;
            let text_fshad = compile_shader(&gl, glow::FRAGMENT_SHADER, TextScene::TEXT_FSHAD)?;
            let text_program = link_program(&gl, text_vshad, text_fshad)?;
            gl.delete_shader(text_vshad);
            gl.delete_shader(text_fshad);
            let scale = cfg.font_size / 8;
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
            let text_res_loc = required_uniform(gl, text_program, "u_resolution")?;
            let text_font_loc = required_uniform(gl, text_program, "u_font")?;
            let text_color_loc = required_uniform(gl, text_program, "u_color")?;
            let text_translate_loc = required_uniform(gl, text_program, "u_translate")?;
            let text_time_loc = required_uniform(gl, text_program, "u_time")?;

            let text_vbo = gl.create_buffer()?;

            Ok(Self {
                atlas_text,
                text_vbo,
                scale,
                w,
                h,
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

    pub fn draw(
        &self,
        gl: &glow::Context,
        scene_fbo: GlFramebuffer,
        view: &LoginView,
    ) -> Result<(), Box<dyn std::error::Error>> {
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(scene_fbo));
            //  // construct the vertices from the
            let display_text: Vec<char> =
                //"Welcome back cuz\nEnter your password _".chars().collect();
                view.msg.chars().collect();
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

                let Some(glyph) = self.atlas.get_glyph(*c) else {
                    continue;
                };
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
            let text_pos = required_attrib(gl, self.text_program, "a_pos")?;
            let text_uv = required_attrib(gl, self.text_program, "a_uv")?;
            let blink_loc = required_attrib(gl, self.text_program, "a_blink")?;
            gl.enable_vertex_attrib_array(text_pos);
            gl.vertex_attrib_pointer_f32(text_pos, 2, glow::FLOAT, false, 20, 0);
            gl.enable_vertex_attrib_array(text_uv);
            gl.vertex_attrib_pointer_f32(text_uv, 2, glow::FLOAT, false, 20, 8);
            gl.enable_vertex_attrib_array(blink_loc);
            gl.vertex_attrib_pointer_f32(blink_loc, 1, glow::FLOAT, false, 20, 16);

            gl.uniform_2_f32(
                Some(&self.text_translate_loc),
                (self.w.saturating_sub(width) / 2) as f32,
                height as f32 / 2.0,
            );

            gl.uniform_2_f32(Some(&self.text_res_loc), self.w as f32, self.h as f32);

            gl.viewport(0, 0, self.w as i32, self.h as i32);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.atlas_text));
            gl.uniform_1_i32(Some(&self.text_font_loc), 0);
            gl.uniform_1_f32(Some(&self.text_time_loc), view.time);
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

pub struct BackgroundFxScene {
    prog: GlProgram,
    vbo: GlBuffer,
    output_tex: GlTexture,
    output_fbo: GlFramebuffer,
    w: f32,
    h: f32,
    tex_loc: Option<glow::UniformLocation>,
    time_loc: Option<glow::UniformLocation>,
    aspect_loc: Option<glow::UniformLocation>,
    resolution_loc: Option<glow::UniformLocation>,
    login_state_loc: Option<glow::UniformLocation>,
    state_time_loc: Option<glow::UniformLocation>,
    pos: u32,
    uv: u32,
}

impl BackgroundFxScene {
    const FX_VSHAD: &str = "attribute vec2 a_pos; attribute vec2 a_uv; varying vec2 v_uv; \
             void main() { gl_Position = vec4(a_pos, 0.0, 1.0); v_uv = a_uv; }";

    pub fn new(
        gl: &glow::Context,
        effect: &BackgroundEffect,
        w: u32,
        h: u32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let verts: [f32; 16] = [
            -1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0, -1.0, -1.0, 0.0, 0.0, 1.0, -1.0, 1.0, 0.0,
        ];

        let fx = unsafe {
            let verts_bytes = std::slice::from_raw_parts(
                verts.as_ptr() as *const u8,
                verts.len() * std::mem::size_of::<f32>(),
            );
            let vbo = gl.create_buffer()?;
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, verts_bytes, glow::STATIC_DRAW);

            let output_tex = gl.create_texture()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(output_tex));
            let empty_pixels = vec![0u8; (w as usize) * (h as usize) * 4];
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA as i32,
                w as i32,
                h as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                Some(&empty_pixels),
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

            let output_fbo = gl.create_framebuffer()?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(output_fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(output_tex),
                0,
            );
            check_framebuffer(gl)?;

            let vs = compile_shader(gl, glow::VERTEX_SHADER, BackgroundFxScene::FX_VSHAD)?;
            let shader_src = background_effect_shader_src(effect)?;
            let fs = compile_shader(gl, glow::FRAGMENT_SHADER, &shader_src)?;
            let prog = link_program(gl, vs, fs)?;
            gl.use_program(Some(prog));
            gl.delete_shader(vs);
            gl.delete_shader(fs);

            let pos = required_attrib(gl, prog, "a_pos")?;
            let uv = required_attrib(gl, prog, "a_uv")?;

            Self {
                prog,
                vbo,
                output_tex,
                output_fbo,
                w: w as f32,
                h: h as f32,
                tex_loc: gl.get_uniform_location(prog, "u_tex"),
                time_loc: gl.get_uniform_location(prog, "u_time"),
                aspect_loc: gl.get_uniform_location(prog, "u_aspect"),
                resolution_loc: gl.get_uniform_location(prog, "u_resolution"),
                login_state_loc: gl.get_uniform_location(prog, "u_login_state"),
                state_time_loc: gl.get_uniform_location(prog, "u_state_time"),
                pos,
                uv,
            }
        };

        Ok(fx)
    }

    pub fn output_texture(&self) -> GlTexture {
        self.output_tex
    }

    pub fn output_fbo(&self) -> GlFramebuffer {
        self.output_fbo
    }

    pub fn draw(
        &self,
        gl: &glow::Context,
        texture: GlTexture,
        view: &LoginView,
    ) -> Result<(), Box<dyn std::error::Error>> {
        unsafe {
            gl.viewport(0, 0, self.w as i32, self.h as i32);
            gl.use_program(Some(self.prog));
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.output_fbo));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));

            gl.enable_vertex_attrib_array(self.pos);
            gl.vertex_attrib_pointer_f32(self.pos, 2, glow::FLOAT, false, 16, 0);
            gl.enable_vertex_attrib_array(self.uv);
            gl.vertex_attrib_pointer_f32(self.uv, 2, glow::FLOAT, false, 16, 8);

            if let Some(tex_loc) = &self.tex_loc {
                gl.uniform_1_i32(Some(tex_loc), 0);
            }
            if let Some(aspect_loc) = &self.aspect_loc {
                gl.uniform_1_f32(Some(aspect_loc), self.w / self.h);
            }
            if let Some(resolution_loc) = &self.resolution_loc {
                gl.uniform_2_f32(Some(resolution_loc), self.w, self.h);
            }
            if let Some(time_loc) = &self.time_loc {
                gl.uniform_1_f32(Some(time_loc), view.time);
            }
            if let Some(login_state_loc) = &self.login_state_loc {
                gl.uniform_1_i32(Some(login_state_loc), view.state);
            }
            if let Some(state_time_loc) = &self.state_time_loc {
                gl.uniform_1_f32(Some(state_time_loc), view.state_time);
            }

            gl.clear(COLOR_BUFFER_BIT);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        }
        Ok(())
    }
}

pub struct PostprocScene {
    prog: GlProgram,
    vbo: GlBuffer,
    w: f32,
    h: f32,
    tex_loc: Option<glow::UniformLocation>,
    time_loc: Option<glow::UniformLocation>,
    aspect_loc: Option<glow::UniformLocation>,
    resolution_loc: Option<glow::UniformLocation>,
    login_state_loc: Option<glow::UniformLocation>,
    state_time_loc: Option<glow::UniformLocation>,
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

    fn shader_src(cfg: &PostConfig) -> String {
        if let Some(shader) = &cfg.shader {
            return shader.clone();
        }

        cfg.path
            .as_deref()
            .map(load_shader)
            .unwrap_or_else(|| Ok(Self::POST_FSHAD.into()))
            .unwrap_or_else(|_| Self::POST_FSHAD.into())
    }

    pub fn new(
        gl: &glow::Context,
        cfg: &PostConfig,
        w: u32,
        h: u32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
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
            let shader_src = Self::shader_src(cfg);
            let fs = compile_shader(&gl, glow::FRAGMENT_SHADER, &shader_src)?;
            let prog = link_program(&gl, vs, fs)?;
            gl.use_program(Some(prog));
            gl.delete_shader(vs);
            gl.delete_shader(fs);

            let pos = required_attrib(gl, prog, "a_pos")?;
            let uv = required_attrib(gl, prog, "a_uv")?;

            let tex_loc = gl.get_uniform_location(prog, "u_tex");
            let time_loc = gl.get_uniform_location(prog, "u_time");
            let aspect_loc = gl.get_uniform_location(prog, "u_aspect");
            let resolution_loc = gl.get_uniform_location(prog, "u_resolution");
            let login_state_loc = gl.get_uniform_location(prog, "u_login_state");
            let state_time_loc = gl.get_uniform_location(prog, "u_state_time");
            Self {
                pos,
                uv,
                prog,
                vbo,
                w: w as f32,
                h: h as f32,
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
    pub fn draw(
        &self,
        gl: &glow::Context,
        texture: GlTexture,
        view: &LoginView,
    ) -> Result<(), Box<dyn std::error::Error>> {
        unsafe {
            gl.viewport(0, 0, self.w as i32, self.h as i32);
            gl.use_program(Some(self.prog));
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));

            gl.enable_vertex_attrib_array(self.pos);
            gl.vertex_attrib_pointer_f32(self.pos, 2, glow::FLOAT, false, 16, 0);
            gl.enable_vertex_attrib_array(self.uv);
            gl.vertex_attrib_pointer_f32(self.uv, 2, glow::FLOAT, false, 16, 8);

            if let Some(tex_loc) = &self.tex_loc {
                gl.uniform_1_i32(Some(tex_loc), 0);
            }
            if let Some(aspect_loc) = &self.aspect_loc {
                gl.uniform_1_f32(Some(aspect_loc), self.w / self.h);
            }
            if let Some(resolution_loc) = &self.resolution_loc {
                gl.uniform_2_f32(Some(resolution_loc), self.w, self.h);
            }
            if let Some(time_loc) = &self.time_loc {
                gl.uniform_1_f32(Some(time_loc), view.time);
            }
            if let Some(login_state_loc) = &self.login_state_loc {
                gl.uniform_1_i32(Some(login_state_loc), view.state);
            }
            if let Some(state_time_loc) = &self.state_time_loc {
                gl.uniform_1_f32(Some(state_time_loc), view.state_time);
            }
            gl.clear(COLOR_BUFFER_BIT);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        }
        Ok(())
    }
}
