//! Reads a tiled or compressed scanout buffer through the GPU: EGL imports its dma-buf, GLES
//! draws it into a plain texture and reads that back. libEGL and libGLESv2 are loaded at run
//! time, so the program starts on systems without them.

use std::ffi::{CStr, c_char, c_void};

type Ptr = *mut c_void;
type EglInt = i32;

const EGL_NONE: EglInt = 0x3038;
const EGL_WIDTH: EglInt = 0x3057;
const EGL_HEIGHT: EglInt = 0x3056;
const EGL_OPENGL_ES_API: u32 = 0x30A0;
const EGL_CONTEXT_CLIENT_VERSION: EglInt = 0x3098;
const EGL_PLATFORM_DEVICE_EXT: u32 = 0x313F;
const EGL_PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;
const EGL_DRM_DEVICE_FILE_EXT: EglInt = 0x3233;
const EGL_LINUX_DMA_BUF_EXT: u32 = 0x3270;
const EGL_LINUX_DRM_FOURCC_EXT: EglInt = 0x3271;
const EGL_EXTENSIONS: EglInt = 0x3055;
/// Per plane: fd, offset, pitch, modifier low and high bits.
const PLANE_ATTRS: [[EglInt; 5]; 4] = [
    [0x3272, 0x3273, 0x3274, 0x3443, 0x3444],
    [0x3275, 0x3276, 0x3277, 0x3445, 0x3446],
    [0x3278, 0x3279, 0x327A, 0x3447, 0x3448],
    [0x3440, 0x3441, 0x3442, 0x3449, 0x344A],
];

const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_TEXTURE_EXTERNAL_OES: u32 = 0x8D65;
const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
const GL_NEAREST: i32 = 0x2600;
const GL_RGBA: u32 = 0x1908;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_FRAMEBUFFER: u32 = 0x8D40;
const GL_COLOR_ATTACHMENT0: u32 = 0x8CE0;
const GL_FRAMEBUFFER_COMPLETE: u32 = 0x8CD5;
const GL_VERTEX_SHADER: u32 = 0x8B31;
const GL_FRAGMENT_SHADER: u32 = 0x8B30;
const GL_COMPILE_STATUS: u32 = 0x8B81;
const GL_LINK_STATUS: u32 = 0x8B82;
const GL_FLOAT: u32 = 0x1406;
const GL_TRIANGLE_STRIP: u32 = 0x0005;
const GL_PACK_ALIGNMENT: u32 = 0x0D05;
const GL_TEXTURE0: u32 = 0x84C0;

macro_rules! fns {
    ($($name:ident: fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        #[allow(non_snake_case)]
        struct Fns { $($name: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)* }
        impl Fns {
            fn load(get: &dyn Fn(&CStr) -> Ptr) -> Result<Fns, String> {
                Ok(Fns { $($name: {
                    let p = get(&std::ffi::CString::new(stringify!($name)).unwrap());
                    if p.is_null() {
                        return Err(format!("{} is missing", stringify!($name)));
                    }
                    // SAFETY: the symbol has this C signature (EGL 1.5, GLES 2 and their extensions).
                    unsafe { std::mem::transmute::<Ptr, unsafe extern "C" fn($($arg),*) $(-> $ret)?>(p) }
                },)* })
            }
        }
    };
}

fns! {
    eglInitialize: fn(Ptr, *mut EglInt, *mut EglInt) -> u32;
    eglBindAPI: fn(u32) -> u32;
    eglCreateContext: fn(Ptr, Ptr, Ptr, *const EglInt) -> Ptr;
    eglMakeCurrent: fn(Ptr, Ptr, Ptr, Ptr) -> u32;
    eglQueryString: fn(Ptr, EglInt) -> *const c_char;
    eglGetError: fn() -> EglInt;
    eglGetPlatformDisplayEXT: fn(u32, Ptr, *const EglInt) -> Ptr;
    eglQueryDevicesEXT: fn(EglInt, *mut Ptr, *mut EglInt) -> u32;
    eglQueryDeviceStringEXT: fn(Ptr, EglInt) -> *const c_char;
    eglCreateImageKHR: fn(Ptr, Ptr, u32, Ptr, *const EglInt) -> Ptr;
    eglDestroyImageKHR: fn(Ptr, Ptr) -> u32;
    glEGLImageTargetTexture2DOES: fn(u32, Ptr);
    glGenTextures: fn(i32, *mut u32);
    glDeleteTextures: fn(i32, *const u32);
    glBindTexture: fn(u32, u32);
    glActiveTexture: fn(u32);
    glTexParameteri: fn(u32, u32, i32);
    glTexImage2D: fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void);
    glGenFramebuffers: fn(i32, *mut u32);
    glBindFramebuffer: fn(u32, u32);
    glFramebufferTexture2D: fn(u32, u32, u32, u32, i32);
    glCheckFramebufferStatus: fn(u32) -> u32;
    glViewport: fn(i32, i32, i32, i32);
    glReadPixels: fn(i32, i32, i32, i32, u32, u32, *mut c_void);
    glPixelStorei: fn(u32, i32);
    glCreateShader: fn(u32) -> u32;
    glShaderSource: fn(u32, i32, *const *const c_char, *const i32);
    glCompileShader: fn(u32);
    glGetShaderiv: fn(u32, u32, *mut i32);
    glCreateProgram: fn() -> u32;
    glAttachShader: fn(u32, u32);
    glBindAttribLocation: fn(u32, u32, *const c_char);
    glLinkProgram: fn(u32);
    glGetProgramiv: fn(u32, u32, *mut i32);
    glUseProgram: fn(u32);
    glGetUniformLocation: fn(u32, *const c_char) -> i32;
    glUniform1i: fn(i32, i32);
    glVertexAttribPointer: fn(u32, i32, u32, u8, i32, *const c_void);
    glEnableVertexAttribArray: fn(u32);
    glDrawArrays: fn(u32, i32, i32);
    glGetError: fn() -> u32;
}

