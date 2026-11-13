use super::*;
use napi::{JsFunction, JsObject, JsUnknown, NapiRaw};

#[repr(C)]
pub struct Owner {
    _private: [u8; 0],
}
#[repr(C)]
#[derive(Clone, Copy)]
struct ValueRef {
    owner: *const Owner,
    offset: usize,
}
#[repr(C)]
#[derive(Default)]
struct Value {
    kind: u32,
    integer: i64,
    real: f64,
    bytes: *const u8,
    len: usize,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Entry {
    key: *const u8,
    key_len: usize,
    value: ValueRef,
}
#[repr(C)]
struct Cursor {
    _private: [u8; 0],
}
struct OwnedCursor(*mut Cursor);
impl Drop for OwnedCursor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { plsm_cursor_free(self.0) }
        }
    }
}
unsafe extern "C" {
    fn plsm_value_iter(value: ValueRef, out: *mut *mut Cursor, err: *mut *mut plsm_error_t) -> i32;
    fn plsm_cursor_next(
        cursor: *mut Cursor,
        slots: *mut Entry,
        capacity: usize,
        written: *mut usize,
        err: *mut *mut plsm_error_t,
    ) -> i32;
    fn plsm_cursor_free(cursor: *mut Cursor);
    fn plsm_view_release(owner: *const Owner);
    fn plsm_view_root(owner: *const Owner) -> ValueRef;
    fn plsm_view_seq(owner: *const Owner) -> u64;
    fn plsm_view_timestamp_ns(owner: *const Owner) -> u64;
    fn plsm_view_bytes(owner: *const Owner, len: *mut usize) -> *const u8;
    fn plsm_value_get(
        value: ValueRef,
        key: *const u8,
        len: usize,
        out: *mut ValueRef,
        err: *mut *mut plsm_error_t,
    ) -> i32;
    fn plsm_value_index(
        value: ValueRef,
        index: u32,
        out: *mut ValueRef,
        err: *mut *mut plsm_error_t,
    ) -> i32;
    fn plsm_value_read(value: ValueRef, out: *mut Value, err: *mut *mut plsm_error_t) -> i32;
    fn plsm_value_entries(
        value: ValueRef,
        slots: *mut Entry,
        capacity: usize,
        count: *mut usize,
        err: *mut *mut plsm_error_t,
    ) -> i32;
    fn plsm_value_to_json(
        value: ValueRef,
        out: *mut plsm_buf_t,
        err: *mut *mut plsm_error_t,
    ) -> i32;
    pub fn plsm_pool_get_view(
        pool: *mut plsm_pool_t,
        seq: u64,
        out: *mut *const Owner,
        err: *mut *mut plsm_error_t,
    ) -> i32;
}

pub struct Owned(pub *const Owner);
// The view ABI allows immutable reads on independently retained owners.
unsafe impl Send for Owned {}
unsafe impl Sync for Owned {}
impl Drop for Owned {
    fn drop(&mut self) {
        unsafe { plsm_view_release(self.0) }
    }
}

