//! Evaluate input once on the environment thread, then commit its owned encoding.
use super::*;
use napi::sys::{self, ValueType as Type};
use napi::{JsUnknown, NapiRaw};

type Raw = sys::napi_value;
#[repr(C)]
struct Builder {
    _private: [u8; 0],
}
#[repr(C)]
struct Text {
    data: *const u8,
    len: usize,
}
#[repr(C)]
#[derive(Default)]
struct Input {
    kind: u32,
    integer: i64,
    real: f64,
    bytes: *const u8,
    len: usize,
}
type New = unsafe extern "C" fn(
    *const Text,
    usize,
    *mut *mut Builder,
    *mut usize,
    *mut *mut plsm_error_t,
) -> i32;
type Put = unsafe extern "C" fn(
    *mut Builder,
    usize,
    *const u8,
    usize,
    *const Input,
    *mut usize,
    *mut *mut plsm_error_t,
) -> i32;
type CopyValue = unsafe extern "C" fn(
    *mut Builder,
    usize,
    *const u8,
    usize,
    views::ValueRef,
    *mut *mut plsm_error_t,
) -> i32;
type Commit = unsafe extern "C" fn(
    *mut plsm_pool_t,
    *mut Builder,
    *const plsm_feed_options_t,
    *mut *const views::Owner,
    *mut *mut plsm_error_t,
) -> i32;
type Forward = unsafe extern "C" fn(
    *mut plsm_pool_t,
    *const views::Owner,
    *const plsm_feed_options_t,
    *mut *const views::Owner,
    *mut *mut plsm_error_t,
) -> i32;
type Json = unsafe extern "C" fn(
    *mut plsm_pool_t,
    *const u8,
    usize,
    *const plsm_feed_options_t,
    *mut *const views::Owner,
    *mut *mut plsm_error_t,
) -> i32;
type Free = unsafe extern "C" fn(*mut Builder);
struct Api {
    new: New,
    put: Put,
    copy: CopyValue,
    commit: Commit,
    free: Free,
    forward: Forward,
    json: Json,
}
#[cfg_attr(windows, link(name = "plasmite2", kind = "raw-dylib"))]
unsafe extern "C" {
    fn plsm_builder_new(
        arg0: *const Text,
        arg1: usize,
        arg2: *mut *mut Builder,
        arg3: *mut usize,
        arg4: *mut *mut plsm_error_t,
    ) -> i32;
    fn plsm_builder_put(
        arg0: *mut Builder,
        arg1: usize,
        arg2: *const u8,
        arg3: usize,
        arg4: *const Input,
        arg5: *mut usize,
        arg6: *mut *mut plsm_error_t,
    ) -> i32;
    fn plsm_builder_copy_value(
        arg0: *mut Builder,
        arg1: usize,
        arg2: *const u8,
        arg3: usize,
        arg4: views::ValueRef,
        arg5: *mut *mut plsm_error_t,
    ) -> i32;
    fn plsm_pool_feed_builder_view(
        arg0: *mut plsm_pool_t,
        arg1: *mut Builder,
        arg2: *const plsm_feed_options_t,
        arg3: *mut *const views::Owner,
        arg4: *mut *mut plsm_error_t,
    ) -> i32;
    fn plsm_builder_free(arg0: *mut Builder);
    fn plsm_pool_feed_view(
        arg0: *mut plsm_pool_t,
        arg1: *const views::Owner,
        arg2: *const plsm_feed_options_t,
        arg3: *mut *const views::Owner,
        arg4: *mut *mut plsm_error_t,
    ) -> i32;
    fn plsm_pool_feed_json_view(
        arg0: *mut plsm_pool_t,
        arg1: *const u8,
        arg2: usize,
        arg3: *const plsm_feed_options_t,
        arg4: *mut *const views::Owner,
        arg5: *mut *mut plsm_error_t,
    ) -> i32;
}

fn api() -> &'static Api {
    static API: Api = Api {
        new: plsm_builder_new,
        put: plsm_builder_put,
        copy: plsm_builder_copy_value,
        commit: plsm_pool_feed_builder_view,
        free: plsm_builder_free,
        forward: plsm_pool_feed_view,
        json: plsm_pool_feed_json_view,
    };
    &API
}
struct OwnedBuilder {
    pointer: *mut Builder,
    api: &'static Api,
}
// The completed builder owns only native bytes; a single worker consumes it.
unsafe impl Send for OwnedBuilder {}
impl Drop for OwnedBuilder {
    fn drop(&mut self) {
        unsafe { (self.api.free)(self.pointer) }
    }
}