/// A dma-buf plane of the framebuffer.
pub struct PlaneDesc {
    pub fd: i32,
    pub offset: u32,
    pub pitch: u32,
    pub modifier: u64,
}

pub struct Gl {
    f: Fns,
    display: Ptr,
    /// A program sampling a 2D texture, and one sampling an external one (formats only the
    /// external target takes).
    programs: [u32; 2],
    target: (u32, u32, u32, u32),
    rgba: Vec<u8>,
}

fn dlopen(names: &[&str]) -> Option<Ptr> {
    names.iter().find_map(|n| {
        let c = std::ffi::CString::new(*n).ok()?;
        // SAFETY: loading a system library by its soname.
        let h = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        (!h.is_null()).then_some(h)
    })
}

const VS: &str = "attribute vec2 p; varying vec2 t; void main() { t = p * 0.5 + 0.5; gl_Position = vec4(p, 0.0, 1.0); }\0";
const FS_2D: &str = "precision mediump float; uniform sampler2D s; varying vec2 t; void main() { gl_FragColor = texture2D(s, t); }\0";
const FS_EXT: &str =
    "#extension GL_OES_EGL_image_external : require\nprecision mediump float; uniform samplerExternalOES s; varying vec2 t; void main() { gl_FragColor = texture2D(s, t); }\0";

impl Gl {
    /// EGL on the GPU of `card` (as `/dev/dri/card0`), with no window.
    pub fn open(card: &str) -> Result<Gl, String> {
        let egl = dlopen(&["libEGL.so.1", "libEGL.so"]).ok_or("libEGL is not installed")?;
        let gles = dlopen(&["libGLESv2.so.2", "libGLESv2.so"]);
        // SAFETY: looking up symbols in libraries that stay loaded for the program's life.
        let get_proc: unsafe extern "C" fn(*const c_char) -> Ptr = unsafe {
            let p = libc::dlsym(egl, c"eglGetProcAddress".as_ptr());
            if p.is_null() {
                return Err("libEGL has no eglGetProcAddress".into());
            }
            std::mem::transmute::<Ptr, unsafe extern "C" fn(*const c_char) -> Ptr>(p)
        };
        let get = |name: &CStr| -> Ptr {
            // SAFETY: as above.
            unsafe {
                let mut p = libc::dlsym(egl, name.as_ptr());
                if p.is_null()
                    && let Some(g) = gles
                {
                    p = libc::dlsym(g, name.as_ptr());
                }
                if p.is_null() { get_proc(name.as_ptr()) } else { p }
            }
        };
        let f = Fns::load(&get)?;
        // SAFETY: EGL and GL calls with valid arguments, on this thread only.
        unsafe {
            let display = Self::display(&f, card)?;
            let (mut major, mut minor) = (0, 0);
            if (f.eglInitialize)(display, &mut major, &mut minor) == 0 {
                return Err(format!("EGL did not start (0x{:x})", (f.eglGetError)()));
            }
            let ext = (f.eglQueryString)(display, EGL_EXTENSIONS);
            let ext = if ext.is_null() { String::new() } else { CStr::from_ptr(ext).to_string_lossy().into_owned() };
            for need in ["EGL_EXT_image_dma_buf_import", "EGL_KHR_surfaceless_context", "EGL_KHR_no_config_context"] {
                if !ext.split(' ').any(|e| e == need) {
                    return Err(format!("this GPU's EGL lacks {need}"));
                }
            }
            (f.eglBindAPI)(EGL_OPENGL_ES_API);
            let attrs = [EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE];
            let ctx = (f.eglCreateContext)(display, std::ptr::null_mut(), std::ptr::null_mut(), attrs.as_ptr());
            if ctx.is_null() {
                return Err(format!("no GLES context (0x{:x})", (f.eglGetError)()));
            }
            if (f.eglMakeCurrent)(display, std::ptr::null_mut(), std::ptr::null_mut(), ctx) == 0 {
                return Err(format!("the GLES context did not start (0x{:x})", (f.eglGetError)()));
            }
            let mut gl = Gl { f, display, programs: [0, 0], target: (0, 0, 0, 0), rgba: Vec::new() };
            gl.programs[0] = gl.program(FS_2D)?;
            gl.programs[1] = gl.program(FS_EXT).unwrap_or(0);
            Ok(gl)
        }
    }