#[napi(custom_finalize)]
pub struct NativeView {
    owner: Arc<Owned>,
}
impl NativeView {
    pub fn new(mut env: Env, owner: Owned) -> Result<Self> {
        let mut len = 0;
        unsafe { plsm_view_bytes(owner.0, &mut len) };
        env.adjust_external_memory(len as i64)?;
        Ok(Self {
            owner: Arc::new(owner),
        })
    }
    fn reference(&self, node: u32) -> ValueRef {
        ValueRef {
            owner: self.owner.0,
            offset: node as usize,
        }
    }
    fn read(&self, env: Env, node: u32) -> Result<Value> {
        let mut value = Value::default();
        let mut err = ptr::null_mut();
        if unsafe { plsm_value_read(self.reference(node), &mut value, &mut err) } != 0 {
            return Err(take_error(env, err)?);
        }
        Ok(value)
    }
    fn descriptor(&self, env: Env, reference: ValueRef) -> Result<JsUnknown> {
        let value = self.read(env, reference.offset as u32)?;
        if value.kind < 6 {
            return self.scalar(env, value);
        }
        let mut object = env.create_object()?;
        {
            object.set_named_property(
                "kind",
                match value.kind {
                    6 => "bytes",
                    7 => "object",
                    _ => "array",
                },
            )?;
            object.set_named_property("node", reference.offset as u32)?;
            object.set_named_property("count", value.len as u32)?;
        }
        Ok(object.into_unknown())
    }
    fn scalar(&self, env: Env, value: Value) -> Result<JsUnknown> {
        Ok(match value.kind {
            1 => env.get_null()?.into_unknown(),
            2 => env.get_boolean(value.integer != 0)?.into_unknown(),
            3 if value.integer.unsigned_abs() > 9007199254740991 => {
                env.create_bigint_from_i64(value.integer)?.into_unknown()?
            }
            3 => env.create_double(value.integer as f64)?.into_unknown(),
            4 => env.create_double(value.real)?.into_unknown(),
            5 => env
                .create_string(
                    std::str::from_utf8(unsafe {
                        std::slice::from_raw_parts(value.bytes, value.len)
                    })
                    .map_err(|e| Error::from_reason(e.to_string()))?,
                )?
                .into_unknown(),
            _ => return Err(Error::from_reason("unsupported view scalar")),
        })
    }
    fn entries(&self, env: Env, node: u32) -> Result<Vec<Entry>> {
        let value = self.read(env, node)?;
        let empty = Entry {
            key: ptr::null(),
            key_len: 0,
            value: self.reference(0),
        };
        let mut entries = vec![empty; value.len];
        let mut count = 0;
        let mut err = ptr::null_mut();
        if unsafe {
            plsm_value_entries(
                self.reference(node),
                entries.as_mut_ptr(),
                entries.len(),
                &mut count,
                &mut err,
            )
        } != 0
        {
            return Err(take_error(env, err)?);
        }
        entries.truncate(count);
        Ok(entries)
    }
    fn property(&self, env: Env, entry: &Entry) -> Result<DataProperty> {
        let child = self.native_value(env, entry.value.offset as u32)?;
        let key =
            std::str::from_utf8(unsafe { std::slice::from_raw_parts(entry.key, entry.key_len) })
                .map_err(|error| Error::from_reason(error.to_string()))?;
        Ok(DataProperty {
            key: CString::new(key)?,
            value: child,
        })
    }
    fn native_value(&self, env: Env, node: u32) -> Result<JsUnknown> {
        let value = self.read(env, node)?;
        if value.kind < 6 {
            return self.scalar(env, value);
        }
        if value.kind == 6 {
            return Ok(env
                .create_buffer_with_data(
                    unsafe { std::slice::from_raw_parts(value.bytes, value.len) }.to_vec(),
                )?
                .into_unknown());
        }
        let entries = self.entries(env, node)?;
        let mut out = if value.kind == 8 {
            env.create_array_with_length(entries.len())?
        } else {
            env.create_object()?
        };
        if value.kind == 8 {
            for (index, entry) in entries.iter().enumerate() {
                out.set_element(
                    index as u32,
                    self.native_value(env, entry.value.offset as u32)?,
                )?;
            }
        } else if entries.len() == 1 {
            define_data_properties(env, &out, &[self.property(env, &entries[0])?])?;
        } else if !entries.is_empty() {
            // Keys and JS handles live through the single definition call.
            // Plain data properties need no native callback/finalizer metadata.
            let properties = entries
                .iter()
                .map(|entry| self.property(env, entry))
                .collect::<Result<Vec<_>>>()?;
            define_data_properties(env, &out, &properties)?;
        }
        Ok(out.into_unknown())
    }
}
#[napi]
impl NativeView {
    #[napi]
    pub fn root(&self, env: Env) -> Result<JsUnknown> {
        self.descriptor(env, unsafe { plsm_view_root(self.owner.0) })
    }
    #[napi(getter)]
    pub fn seq(&self) -> BigInt {
        BigInt::from(unsafe { plsm_view_seq(self.owner.0) })
    }
    #[napi(getter)]
    pub fn timestamp_ns(&self) -> BigInt {
        BigInt::from(unsafe { plsm_view_timestamp_ns(self.owner.0) })
    }
    #[napi]
    pub fn lookup(&self, env: Env, node: u32, key: String) -> Result<JsUnknown> {
        let mut out = ValueRef {
            owner: ptr::null(),
            offset: 0,
        };
        let mut err = ptr::null_mut();
        if unsafe {
            plsm_value_get(
                self.reference(node),
                key.as_ptr(),
                key.len(),
                &mut out,
                &mut err,
            )
        } != 0
        {
            return Err(take_error(env, err)?);
        }
        if out.owner.is_null() {
            Ok(env.get_undefined()?.into_unknown())
        } else {
            self.descriptor(env, out)
        }
    }
    #[napi]
    pub fn has(&self, env: Env, node: u32, key: String) -> Result<bool> {
        let mut out = ValueRef {
            owner: ptr::null(),
            offset: 0,
        };
        let mut err = ptr::null_mut();
        if unsafe {
            plsm_value_get(
                self.reference(node),
                key.as_ptr(),
                key.len(),
                &mut out,
                &mut err,
            )
        } != 0
        {
            return Err(take_error(env, err)?);
        }
        Ok(!out.owner.is_null())
    }
    #[napi]
    pub fn at(&self, env: Env, node: u32, index: u32) -> Result<JsUnknown> {
        let value = self.read(env, node)?;
        if value.kind == 6 {
            if index as usize >= value.len {
                return Err(Error::from_reason("byte index out of bounds"));
            }
            return Ok(env
                .create_uint32(unsafe { *value.bytes.add(index as usize) } as u32)?
                .into_unknown());
        }
        let mut out = ValueRef {
            owner: ptr::null(),
            offset: 0,
        };
        let mut err = ptr::null_mut();
        if unsafe { plsm_value_index(self.reference(node), index, &mut out, &mut err) } != 0 {
            return Err(take_error(env, err)?);
        }
        self.descriptor(env, out)
    }
    #[napi]
    pub fn keys(&self, env: Env, node: u32) -> Result<Vec<String>> {
        self.entries(env, node)?
            .iter()
            .map(|e| {
                std::str::from_utf8(unsafe { std::slice::from_raw_parts(e.key, e.key_len) })
                    .map(str::to_owned)
                    .map_err(|e| Error::from_reason(e.to_string()))
            })
            .collect()
    }
    #[napi]
    pub fn materialize(&self, env: Env, node: u32, parse: JsFunction) -> Result<JsUnknown> {
        if let Some(api) = native_json_api()
            && unsafe { (api.safe)(self.owner.0) }
        {
            let mut out = plsm_buf_t {
                data: ptr::null_mut(),
                len: 0,
            };
            let mut err = ptr::null_mut();
            if unsafe { (api.json)(self.reference(node), &mut out, &mut err) } != 0 {
                return Err(take_error(env, err)?);
            }
            let buffer = NativeBuf(out);
            // The C formatter guarantees UTF8. Node copies directly from its
            // allocation; the guard frees it after creating the JS string.
            let text =
                unsafe { env.create_string_from_c_char(buffer.0.data.cast(), buffer.0.len) }?;
            drop(buffer);
            return parse.call(None, &[text]);
        }
        self.native_value(env, node)
    }
    #[napi]
    pub fn json(&self, env: Env, node: u32) -> Result<String> {
        let mut out = plsm_buf_t {
            data: ptr::null_mut(),
            len: 0,
        };
        let mut err = ptr::null_mut();
        if unsafe { plsm_value_to_json(self.reference(node), &mut out, &mut err) } != 0 {
            return Err(take_error(env, err)?);
        }
        let bytes = NativeBuf(out).copy_bytes();
        String::from_utf8(bytes).map_err(|e| Error::from_reason(e.to_string()))
    }
    #[napi]
    pub fn byte_length(&self) -> u32 {
        let mut len = 0;
        unsafe { plsm_view_bytes(self.owner.0, &mut len) };
        len as u32
    }
}