fn invalid(env: Env, message: &str) -> Error {
    let value =
        unsafe { napi::JsTypeError::from(Error::from_reason(message)).into_value(env.raw()) };
    unsafe { sys::napi_throw(env.raw(), value) };
    Error::new(Status::PendingException, "")
}
// Keep synchronous VM exceptions pending: napi's Error::from(JsUnknown)
// coerces them to text and cannot reference every primitive thrown value.
// Returning PendingException preserves identity without invoking more user code.

fn kind(env: Env, value: Raw) -> Result<sys::napi_valuetype> {
    let mut result = Type::napi_undefined;
    napi::check_status!(unsafe { sys::napi_typeof(env.raw(), value, &mut result) })?;
    Ok(result)
}
// Inspect the actual backing store before passing bytes to C. In particular,
// do not construct a Rust slice over concurrently mutable shared memory.
fn typed_span(
    env: Env,
    value: Raw,
    expected: sys::napi_typedarray_type,
) -> Result<(*const u8, usize)> {
    let mut ty = expected;
    let mut data = ptr::null_mut();
    let mut buffer = ptr::null_mut();
    let mut length = 0;
    let mut offset = 0;
    napi::check_status!(unsafe {
        sys::napi_get_typedarray_info(
            env.raw(),
            value,
            &mut ty,
            &mut length,
            &mut data,
            &mut buffer,
            &mut offset,
        )
    })?;
    if ty != expected {
        return Err(invalid(env, "unexpected captured typed array"));
    }
    let mut ordinary = false;
    napi::check_status!(unsafe { sys::napi_is_arraybuffer(env.raw(), buffer, &mut ordinary) })?;
    if !ordinary {
        return Err(invalid(
            env,
            "shared-backed bytes require an ordinary buffer copy",
        ));
    }
    Ok((data.cast(), length))
}
fn byte_span(env: Env, value: Raw) -> Result<(*const u8, usize)> {
    typed_span(env, value, sys::TypedarrayType::uint8_array)
}

// Transfer the VM's UTF8 write buffer into String without napi's extra Vec copy.
// Only U+FFFD output can conceal replacement of an isolated UTF16 surrogate.
fn text(env: Env, value: Raw) -> Result<String> {
    let mut len = 0;
    napi::check_status!(unsafe {
        sys::napi_get_value_string_utf8(env.raw(), value, ptr::null_mut(), 0, &mut len)
    })?;
    let mut bytes = Vec::<u8>::with_capacity(len + 1);
    napi::check_status!(unsafe {
        sys::napi_get_value_string_utf8(
            env.raw(),
            value,
            bytes.as_mut_ptr().cast(),
            len + 1,
            &mut len,
        )
    })?;
    unsafe { bytes.set_len(len) };
    let text =
        String::from_utf8(bytes).map_err(|_| invalid(env, "input string is not valid UTF8"))?;
    if text.contains('\u{fffd}') {
        let mut len = 0;
        napi::check_status!(unsafe {
            sys::napi_get_value_string_utf16(env.raw(), value, ptr::null_mut(), 0, &mut len)
        })?;
        let mut units = Vec::<u16>::with_capacity(len + 1);
        napi::check_status!(unsafe {
            sys::napi_get_value_string_utf16(
                env.raw(),
                value,
                units.as_mut_ptr(),
                len + 1,
                &mut len,
            )
        })?;
        unsafe { units.set_len(len) };
        if char::decode_utf16(units).any(|unit| unit.is_err()) {
            return Err(invalid(
                env,
                "input string contains an isolated UTF16 surrogate",
            ));
        }
    }
    Ok(text)
}