    /// The EGL display of the GPU that drives `card`, else the default one.
    unsafe fn display(f: &Fns, card: &str) -> Result<Ptr, String> {
        // SAFETY: the caller's.
        unsafe {
            let mut n = 0;
            let mut devices = [std::ptr::null_mut(); 16];
            if (f.eglQueryDevicesEXT)(16, devices.as_mut_ptr(), &mut n) != 0 {
                for &d in &devices[..n.clamp(0, 16) as usize] {
                    let file = (f.eglQueryDeviceStringEXT)(d, EGL_DRM_DEVICE_FILE_EXT);
                    if !file.is_null() && CStr::from_ptr(file).to_str() == Ok(card) {
                        let display = (f.eglGetPlatformDisplayEXT)(EGL_PLATFORM_DEVICE_EXT, d, [EGL_NONE].as_ptr());
                        if !display.is_null() {
                            return Ok(display);
                        }
                    }
                }
            }
            let display = (f.eglGetPlatformDisplayEXT)(EGL_PLATFORM_SURFACELESS_MESA, std::ptr::null_mut(), [EGL_NONE].as_ptr());
            if display.is_null() { Err("no EGL display for the GPU".into()) } else { Ok(display) }
        }
    }

    unsafe fn program(&self, fs: &str) -> Result<u32, String> {
        let f = &self.f;
        // SAFETY: GL calls on the current context.
        unsafe {
            let shader = |kind, src: &str| -> Result<u32, String> {
                let s = (f.glCreateShader)(kind);
                let p = src.as_ptr() as *const c_char;
                (f.glShaderSource)(s, 1, &p, std::ptr::null());
                (f.glCompileShader)(s);
                let mut ok = 0;
                (f.glGetShaderiv)(s, GL_COMPILE_STATUS, &mut ok);
                if ok == 0 { Err("a shader did not compile".into()) } else { Ok(s) }
            };
            let (v, fr) = (shader(GL_VERTEX_SHADER, VS)?, shader(GL_FRAGMENT_SHADER, fs)?);
            let prog = (f.glCreateProgram)();
            (f.glAttachShader)(prog, v);
            (f.glAttachShader)(prog, fr);
            (f.glBindAttribLocation)(prog, 0, c"p".as_ptr());
            (f.glLinkProgram)(prog);
            let mut ok = 0;
            (f.glGetProgramiv)(prog, GL_LINK_STATUS, &mut ok);
            if ok == 0 {
                return Err("the shaders did not link".into());
            }
            Ok(prog)
        }
    }

