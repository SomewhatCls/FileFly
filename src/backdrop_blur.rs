//! Frosted-glass backdrop for Glow.
//!
//! Per frame while a drag is active:
//!   1. Runs the UI in a second `egui::Context` (reduced pixel density).
//!   2. Renders it with a second `egui_glow::Painter` into an offscreen texture.
//!   3. Blurs it with a separable Gaussian (two fullscreen passes).
//!   4. `paint_zone` draws that blurred texture inside a rect, with rounded corners,
//!      through an `egui_glow::CallbackFn`, so it sits in the normal egui paint order.
//!
//! All GL work in `render` happens directly inside `App::ui`, on eframe's GL context,
//! and restores the framebuffer binding and viewport afterwards.

use std::sync::Arc;

use eframe::{egui, egui_glow, glow};
use glow::HasContext as _;

/// The copied frame is drawn at this resolution scale.
const SCALE: f32 = 0.1;

// Fullscreen triangle generated from gl_VertexID, so no vertex buffer is needed.
const VERT: &str = r#"
out vec2 v_uv;
void main() {
    vec2 t = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    v_uv = t;
    gl_Position = vec4(t * 2.0 - 1.0, 0.0, 1.0);
}
"#;

// 9-tap gaussian using 5 bilinear samples.
const BLUR_FRAG: &str = r#"
uniform sampler2D u_tex;
uniform vec2 u_dir;
in vec2 v_uv;
out vec4 o_color;
void main() {
    vec2 o1 = u_dir * 1.3846153846;
    vec2 o2 = u_dir * 3.2307692308;
    vec4 c = texture(u_tex, v_uv) * 0.2270270270;
    c += texture(u_tex, v_uv + o1) * 0.3162162162;
    c += texture(u_tex, v_uv - o1) * 0.3162162162;
    c += texture(u_tex, v_uv + o2) * 0.0702702703;
    c += texture(u_tex, v_uv - o2) * 0.0702702703;
    o_color = c;
}
"#;

// Draws the blurred texture into the callback rect (viewport = rect).
// Output is premultiplied alpha.
const SHOW_FRAG: &str = r#"
uniform sampler2D u_tex;
uniform vec4 u_uv;      // egui-space uv rect (min.x, min.y, max.x, max.y), top-down
uniform vec2 u_size;    // rect size in pixels
uniform float u_radius; // corner radius in pixels
in vec2 v_uv;
out vec4 o_color;
void main() {
    vec2 local = vec2(v_uv.x, 1.0 - v_uv.y);            // 0..1, top-down
    vec2 uv = mix(u_uv.xy, u_uv.zw, local);
    vec4 c = texture(u_tex, vec2(uv.x, 1.0 - uv.y));    // GL textures are bottom-up
    float r = min(u_radius, min(u_size.x, u_size.y) * 0.5);
    vec2 p = (local - 0.5) * u_size;
    vec2 b = u_size * 0.5 - vec2(r);
    float d = length(max(abs(p) - b, 0.0)) - r;
    float a = clamp(0.5 - d, 0.0, 1.0);
    o_color = vec4(c.rgb * a, a);
}
"#;

fn glsl_headers(gl: &glow::Context) -> (String, String) {
    let v = gl.version();
    if v.is_embedded {
        (
            "#version 300 es\n".to_owned(),
            "#version 300 es\nprecision highp float;\n".to_owned(),
        )
    } else if (v.major, v.minor) >= (3, 3) {
        ("#version 330 core\n".to_owned(), "#version 330 core\n".to_owned())
    } else {
        ("#version 130\n".to_owned(), "#version 130\n".to_owned())
    }
}

fn compile_program(gl: &glow::Context, vs: &str, fs: &str) -> Result<glow::Program, String> {
    unsafe {
        let program = gl.create_program()?;
        let mut shaders = Vec::new();
        for (kind, src) in [(glow::VERTEX_SHADER, vs), (glow::FRAGMENT_SHADER, fs)] {
            let sh = gl.create_shader(kind)?;
            gl.shader_source(sh, src);
            gl.compile_shader(sh);
            if !gl.get_shader_compile_status(sh) {
                return Err(gl.get_shader_info_log(sh));
            }
            gl.attach_shader(program, sh);
            shaders.push(sh);
        }
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            return Err(gl.get_program_info_log(program));
        }
        for sh in shaders {
            gl.detach_shader(program, sh);
            gl.delete_shader(sh);
        }
        Ok(program)
    }
}

#[derive(Clone, Copy)]
struct Target {
    tex: glow::Texture,
    fbo: glow::Framebuffer,
}

struct Targets {
    size: [i32; 2],
    scene: Target, // egui renders the background here
    ping: Target,  // horizontal blur result
    out: Target,   // final blurred result
}

fn make_target(gl: &glow::Context, w: i32, h: i32) -> Result<Target, String> {
    unsafe {
        let tex = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            w,
            h,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None), // older glow: `Slice(&[])`-style, see notes
        );
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
        // clamp-to-edge avoids dark fringes at the borders
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);

        let fbo = gl.create_framebuffer()?;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(tex),
            0,
        );
        Ok(Target { tex, fbo })
    }
}