#[napi(custom_finalize)]
pub struct NativeCursor {
    backend: NativeView,
    cursor: OwnedCursor,
    slots: [Entry; 16],
    available: usize,
    position: usize,
    node: u32,
    count: u32,
    index: u32,
    bytes: bool,
}
#[napi]
impl NativeCursor {
    #[napi]
    pub fn next_value(&mut self, env: Env) -> Result<JsUnknown> {
        if self.bytes {
            if self.index >= self.count {
                return Ok(env.get_undefined()?.into_unknown());
            }
            let index = self.index;
            self.index += 1;
            return self.backend.at(env, self.node, index);
        }
        if self.position == self.available {
            let mut err = ptr::null_mut();
            if unsafe {
                plsm_cursor_next(
                    self.cursor.0,
                    self.slots.as_mut_ptr(),
                    self.slots.len(),
                    &mut self.available,
                    &mut err,
                )
            } != 0
            {
                return Err(take_error(env, err)?);
            }
            self.position = 0;
            if self.available == 0 {
                return Ok(env.get_undefined()?.into_unknown());
            }
        }
        let reference = self.slots[self.position].value;
        self.position += 1;
        self.backend.descriptor(env, reference)
    }
}
#[napi]
impl NativeView {
    #[napi]
    pub fn cursor(&self, env: Env, node: u32) -> Result<NativeCursor> {
        let value = self.read(env, node)?;
        let bytes = value.kind == 6;
        let mut cursor = ptr::null_mut();
        if !bytes {
            let mut err = ptr::null_mut();
            if unsafe { plsm_value_iter(self.reference(node), &mut cursor, &mut err) } != 0 {
                return Err(take_error(env, err)?);
            }
        }
        let empty = Entry {
            key: ptr::null(),
            key_len: 0,
            value: self.reference(0),
        };
        Ok(NativeCursor {
            backend: NativeView {
                owner: self.owner.clone(),
            },
            cursor: OwnedCursor(cursor),
            slots: [empty; 16],
            available: 0,
            position: 0,
            node,
            count: value.len as u32,
            index: 0,
            bytes,
        })
    }
}