#[napi]
pub struct NativeAppendTarget {
    handle: Option<Arc<PoolHandle>>,
    durability: u32,
    capture: Option<Capture>,
}
const CAPTURE_END: u32 = 9;
const CAPTURE_NUMBER_I64: u32 = 10;
struct Capture {
    builder: OwnedBuilder,
    parents: Vec<usize>,
}
impl Capture {
    fn put(&mut self, env: Env, key: Option<&str>, value: &Input) -> Result<()> {
        let (key, len) = key.map_or((ptr::null(), 0), |key| (key.as_ptr(), key.len()));
        let mut child = 0;
        let mut error = ptr::null_mut();
        if unsafe {
            (self.builder.api.put)(
                self.builder.pointer,
                *self.parents.last().expect("root parent"),
                key,
                len,
                value,
                &mut child,
                &mut error,
            )
        } != 0
        {
            return Err(take_error(env, error)?);
        }
        if value.kind == 7 || value.kind == 8 {
            self.parents.push(child);
        }
        Ok(())
    }
}
impl NativeAppendTarget {
    pub(super) fn new(handle: Arc<PoolHandle>, durability: u32) -> Self {
        Self {
            handle: Some(handle),
            durability,
            capture: None,
        }
    }
    fn consume(&mut self, env: Env) -> Result<(Arc<PoolHandle>, &'static Api)> {
        let handle = self
            .handle
            .take()
            .ok_or_else(|| pool_error(env, "Usage", "append target was already consumed"))?;
        let api = api();
        Ok((handle, api))
    }
}
#[napi]
impl NativeAppendTarget {
    #[napi]
    pub fn discard(&mut self) {
        self.capture.take();
        self.handle.take();
    }
    fn begin(&mut self, env: Env, tags: Vec<String>) -> Result<()> {
        if self.handle.is_none() || self.capture.is_some() {
            return Err(pool_error(
                env,
                "Usage",
                "append target is not ready for capture",
            ));
        }
        let api = api();
        let spans: Vec<_> = tags
            .iter()
            .map(|tag| Text {
                data: tag.as_ptr(),
                len: tag.len(),
            })
            .collect();
        let mut pointer = ptr::null_mut();
        let mut data = 0;
        let mut error = ptr::null_mut();
        if unsafe {
            (api.new)(
                spans.as_ptr(),
                spans.len(),
                &mut pointer,
                &mut data,
                &mut error,
            )
        } != 0
        {
            return Err(take_error(env, error)?);
        }
        let mut parents = Vec::with_capacity(4);
        parents.push(data);
        self.capture = Some(Capture {
            builder: OwnedBuilder { pointer, api },
            parents,
        });
        Ok(())
    }
    #[napi]
    pub fn write(
        &mut self,
        env: Env,
        ops: JsUnknown,
        numbers: JsUnknown,
        strings: Vec<String>,
        count: u32,
        tags: Option<Vec<String>>,
    ) -> Result<()> {
        let (ops, len) = typed_span(env, unsafe { ops.raw() }, sys::TypedarrayType::uint32_array)?;
        let (numbers, numeric_len) = typed_span(
            env,
            unsafe { numbers.raw() },
            sys::TypedarrayType::float64_array,
        )?;
        let count = count as usize;
        if count > 512 || len < count * 3 || numeric_len < count {
            return Err(invalid(env, "invalid captured input batch"));
        }
        if self.capture.is_none() {
            self.begin(
                env,
                tags.ok_or_else(|| pool_error(env, "Usage", "captured input tags are missing"))?,
            )?;
        }
        let capture = self.capture.as_mut().expect("initialized capture");
        for row in 0..count {
            let command = unsafe { std::slice::from_raw_parts(ops.cast::<u32>().add(row * 3), 3) };
            if command[0] == CAPTURE_END {
                if capture.parents.len() <= 1 {
                    return Err(invalid(env, "captured parent stack is empty"));
                }
                capture.parents.pop();
                continue;
            }
            let key = if command[1] == u32::MAX {
                None
            } else {
                Some(
                    strings
                        .get(command[1] as usize)
                        .ok_or_else(|| invalid(env, "captured key is missing"))?
                        .as_str(),
                )
            };
            let mut value = Input {
                kind: command[0],
                ..Input::default()
            };
            match value.kind {
                1 | 7 | 8 => (),
                2 => value.integer = i64::from(command[2] != 0),
                3 => {
                    value.integer =
                        unsafe { numbers.cast::<u64>().add(row).read_unaligned() } as i64
                }
                4 => {
                    value.real =
                        f64::from_bits(unsafe { numbers.cast::<u64>().add(row).read_unaligned() })
                }
                5 => {
                    let text = strings
                        .get(command[2] as usize)
                        .ok_or_else(|| invalid(env, "captured text is missing"))?;
                    value.bytes = text.as_ptr();
                    value.len = text.len();
                }
                CAPTURE_NUMBER_I64 => {
                    value.kind = 3;
                    value.integer =
                        f64::from_bits(unsafe { numbers.cast::<u64>().add(row).read_unaligned() })
                            as i64;
                }
                _ => return Err(invalid(env, "invalid captured value kind")),
            }
            capture.put(env, key, &value)?;
        }
        Ok(())
    }
    #[napi]
    pub fn write_bytes(&mut self, env: Env, key: Option<String>, bytes: JsUnknown) -> Result<()> {
        let (data, len) = byte_span(env, unsafe { bytes.raw() })?;
        let capture = self
            .capture
            .as_mut()
            .ok_or_else(|| pool_error(env, "Usage", "append target has no capture"))?;
        capture.put(
            env,
            key.as_deref(),
            &Input {
                kind: 6,
                bytes: data,
                len,
                ..Input::default()
            },
        )
    }
    #[napi]
    pub fn write_copy(
        &mut self,
        env: Env,
        source: &views::NativeView,
        node: u32,
        key: Option<String>,
        root: bool,
    ) -> Result<()> {
        let capture = self
            .capture
            .as_mut()
            .ok_or_else(|| pool_error(env, "Usage", "append target has no capture"))?;
        let parent = if root {
            usize::MAX
        } else {
            *capture.parents.last().expect("root parent")
        };
        let (key, len) = key
            .as_deref()
            .map_or((ptr::null(), 0), |key| (key.as_ptr(), key.len()));
        let mut error = ptr::null_mut();
        if unsafe {
            (capture.builder.api.copy)(
                capture.builder.pointer,
                parent,
                key,
                len,
                source.reference(node),
                &mut error,
            )
        } != 0
        {
            return Err(take_error(env, error)?);
        }
        Ok(())
    }
    #[napi]
    pub fn finish(
        &mut self,
        env: Env,
        ops: JsUnknown,
        numbers: JsUnknown,
        strings: Vec<String>,
        count: u32,
        tags: Option<Vec<String>>,
    ) -> Result<AsyncTask<AppendTask>> {
        self.write(env, ops, numbers, strings, count, tags)?;
        let capture = self
            .capture
            .take()
            .ok_or_else(|| pool_error(env, "Usage", "append target has no capture"))?;
        if capture.parents.len() != 1 {
            return Err(invalid(env, "captured input has unclosed parents"));
        }
        let (handle, _) = self.consume(env)?;
        Ok(AsyncTask::new(AppendTask {
            handle,
            input: AppendInput::Encoded(capture.builder),
            durability: self.durability,
        }))
    }
    #[napi]
    pub fn copy(
        &mut self,
        env: Env,
        source: &views::NativeView,
        node: u32,
        tags: Vec<String>,
    ) -> Result<AsyncTask<AppendTask>> {
        let (handle, _) = self.consume(env)?;
        Ok(AsyncTask::new(AppendTask {
            handle,
            input: AppendInput::CopyRoot {
                source: Some(source.retained()),
                node,
                tags,
            },
            durability: self.durability,
        }))
    }
    #[napi]
    pub fn json(
        &mut self,
        env: Env,
        value: JsUnknown,
        tags: Vec<String>,
    ) -> Result<AsyncTask<AppendTask>> {
        let (handle, _) = self.consume(env)?;
        if kind(env, unsafe { value.raw() })? != Type::napi_string {
            return Err(invalid(env, "JSON input must be text"));
        }
        let text = text(env, unsafe { value.raw() })?;
        Ok(AsyncTask::new(AppendTask {
            handle,
            input: AppendInput::Json { text, tags },
            durability: self.durability,
        }))
    }
    #[napi]
    pub fn forward(
        &mut self,
        env: Env,
        source: &views::NativeView,
    ) -> Result<AsyncTask<AppendTask>> {
        let (handle, _) = self.consume(env)?;
        Ok(AsyncTask::new(AppendTask {
            handle,
            input: AppendInput::Forwarded(Some(source.retained())),
            durability: self.durability,
        }))
    }
}

