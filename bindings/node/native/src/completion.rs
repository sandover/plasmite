//! One lazy completion channel per stream. Each read creates a Promise, while
//! the native async handle and environment cleanup hook survive between reads.
use napi::bindgen_prelude::ToNapiValue;
use napi::{Env, JsObject, NapiRaw, NapiValue, Result, sys};
use std::ffi::c_void;
use std::ptr;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

type Resolver = Box<dyn FnOnce(Env) -> Result<sys::napi_value> + Send>;
type Cancel = Box<dyn FnOnce() + Send>;
struct Context {
    cancel: Option<Cancel>,
    function: usize,
}
struct Pending {
    deferred: usize,
    resolver: Resolver,
}

pub(super) struct Channel {
    function: usize,
    closed: AtomicBool,
}
pub(super) struct Completion {
    channel: Arc<Channel>,
    deferred: usize,
}

unsafe extern "C" fn cancel(data: *mut c_void) {
    let context = unsafe { &mut *data.cast::<Context>() };
    if let Some(cancel) = context.cancel.take() {
        cancel();
    }
}
unsafe extern "C" fn finalize(env: sys::napi_env, data: *mut c_void, _: *mut c_void) {
    let context = unsafe { Box::<Context>::from_raw(data.cast()) };
    if context.cancel.is_some() && !env.is_null() {
        unsafe { sys::napi_remove_env_cleanup_hook(env, Some(cancel), data) };
    }
}

impl Channel {
    pub(super) fn new(env: Env, on_cleanup: impl FnOnce() + Send + 'static) -> Result<Arc<Self>> {
        let name = env.create_string("plasmite:stream")?;
        let context = Box::into_raw(Box::new(Context {
            cancel: Some(Box::new(on_cleanup)),
            function: 0,
        }))
        .cast();
        let status = unsafe { sys::napi_add_env_cleanup_hook(env.raw(), Some(cancel), context) };
        if status != sys::Status::napi_ok {
            drop(unsafe { Box::<Context>::from_raw(context.cast()) });
            napi::check_status!(status)?;
        }
        let mut function = ptr::null_mut();
        let status = unsafe {
            sys::napi_create_threadsafe_function(
                env.raw(),
                ptr::null_mut(),
                ptr::null_mut(),
                name.raw(),
                1,
                1,
                context,
                Some(finalize),
                context,
                Some(complete),
                &mut function,
            )
        };
        if status != sys::Status::napi_ok {
            unsafe { sys::napi_remove_env_cleanup_hook(env.raw(), Some(cancel), context) };
            drop(unsafe { Box::<Context>::from_raw(context.cast()) });
            napi::check_status!(status)?;
        }
        unsafe {
            (*context.cast::<Context>()).function = function as usize;
        }
        napi::check_status!(unsafe { sys::napi_unref_threadsafe_function(env.raw(), function) })?;
        Ok(Arc::new(Self {
            function: function as usize,
            closed: AtomicBool::new(false),
        }))
    }
    pub(super) fn promise(self: &Arc<Self>, env: Env) -> Result<(Completion, JsObject)> {
        let mut promise = ptr::null_mut();
        let mut deferred = ptr::null_mut();
        napi::check_status!(unsafe {
            sys::napi_create_promise(env.raw(), &mut deferred, &mut promise)
        })?;
        napi::check_status!(unsafe {
            sys::napi_ref_threadsafe_function(
                env.raw(),
                self.function as sys::napi_threadsafe_function,
            )
        })?;
        Ok((
            Completion {
                channel: Arc::clone(self),
                deferred: deferred as usize,
            },
            unsafe { JsObject::from_raw_unchecked(env.raw(), promise) },
        ))
    }
}
impl Drop for Channel {
    fn drop(&mut self) {
        // napi_closing already removes the producer's participation; another
        // release could access a destroyed handle during Worker termination.
        if !self.closed.load(Ordering::Acquire) {
            unsafe {
                sys::napi_release_threadsafe_function(
                    self.function as sys::napi_threadsafe_function,
                    sys::ThreadsafeFunctionReleaseMode::release,
                )
            };
        }
    }
}
impl Completion {
    pub(super) fn resolve<T: ToNapiValue>(
        self,
        resolver: impl FnOnce(Env) -> Result<T> + Send + 'static,
    ) {
        let data = Box::into_raw(Box::new(Pending {
            deferred: self.deferred,
            resolver: Box::new(move |env| {
                let value = resolver(env)?;
                unsafe { T::to_napi_value(env.raw(), value) }
            }),
        }))
        .cast();
        let status = unsafe {
            sys::napi_call_threadsafe_function(
                self.channel.function as sys::napi_threadsafe_function,
                data,
                sys::ThreadsafeFunctionCallMode::nonblocking,
            )
        };
        if status != sys::Status::napi_ok {
            drop(unsafe { Box::<Pending>::from_raw(data.cast()) });
        }
        if status == sys::Status::napi_closing {
            self.channel.closed.store(true, Ordering::Release);
        }
    }
}
unsafe extern "C" fn complete(
    env: sys::napi_env,
    _: sys::napi_value,
    context: *mut c_void,
    data: *mut c_void,
) {
    let pending = unsafe { Box::<Pending>::from_raw(data.cast()) };
    if env.is_null() {
        return;
    }
    let context = unsafe { &*context.cast::<Context>() };
    // The Promise's continuation can request the next read after this callback.
    unsafe {
        sys::napi_unref_threadsafe_function(env, context.function as sys::napi_threadsafe_function)
    };
    let deferred = pending.deferred as sys::napi_deferred;
    match (pending.resolver)(unsafe { Env::from_raw(env) }) {
        Ok(value) => {
            unsafe { sys::napi_resolve_deferred(env, deferred, value) };
        }
        Err(error) => {
            let value = unsafe { napi::JsError::from(error).into_value(env) };
            unsafe { sys::napi_reject_deferred(env, deferred, value) };
        }
    }
}