    /// The framebuffer of `planes`, `w` x `h` in `fourcc`, into `out` as BGRx rows.
    pub fn read(&mut self, w: u32, h: u32, fourcc: u32, planes: &[PlaneDesc], out: &mut Vec<u8>) -> Result<(), String> {
        let f = &self.f;
        let mut attrs = vec![EGL_WIDTH, w as EglInt, EGL_HEIGHT, h as EglInt, EGL_LINUX_DRM_FOURCC_EXT, fourcc as EglInt];
        for (p, a) in planes.iter().zip(PLANE_ATTRS) {
            attrs.extend_from_slice(&[a[0], p.fd, a[1], p.offset as EglInt, a[2], p.pitch as EglInt]);
            attrs.extend_from_slice(&[a[3], (p.modifier & 0xffff_ffff) as u32 as EglInt, a[4], (p.modifier >> 32) as u32 as EglInt]);
        }
        attrs.push(EGL_NONE);
        // SAFETY: GL and EGL calls on this thread's current context with buffers of the sizes given.
        unsafe {
            let image = (f.eglCreateImageKHR)(self.display, std::ptr::null_mut(), EGL_LINUX_DMA_BUF_EXT, std::ptr::null_mut(), attrs.as_ptr());
            if image.is_null() {
                return Err(format!("the GPU did not take the screen's buffer (0x{:x})", (f.eglGetError)()));
            }
            // The plain texture drawn into, kept while the size stays.
            if (self.target.0, self.target.1) != (w, h) {
                if self.target.2 != 0 {
                    (f.glDeleteTextures)(1, &self.target.2);
                }
                let mut tex = 0;
                (f.glGenTextures)(1, &mut tex);
                (f.glBindTexture)(GL_TEXTURE_2D, tex);
                (f.glTexImage2D)(GL_TEXTURE_2D, 0, GL_RGBA as i32, w as i32, h as i32, 0, GL_RGBA, GL_UNSIGNED_BYTE, std::ptr::null());
                let mut fbo = self.target.3;
                if fbo == 0 {
                    (f.glGenFramebuffers)(1, &mut fbo);
                }
                (f.glBindFramebuffer)(GL_FRAMEBUFFER, fbo);
                (f.glFramebufferTexture2D)(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
                if (f.glCheckFramebufferStatus)(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE {
                    (f.eglDestroyImageKHR)(self.display, image);
                    return Err("the GPU cannot draw into a texture".into());
                }
                self.target = (w, h, tex, fbo);
            }
            let mut src = 0;
            (f.glGenTextures)(1, &mut src);
            (f.glActiveTexture)(GL_TEXTURE0);
            // Some formats are only sampled through the external target.
            let mut which = 0;
            (f.glBindTexture)(GL_TEXTURE_2D, src);
            (f.glGetError)();
            (f.glEGLImageTargetTexture2DOES)(GL_TEXTURE_2D, image);
            if (f.glGetError)() != 0 && self.programs[1] != 0 {
                (f.glBindTexture)(GL_TEXTURE_EXTERNAL_OES, src);
                (f.glEGLImageTargetTexture2DOES)(GL_TEXTURE_EXTERNAL_OES, image);
                which = 1;
            }
            let target = if which == 1 { GL_TEXTURE_EXTERNAL_OES } else { GL_TEXTURE_2D };
            (f.glTexParameteri)(target, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
            (f.glTexParameteri)(target, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
            (f.glBindFramebuffer)(GL_FRAMEBUFFER, self.target.3);
            (f.glViewport)(0, 0, w as i32, h as i32);
            (f.glUseProgram)(self.programs[which]);
            (f.glUniform1i)((f.glGetUniformLocation)(self.programs[which], c"s".as_ptr()), 0);
            let quad: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
            (f.glVertexAttribPointer)(0, 2, GL_FLOAT, 0, 0, quad.as_ptr() as *const c_void);
            (f.glEnableVertexAttribArray)(0);
            (f.glDrawArrays)(GL_TRIANGLE_STRIP, 0, 4);
            // Row 0 of what is read is the bottom of the drawing, the buffer's first row.
            self.rgba.resize(w as usize * h as usize * 4, 0);
            (f.glPixelStorei)(GL_PACK_ALIGNMENT, 1);
            (f.glReadPixels)(0, 0, w as i32, h as i32, GL_RGBA, GL_UNSIGNED_BYTE, self.rgba.as_mut_ptr() as *mut c_void);
            let err = (f.glGetError)();
            (f.glDeleteTextures)(1, &src);
            (f.eglDestroyImageKHR)(self.display, image);
            if err != 0 {
                return Err(format!("reading the screen back from the GPU failed (0x{err:x})"));
            }
        }
        out.resize(self.rgba.len(), 0);
        for (d, s) in out.as_chunks_mut::<4>().0.iter_mut().zip(self.rgba.as_chunks::<4>().0) {
            *d = [s[2], s[1], s[0], 255];
        }
        Ok(())
    }
}