fn delete_target(gl: &glow::Context, t: Target) {
    unsafe {
        gl.delete_framebuffer(t.fbo);
        gl.delete_texture(t.tex);
    }
}

struct BlurProgram {
    program: glow::Program,
    u_tex: Option<glow::UniformLocation>,
    u_dir: Option<glow::UniformLocation>,
}

/// Everything the paint callback needs. Cheap to share with the (Send + Sync) callback.
struct ShowProgram {
    program: glow::Program,
    vao: glow::VertexArray,
    u_tex: Option<glow::UniformLocation>,
    u_uv: Option<glow::UniformLocation>,
    u_size: Option<glow::UniformLocation>,
    u_radius: Option<glow::UniformLocation>,
}

pub struct BackdropBlur {
    gl: Arc<glow::Context>,
    ctx: egui::Context, // second context, background UI only
    painter: egui_glow::Painter,
    vao: glow::VertexArray,
    blur: BlurProgram,
    show: Arc<ShowProgram>,
    targets: Option<Targets>,
}

impl BackdropBlur {
    /// `setup_ctx` must apply the same fonts and image loaders you apply to the main context.
    /// Returns `None` (and prints why) if the GL setup fails; callers then keep their
    /// plain-fill fallback.
    pub fn new(gl: Arc<glow::Context>, setup_ctx: impl FnOnce(&egui::Context)) -> Option<Self> {
        match Self::try_new(gl, setup_ctx) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("backdrop blur disabled: {e}");
                None
            }
        }
    }

    fn try_new(gl: Arc<glow::Context>, setup_ctx: impl FnOnce(&egui::Context)) -> Result<Self, String> {
        let painter = egui_glow::Painter::new(gl.clone(), "", None, false).map_err(|e| e.to_string())?;

        let ctx = egui::Context::default();
        setup_ctx(&ctx);

        let (vh, fh) = glsl_headers(&gl);
        let vs = format!("{vh}{VERT}");
        let blur_program = compile_program(&gl, &vs, &format!("{fh}{BLUR_FRAG}"))?;
        let show_program = compile_program(&gl, &vs, &format!("{fh}{SHOW_FRAG}"))?;

        let (vao, blur, show) = unsafe {
            let vao = gl.create_vertex_array()?;
            let blur = BlurProgram {
                program: blur_program,
                u_tex: gl.get_uniform_location(blur_program, "u_tex"),
                u_dir: gl.get_uniform_location(blur_program, "u_dir"),
            };
            let show = ShowProgram {
                program: show_program,
                vao,
                u_tex: gl.get_uniform_location(show_program, "u_tex"),
                u_uv: gl.get_uniform_location(show_program, "u_uv"),
                u_size: gl.get_uniform_location(show_program, "u_size"),
                u_radius: gl.get_uniform_location(show_program, "u_radius"),
            };
            (vao, blur, show)
        };

        Ok(Self { gl, ctx, painter, vao, blur, show: Arc::new(show), targets: None })
    }

    /// The second context, so you can run your own style setup on it
    /// (e.g. `apply_style(bd.ctx(), &m, accent)`).
    pub fn ctx(&self) -> &egui::Context {
        &self.ctx
    }

    /// Frees the offscreen textures (call when no drag is active). The next `render`
    /// recreates them. Until then `paint_zone` returns false.
    pub fn release(&mut self) {
        if let Some(t) = self.targets.take() {
            delete_target(&self.gl, t.scene);
            delete_target(&self.gl, t.ping);
            delete_target(&self.gl, t.out);
        }
    }

    /// Runs the background UI in the second context, renders it at reduced resolution into
    /// an FBO and blurs it. Call from `App::ui`, before drawing the overlay.
    ///
    /// * `clear` is the color behind your UI (usually your panel fill).
    /// * `run_background_ui` gets the root `Ui` of the second context (no margin, no
    ///   background), so draw panels with `show_inside(ui, ..)` as in your main UI.
    pub fn render(
        &mut self,
        main_ctx: &egui::Context,
        clear: egui::Color32,
        run_background_ui: impl FnMut(&mut egui::Ui),
    ) {
        let screen = main_ctx.content_rect();
        let ppp = main_ctx.pixels_per_point();
        let scaled_ppp = ppp * SCALE;
        let size = [
            ((screen.width() * scaled_ppp).ceil() as i32).max(1),
            ((screen.height() * scaled_ppp).ceil() as i32).max(1),
        ];

        // 1. Run the background UI in the second context (no input, so no hover/clicks).
        //    The second context gets no system-theme input, so mirror the main one.
        self.ctx.set_theme(main_ctx.theme());
        let mut raw = egui::RawInput::default();
        raw.screen_rect = Some(screen);
        raw.time = Some(main_ctx.input(|i| i.time));
        raw.viewports.insert(
            raw.viewport_id,
            egui::ViewportInfo {
                native_pixels_per_point: Some(scaled_ppp),
                ..Default::default()
            },
        );
        let mut output = self.ctx.run_ui(raw, run_background_ui);
        let jobs = self.ctx.tessellate(output.shapes, output.pixels_per_point);

        // (Re)create targets on first use / resize.
        let recreate = self.targets.as_ref().map_or(true, |t| t.size != size);

        let gl = self.gl.clone();
        unsafe {
            // Save the state we touch so eframe's painter isn't disturbed.
            let mut vp = [0i32; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut vp);
            let prev_fbo = gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING);

            if recreate {
                self.release();
                let made = (|| -> Result<Targets, String> {
                    Ok(Targets {
                        size,
                        scene: make_target(&gl, size[0], size[1])?,
                        ping: make_target(&gl, size[0], size[1])?,
                        out: make_target(&gl, size[0], size[1])?,
                    })
                })();
                match made {
                    Ok(t) => self.targets = Some(t),
                    Err(e) => {
                        eprintln!("backdrop blur: could not create targets: {e}");
                        restore(&gl, prev_fbo, vp);
                        output.textures_delta.clear();
                        return;
                    }
                }
            }
            let t = self.targets.as_ref().unwrap();

            // 2. Render the background into the scene texture.
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(t.scene.fbo));
            gl.viewport(0, 0, size[0], size[1]);
            gl.disable(glow::SCISSOR_TEST);
            gl.color_mask(true, true, true, true);
            gl.clear_color(
                clear.r() as f32 / 255.0,
                clear.g() as f32 / 255.0,
                clear.b() as f32 / 255.0,
                1.0,
            );
            gl.clear(glow::COLOR_BUFFER_BIT);
            self.painter.paint_and_update_textures(
                [size[0] as u32, size[1] as u32],
                output.pixels_per_point,
                &jobs,
                &mut output.textures_delta,
            );

            // 3. Blur: scene -> ping (horizontal), ping -> out (vertical).
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
            gl.use_program(Some(self.blur.program));
            gl.bind_vertex_array(Some(self.vao));
            gl.active_texture(glow::TEXTURE0);
            gl.uniform_1_i32(self.blur.u_tex.as_ref(), 0);

            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(t.ping.fbo));
            gl.bind_texture(glow::TEXTURE_2D, Some(t.scene.tex));
            gl.uniform_2_f32(self.blur.u_dir.as_ref(), 1.0 / size[0] as f32, 0.0);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);

            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(t.out.fbo));
            gl.bind_texture(glow::TEXTURE_2D, Some(t.ping.tex));
            gl.uniform_2_f32(self.blur.u_dir.as_ref(), 0.0, 1.0 / size[1] as f32);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);

            restore(&gl, prev_fbo, vp);
        }

        // TexturesDelta panics on drop if deltas are left; the painter applied them above.
        output.textures_delta.clear();
    }

    /// Paints the blurred scene inside `rect` (egui points) with rounded corners.
    /// `screen` must be the same `content_rect()` the blur was rendered for.
    /// Returns false (and paints nothing) if no blurred texture exists yet, so the
    /// caller can draw its fallback fill instead.
    pub fn paint_zone(&self, painter: &egui::Painter, rect: egui::Rect, screen: egui::Rect, radius: f32) -> bool {
        let Some(t) = &self.targets else {
            return false;
        };
        let n = |p: egui::Pos2| {
            egui::pos2(
                (p.x - screen.min.x) / screen.width(),
                (p.y - screen.min.y) / screen.height(),
            )
        };
        let (uv_min, uv_max) = (n(rect.min), n(rect.max));
        let tex = t.out.tex;
        let show = self.show.clone();

        painter.add(egui::PaintCallback {
            rect,
            callback: Arc::new(egui_glow::CallbackFn::new(move |info, p| {
                let gl = p.gl();
                let vp = info.viewport_in_pixels();
                unsafe {
                    gl.use_program(Some(show.program));
                    gl.active_texture(glow::TEXTURE0);
                    gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                    gl.uniform_1_i32(show.u_tex.as_ref(), 0);
                    gl.uniform_4_f32(show.u_uv.as_ref(), uv_min.x, uv_min.y, uv_max.x, uv_max.y);
                    gl.uniform_2_f32(show.u_size.as_ref(), vp.width_px as f32, vp.height_px as f32);
                    gl.uniform_1_f32(show.u_radius.as_ref(), radius * info.pixels_per_point);
                    gl.bind_vertex_array(Some(show.vao));
                    gl.draw_arrays(glow::TRIANGLES, 0, 3);
                }
                // egui_glow re-prepares its own state after every callback.
            })),
        });
        true
    }
}

fn restore(gl: &glow::Context, prev_fbo: i32, vp: [i32; 4]) {
    unsafe {
        let fb = std::num::NonZeroU32::new(prev_fbo as u32).map(glow::NativeFramebuffer);
        gl.bind_framebuffer(glow::FRAMEBUFFER, fb);
        gl.viewport(vp[0], vp[1], vp[2], vp[3]);
    }
}
