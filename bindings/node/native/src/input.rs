//! Evaluate input once on the environment thread, then commit its owned encoding.
use super::*;
use napi::sys::{self, ValueType as Type};
use napi::{JsObject, JsUnknown, NapiRaw};

type Raw = sys::napi_value;
struct ValueScope {
    env: sys::napi_env,
    scope: sys::napi_handle_scope,
}
impl ValueScope {
    fn new(env: Env) -> Result<Self> {
        let mut scope = ptr::null_mut();
        napi::check_status!(unsafe { sys::napi_open_handle_scope(env.raw(), &mut scope) })?;
        Ok(Self {
            env: env.raw(),
            scope,
        })
    }
}
impl Drop for ValueScope {
    fn drop(&mut self) {
        // Nested Rust guards close in reverse order, including on hook errors.
        unsafe { sys::napi_close_handle_scope(self.env, self.scope) };
    }
}
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
    u32,
    *mut *const views::Owner,
    *mut *mut plsm_error_t,
) -> i32;
type Forward = unsafe extern "C" fn(
    *mut plsm_pool_t,
    *const views::Owner,
    u32,
    *mut *const views::Owner,
    *mut *mut plsm_error_t,
) -> i32;
type Json = unsafe extern "C" fn(
    *mut plsm_pool_t,
    *const u8,
    usize,
    *const *const c_char,
    usize,
    u32,
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
fn api() -> Option<&'static Api> {
    static API: std::sync::OnceLock<Option<Api>> = std::sync::OnceLock::new();
    API.get_or_init(|| {
        macro_rules! load {
            ($name:expr, $kind:ty) => {{
                let symbol = views::optional_symbol($name);
                if symbol.is_null() {
                    return None;
                }
                unsafe { std::mem::transmute::<*mut libc::c_void, $kind>(symbol) }
            }};
        }
        Some(Api {
            new: load!(c"plsm_builder_new", New),
            put: load!(c"plsm_builder_put", Put),
            copy: load!(c"plsm_builder_copy_value", CopyValue),
            commit: load!(c"plsm_pool_append_builder", Commit),
            free: load!(c"plsm_builder_free", Free),
            forward: load!(c"plsm_pool_append_view", Forward),
            json: load!(c"plsm_pool_append_view_json", Json),
        })
    })
    .as_ref()
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
fn property(env: Env, object: Raw, name: &std::ffi::CStr) -> Result<Raw> {
    let mut result = ptr::null_mut();
    napi::check_status!(unsafe {
        sys::napi_get_named_property(env.raw(), object, name.as_ptr(), &mut result)
    })?;
    Ok(result)
}
fn call(env: Env, function: Raw, receiver: Raw, args: &[Raw]) -> Result<Raw> {
    let mut result = ptr::null_mut();
    napi::check_status!(unsafe {
        sys::napi_call_function(
            env.raw(),
            receiver,
            function,
            args.len(),
            args.as_ptr(),
            &mut result,
        )
    })?;
    Ok(result)
}
fn number(env: Env, value: Raw) -> Result<f64> {
    let mut result = 0.0;
    napi::check_status!(unsafe { sys::napi_get_value_double(env.raw(), value, &mut result) })?;
    Ok(result)
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

#[derive(Clone, Copy)]
enum HookKey {
    Root,
    Name(Raw),
    Index(u64),
}
impl HookKey {
    fn raw(self, env: Env) -> Result<Raw> {
        match self {
            Self::Name(key) => Ok(key),
            Self::Root => Ok(unsafe { env.create_string("")?.raw() }),
            Self::Index(index) => Ok(unsafe { env.create_string(&index.to_string())?.raw() }),
        }
    }
}

struct Encoder {
    env: Env,
    builder: OwnedBuilder,
    data: usize,
    classify: Raw,
    unbox: Raw,
    binary_hook: Raw,
    bigint_hook: Raw,
    view: Raw,
    undefined: Raw,
    stack: Vec<Raw>,
}
impl Encoder {
    fn new(env: Env, tags: &[String], helpers: JsObject, api: &'static Api) -> Result<Self> {
        let tags = tags
            .iter()
            .map(|s| Text {
                data: s.as_ptr(),
                len: s.len(),
            })
            .collect::<Vec<_>>();
        let mut pointer = ptr::null_mut();
        let mut data = 0;
        let mut error = ptr::null_mut();
        if unsafe {
            (api.new)(
                tags.as_ptr(),
                tags.len(),
                &mut pointer,
                &mut data,
                &mut error,
            )
        } != 0
        {
            return Err(take_error(env, error)?);
        }
        let builder = OwnedBuilder { pointer, api };
        let helpers = unsafe { helpers.raw() };
        Ok(Self {
            env,
            builder,
            data,
            classify: property(env, helpers, c"classify")?,
            unbox: property(env, helpers, c"unbox")?,
            binary_hook: property(env, helpers, c"bufferToJSON")?,
            bigint_hook: property(env, helpers, c"bigintHook")?,
            view: property(env, helpers, c"view")?,
            undefined: unsafe { env.get_undefined()?.raw() },
            stack: Vec::new(),
        })
    }
    fn class(&self, value: Raw) -> Result<u32> {
        let result = call(self.env, self.classify, self.undefined, &[value])?;
        Ok(number(self.env, result)? as u32)
    }
    fn normalize(&self, mut value: Raw, key: HookKey) -> Result<(Raw, u32)> {
        let mut ty = kind(self.env, value)?;
        let mut class = if ty == Type::napi_object {
            self.class(value)?
        } else {
            0
        };
        if class == 13 {
            return Ok((value, class));
        }
        if matches!(
            ty,
            Type::napi_object | Type::napi_function | Type::napi_bigint
        ) {
            let hook = if ty == Type::napi_bigint {
                call(self.env, self.bigint_hook, self.undefined, &[value])?
            } else {
                property(self.env, value, c"toJSON")?
            };
            let mut default_binary = false;
            if class == 6 {
                napi::check_status!(unsafe {
                    sys::napi_strict_equals(
                        self.env.raw(),
                        hook,
                        self.binary_hook,
                        &mut default_binary,
                    )
                })?;
            }
            if kind(self.env, hook)? == Type::napi_function && !default_binary {
                value = call(self.env, hook, value, &[key.raw(self.env)?])?;
                ty = kind(self.env, value)?;
                class = if ty == Type::napi_object {
                    self.class(value)?
                } else {
                    0
                };
            }
            if (9..=12).contains(&class) {
                let code = unsafe { self.env.create_uint32(class)?.raw() };
                value = call(self.env, self.unbox, self.undefined, &[value, code])?;
                class = 0;
            }
        }
        Ok((value, class))
    }
    fn put(&self, parent: usize, key: Option<&str>, value: &Input) -> Result<usize> {
        let mut child = 0;
        let mut error = ptr::null_mut();
        let (key, len) = key.map_or((ptr::null(), 0), |s| (s.as_ptr(), s.len()));
        if unsafe {
            (self.builder.api.put)(
                self.builder.pointer,
                parent,
                key,
                len,
                value,
                &mut child,
                &mut error,
            )
        } != 0
        {
            return Err(take_error(self.env, error)?);
        }
        Ok(child)
    }
    fn copy(&self, parent: usize, key: Option<&str>, value: Raw) -> Result<()> {
        use napi::bindgen_prelude::FromNapiValue;
        let info = call(self.env, self.view, self.undefined, &[value])?;
        let owner = property(self.env, info, c"owner")?;
        let node = number(self.env, property(self.env, info, c"node")?)? as u32;
        let owner = unsafe { <&views::NativeView>::from_napi_value(self.env.raw(), owner) }?;
        let (key, len) = key.map_or((ptr::null(), 0), |s| (s.as_ptr(), s.len()));
        let mut error = ptr::null_mut();
        if unsafe {
            (self.builder.api.copy)(
                self.builder.pointer,
                parent,
                key,
                len,
                owner.reference(node),
                &mut error,
            )
        } != 0
        {
            return Err(take_error(self.env, error)?);
        }
        Ok(())
    }
    fn enter(&mut self, value: Raw) -> Result<()> {
        for ancestor in &self.stack {
            let mut equal = false;
            napi::check_status!(unsafe {
                sys::napi_strict_equals(self.env.raw(), *ancestor, value, &mut equal)
            })?;
            if equal {
                return Err(invalid(self.env, "cannot encode a cyclic input"));
            }
        }
        // Data sits one level below the enclosing Lite3 message object.
        if self.stack.len() >= 31 {
            return Err(invalid(self.env, "input exceeds Lite3 nesting depth"));
        }
        self.stack.push(value);
        Ok(())
    }
    fn members(&mut self, parent: usize, value: Raw, class: u32) -> Result<()> {
        self.enter(value)?;
        if class == 8 {
            let length = property(self.env, value, c"length")?;
            let mut numeric = ptr::null_mut();
            napi::check_status!(unsafe {
                sys::napi_coerce_to_number(self.env.raw(), length, &mut numeric)
            })?;
            let length = number(self.env, numeric)?
                .clamp(0.0, 9007199254740991.0)
                .floor() as u64;
            for start in (0..length).step_by(16) {
                let _scope = if length > 1 {
                    Some(ValueScope::new(self.env)?)
                } else {
                    None
                };
                for index in start..(start + 16).min(length) {
                    let mut child = ptr::null_mut();
                    if let Ok(index) = u32::try_from(index) {
                        napi::check_status!(unsafe {
                            sys::napi_get_element(self.env.raw(), value, index, &mut child)
                        })?;
                    } else {
                        let key = HookKey::Index(index).raw(self.env)?;
                        napi::check_status!(unsafe {
                            sys::napi_get_property(self.env.raw(), value, key, &mut child)
                        })?;
                    }
                    self.value(parent, None, HookKey::Index(index), child, true)?;
                }
            }
        } else {
            let mut keys = ptr::null_mut();
            napi::check_status!(unsafe {
                sys::napi_get_all_property_names(
                    self.env.raw(),
                    value,
                    sys::KeyCollectionMode::own_only,
                    sys::KeyFilter::enumerable | sys::KeyFilter::skip_symbols,
                    sys::KeyConversion::numbers_to_strings,
                    &mut keys,
                )
            })?;
            let mut count = 0;
            napi::check_status!(unsafe {
                sys::napi_get_array_length(self.env.raw(), keys, &mut count)
            })?;
            for start in (0..count).step_by(16) {
                let _scope = if count > 1 {
                    Some(ValueScope::new(self.env)?)
                } else {
                    None
                };
                for index in start..(start + 16).min(count) {
                    let mut key = ptr::null_mut();
                    let mut child = ptr::null_mut();
                    napi::check_status!(unsafe {
                        sys::napi_get_element(self.env.raw(), keys, index, &mut key)
                    })?;
                    napi::check_status!(unsafe {
                        sys::napi_get_property(self.env.raw(), value, key, &mut child)
                    })?;
                    let name = text(self.env, key)?;
                    self.value(parent, Some(&name), HookKey::Name(key), child, false)?;
                }
            }
        }
        self.stack.pop();
        Ok(())
    }
    fn value(
        &mut self,
        parent: usize,
        key: Option<&str>,
        hook_key: HookKey,
        value: Raw,
        array: bool,
    ) -> Result<()> {
        let (value, class) = self.normalize(value, hook_key)?;
        if class == 13 {
            return self.copy(parent, key, value);
        }
        let mut input = Input::default();
        match kind(self.env, value)? {
            Type::napi_undefined | Type::napi_function | Type::napi_symbol if !array => {
                return Ok(());
            }
            Type::napi_undefined | Type::napi_function | Type::napi_symbol | Type::napi_null => {
                input.kind = 1
            }
            Type::napi_boolean => {
                input.kind = 2;
                let mut b = false;
                napi::check_status!(unsafe {
                    sys::napi_get_value_bool(self.env.raw(), value, &mut b)
                })?;
                input.integer = i64::from(b);
            }
            Type::napi_bigint => {
                input.kind = 3;
                let mut lossless = false;
                napi::check_status!(unsafe {
                    sys::napi_get_value_bigint_int64(
                        self.env.raw(),
                        value,
                        &mut input.integer,
                        &mut lossless,
                    )
                })?;
                if !lossless {
                    return Err(invalid(self.env, "bigint must fit signed i64"));
                }
            }
            Type::napi_number => {
                let number = number(self.env, value)?;
                if !number.is_finite() {
                    return Err(invalid(self.env, "number must be finite"));
                }
                if number.abs() <= 9007199254740991.0
                    && number.fract() == 0.0
                    && !(number == 0.0 && number.is_sign_negative())
                {
                    input.kind = 3;
                    input.integer = number as i64;
                } else {
                    input.kind = 4;
                    input.real = number;
                }
            }
            Type::napi_string => {
                let string = text(self.env, value)?;
                input.kind = 5;
                input.bytes = string.as_ptr();
                input.len = string.len();
                self.put(parent, key, &input)?;
                return Ok(());
            }
            Type::napi_object if class == 6 => {
                let mut ty = sys::TypedarrayType::uint8_array;
                let mut data = ptr::null_mut();
                let mut buffer = ptr::null_mut();
                let mut offset = 0;
                napi::check_status!(unsafe {
                    sys::napi_get_typedarray_info(
                        self.env.raw(),
                        value,
                        &mut ty,
                        &mut input.len,
                        &mut data,
                        &mut buffer,
                        &mut offset,
                    )
                })?;
                let mut ordinary = false;
                napi::check_status!(unsafe {
                    sys::napi_is_arraybuffer(self.env.raw(), buffer, &mut ordinary)
                })?;
                if !ordinary {
                    return Err(invalid(
                        self.env,
                        "shared-backed bytes require an ordinary buffer copy",
                    ));
                }
                input.kind = 6;
                input.bytes = data.cast();
            }
            Type::napi_object => {
                input.kind = class;
                let child = self.put(parent, key, &input)?;
                return self.members(child, value, class);
            }
            _ => return Err(invalid(self.env, "unsupported native input")),
        }
        self.put(parent, key, &input)?;
        Ok(())
    }
    fn encode(mut self, value: JsUnknown) -> Result<OwnedBuilder> {
        let (value, class) = self.normalize(unsafe { value.raw() }, HookKey::Root)?;
        if class == 13 {
            self.copy(usize::MAX, None, value)?;
        } else if class == 7 {
            self.members(self.data, value, class)?;
        } else {
            return Err(invalid(self.env, "message data must encode as an object"));
        }
        Ok(self.builder)
    }
}

#[napi]
pub struct NativeAppendTarget {
    handle: Option<Arc<PoolHandle>>,
    durability: u32,
}
impl NativeAppendTarget {
    pub(super) fn new(handle: Arc<PoolHandle>, durability: u32) -> Self {
        Self {
            handle: Some(handle),
            durability,
        }
    }
    fn consume(&mut self, env: Env) -> Result<(Arc<PoolHandle>, &'static Api)> {
        let handle = self
            .handle
            .take()
            .ok_or_else(|| pool_error(env, "Usage", "append target was already consumed"))?;
        let api = api().ok_or_else(|| {
            pool_error(
                env,
                "Usage",
                "loaded libplasmite lacks native input support; install matching native assets",
            )
        })?;
        Ok((handle, api))
    }
}
#[napi]
impl NativeAppendTarget {
    #[napi]
    pub fn discard(&mut self) {
        self.handle.take();
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
    #[napi]
    pub fn encode(
        &mut self,
        env: Env,
        value: JsUnknown,
        tags: Vec<String>,
        helpers: JsObject,
    ) -> Result<AsyncTask<AppendTask>> {
        let (handle, api) = self.consume(env)?;
        let builder = Encoder::new(env, &tags, helpers, api)?.encode(value)?;
        Ok(AsyncTask::new(AppendTask {
            handle,
            input: AppendInput::Encoded(builder),
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
        let rc = match &self.input {
            AppendInput::Encoded(builder) => {
                let _operation = lock()?;
                unsafe {
                    (builder.api.commit)(
                        self.handle.ptr,
                        builder.pointer,
                        self.durability,
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
                let api = api().expect("prepared input ABI");
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
                                self.durability,
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
                let tags_c = CStringArray::new(tags)?;
                let _operation = lock()?;
                unsafe {
                    (api().expect("prepared input ABI").json)(
                        self.handle.ptr,
                        text.as_ptr(),
                        text.len(),
                        tags_c.as_ptr(),
                        tags.len(),
                        self.durability,
                        &mut owner,
                        &mut error,
                    )
                }
            }
            AppendInput::Forwarded(Some(source)) => {
                let _operation = lock()?;
                unsafe {
                    (api().expect("prepared input ABI").forward)(
                        self.handle.ptr,
                        source.pointer(),
                        self.durability,
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
