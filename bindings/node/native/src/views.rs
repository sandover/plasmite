use super::*;
use napi::{JsObject, JsUnknown, Property};

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
unsafe extern "C" {
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
    fn descriptor(&self, env: Env, reference: ValueRef) -> Result<JsObject> {
        let value = self.read(env, reference.offset as u32)?;
        let mut object = env.create_object()?;
        if value.kind >= 6 {
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
        } else {
            object.set_named_property("value", self.scalar(env, value)?)?;
        }
        Ok(object)
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
        for (index, entry) in entries.iter().enumerate() {
            let child = self.native_value(env, entry.value.offset as u32)?;
            if value.kind == 8 {
                out.set_element(index as u32, child)?;
            } else {
                let key = std::str::from_utf8(unsafe {
                    std::slice::from_raw_parts(entry.key, entry.key_len)
                })
                .map_err(|e| Error::from_reason(e.to_string()))?;
                out.define_properties(&[Property::new(key)?.with_value(&child)])?;
            }
        }
        Ok(out.into_unknown())
    }
}
#[napi]
impl NativeView {
    #[napi]
    pub fn root(&self, env: Env) -> Result<JsObject> {
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
    pub fn lookup(&self, env: Env, node: u32, key: String) -> Result<Option<JsObject>> {
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
            Ok(None)
        } else {
            self.descriptor(env, out).map(Some)
        }
    }
    #[napi]
    pub fn at(&self, env: Env, node: u32, index: u32) -> Result<JsObject> {
        let value = self.read(env, node)?;
        if value.kind == 6 {
            if index as usize >= value.len {
                return Err(Error::from_reason("byte index out of bounds"));
            }
            let mut out = env.create_object()?;
            out.set_named_property("value", unsafe { *value.bytes.add(index as usize) } as u32)?;
            return Ok(out);
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
    pub fn materialize(&self, env: Env, node: u32) -> Result<JsUnknown> {
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
    entries: Vec<Entry>,
    node: u32,
    count: u32,
    index: u32,
    bytes: bool,
}
#[napi]
impl NativeCursor {
    #[napi]
    pub fn next_value(&mut self, env: Env) -> Result<Option<JsObject>> {
        if self.index >= self.count {
            return Ok(None);
        }
        let index = self.index;
        self.index += 1;
        if self.bytes {
            self.backend.at(env, self.node, index).map(Some)
        } else {
            self.backend
                .descriptor(env, self.entries[index as usize].value)
                .map(Some)
        }
    }
}
#[napi]
impl NativeView {
    #[napi]
    pub fn cursor(&self, env: Env, node: u32) -> Result<NativeCursor> {
        let value = self.read(env, node)?;
        let bytes = value.kind == 6;
        Ok(NativeCursor {
            backend: NativeView {
                owner: self.owner.clone(),
            },
            entries: if bytes {
                Vec::new()
            } else {
                self.entries(env, node)?
            },
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