// Published owner references live only in environment-thread view/cursor
// wrappers. Charge once on publication and balance when their last Arc finalizes.
impl napi::bindgen_prelude::ObjectFinalize for NativeView {
    fn finalize(self, mut env: Env) -> Result<()> {
        if Arc::strong_count(&self.owner) == 1 {
            let mut len = 0;
            unsafe { plsm_view_bytes(self.owner.0, &mut len) };
            env.adjust_external_memory(-(len as i64))?;
        }
        Ok(())
    }
}
impl napi::bindgen_prelude::ObjectFinalize for NativeCursor {
    fn finalize(self, env: Env) -> Result<()> {
        drop(self.cursor);
        self.backend.finalize(env)
    }
}

// macOS addons permit dynamic lookup for Node symbols, which also permits an
// older libplasmite to load. Check the additive ABI before calling lazy stubs.
#[cfg(unix)]
pub fn available() -> bool {
    static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        [
            c"plsm_pool_get_view",
            c"plsm_view_root",
            c"plsm_view_release",
            c"plsm_view_seq",
            c"plsm_view_timestamp_ns",
            c"plsm_view_bytes",
            c"plsm_value_get",
            c"plsm_value_index",
            c"plsm_value_read",
            c"plsm_value_entries",
            c"plsm_value_to_json",
            c"plsm_value_iter",
            c"plsm_cursor_next",
            c"plsm_cursor_free",
        ]
        .iter()
        .all(|name| !unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) }.is_null())
    })
}
// Other platform loaders enforce imported symbols before addon initialization.
#[cfg(not(unix))]
pub fn available() -> bool {
    true
}

type NativeJsonSafe = unsafe extern "C" fn(*const Owner) -> bool;
type NativeJsonFormatter =
    unsafe extern "C" fn(ValueRef, *mut plsm_buf_t, *mut *mut plsm_error_t) -> i32;
struct NativeJsonApi {
    safe: NativeJsonSafe,
    json: NativeJsonFormatter,
}

// These additions stay optional so an older owned-view library can still use
// the direct walker. Resolve once; never search the dynamic loader per copy.
fn native_json_api() -> Option<&'static NativeJsonApi> {
    static API: std::sync::OnceLock<Option<NativeJsonApi>> = std::sync::OnceLock::new();
    API.get_or_init(|| {
        let safe = optional_symbol(c"plsm_view_json_native_safe");
        let json = optional_symbol(c"plsm_value_to_json_stored");
        if safe.is_null() || json.is_null() {
            return None;
        }
        Some(NativeJsonApi {
            safe: unsafe { std::mem::transmute::<*mut libc::c_void, NativeJsonSafe>(safe) },
            json: unsafe { std::mem::transmute::<*mut libc::c_void, NativeJsonFormatter>(json) },
        })
    })
    .as_ref()
}

#[cfg(unix)]
fn optional_symbol(name: &std::ffi::CStr) -> *mut libc::c_void {
    unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) }
}
#[cfg(windows)]
fn optional_symbol(name: &std::ffi::CStr) -> *mut libc::c_void {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleA(name: *const u8) -> *mut libc::c_void;
        fn GetProcAddress(module: *mut libc::c_void, name: *const u8) -> *mut libc::c_void;
    }
    unsafe {
        let module = GetModuleHandleA(c"plasmite.dll".as_ptr().cast());
        if module.is_null() {
            ptr::null_mut()
        } else {
            GetProcAddress(module, name.as_ptr().cast())
        }
    }
}
#[cfg(not(any(unix, windows)))]
fn optional_symbol(_name: &std::ffi::CStr) -> *mut libc::c_void {
    ptr::null_mut()
}

struct DataProperty {
    key: CString,
    value: JsUnknown,
}
impl DataProperty {
    fn raw(&self) -> napi::sys::napi_property_descriptor {
        napi::sys::napi_property_descriptor {
            utf8name: self.key.as_ptr(),
            name: ptr::null_mut(),
            method: None,
            getter: None,
            setter: None,
            value: unsafe { self.value.raw() },
            attributes: napi::sys::PropertyAttributes::writable
                | napi::sys::PropertyAttributes::enumerable
                | napi::sys::PropertyAttributes::configurable,
            data: ptr::null_mut(),
        }
    }
}
fn define_data_properties(env: Env, object: &JsObject, properties: &[DataProperty]) -> Result<()> {
    if let [property] = properties {
        napi::check_status!(unsafe {
            napi::sys::napi_define_properties(env.raw(), object.raw(), 1, &property.raw())
        })
    } else {
        let descriptors = properties.iter().map(DataProperty::raw).collect::<Vec<_>>();
        napi::check_status!(unsafe {
            napi::sys::napi_define_properties(
                env.raw(),
                object.raw(),
                descriptors.len(),
                descriptors.as_ptr(),
            )
        })
    }
}