enum AppendInput {
    Encoded(OwnedBuilder),
    CopyRoot {
        source: Option<views::NativeView>,
        node: u32,
        tags: Vec<String>,
    },
    Json {
        text: String,
        tags: Vec<String>,
    },
    Forwarded(Option<views::NativeView>),
}
pub struct AppendTask {
    handle: Arc<PoolHandle>,
    input: AppendInput,
    durability: u32,
}
impl Task for AppendTask {
    type Output = GetViewOutput;
    type JsValue = views::NativeView;
    fn compute(&mut self) -> Result<Self::Output> {
        let lock = || {
            self.handle
                .operation
                .lock()
                .map_err(|_| Error::from_reason("pool operation lock poisoned"))
        };
        let mut owner = ptr::null();
        let mut error = ptr::null_mut();
        let feed_options = plsm_feed_options_t {
            struct_size: std::mem::size_of::<plsm_feed_options_t>() as u32,
            durability: self.durability,
            tags: ptr::null(),
            tags_len: 0,
        };
        let rc = match &self.input {
            AppendInput::Encoded(builder) => {
                let _operation = lock()?;
                unsafe {
                    (builder.api.commit)(
                        self.handle.ptr,
                        builder.pointer,
                        &feed_options,
                        &mut owner,
                        &mut error,
                    )
                }
            }
            AppendInput::CopyRoot {
                source: Some(source),
                node,
                tags,
            } => {
                let api = api();
                let tags = tags
                    .iter()
                    .map(|s| Text {
                        data: s.as_ptr(),
                        len: s.len(),
                    })
                    .collect::<Vec<_>>();
                let mut pointer = ptr::null_mut();
                let mut data = 0;
                let mut rc = unsafe {
                    (api.new)(
                        tags.as_ptr(),
                        tags.len(),
                        &mut pointer,
                        &mut data,
                        &mut error,
                    )
                };
                if rc == 0 {
                    let builder = OwnedBuilder { pointer, api };
                    rc = unsafe {
                        (api.copy)(
                            builder.pointer,
                            usize::MAX,
                            ptr::null(),
                            0,
                            source.reference(*node),
                            &mut error,
                        )
                    };
                    if rc == 0 {
                        let _operation = lock()?;
                        rc = unsafe {
                            (api.commit)(
                                self.handle.ptr,
                                builder.pointer,
                                &feed_options,
                                &mut owner,
                                &mut error,
                            )
                        };
                    }
                }
                rc
            }
            AppendInput::CopyRoot { source: None, .. } => {
                return Err(Error::from_reason("append job already finished"));
            }
            AppendInput::Json { text, tags } => {
                let spans = text_spans(tags);
                let options = plsm_feed_options_t {
                    struct_size: std::mem::size_of::<plsm_feed_options_t>() as u32,
                    durability: self.durability,
                    tags: spans.as_ptr(),
                    tags_len: spans.len(),
                };
                let _operation = lock()?;
                unsafe {
                    (api().json)(
                        self.handle.ptr,
                        text.as_ptr(),
                        text.len(),
                        &options,
                        &mut owner,
                        &mut error,
                    )
                }
            }
            AppendInput::Forwarded(Some(source)) => {
                let _operation = lock()?;
                unsafe {
                    (api().forward)(
                        self.handle.ptr,
                        source.pointer(),
                        &feed_options,
                        &mut owner,
                        &mut error,
                    )
                }
            }
            AppendInput::Forwarded(None) => {
                return Err(Error::from_reason("append job already finished"));
            }
        };
        Ok(GetViewOutput {
            owner: if rc != 0 {
                None
            } else {
                Some(match &self.input {
                    AppendInput::Forwarded(Some(source)) => source.forwarded(owner),
                    _ => views::Owned::new(owner),
                })
            },
            error: error as usize,
        })
    }
    fn resolve(&mut self, env: Env, output: Self::Output) -> Result<Self::JsValue> {
        resolve_owned_view(env, output)
    }
    fn finally(&mut self, env: Env) -> Result<()> {
        let source = match &mut self.input {
            AppendInput::Forwarded(source) | AppendInput::CopyRoot { source, .. } => source.take(),
            _ => None,
        };
        if let Some(source) = source {
            napi::bindgen_prelude::ObjectFinalize::finalize(source, env)?;
        }
        Ok(())
    }
}
