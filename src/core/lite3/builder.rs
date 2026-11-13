//! Grow one aligned encoding without reevaluating the caller's input.
use super::{Lite3DocRef, MAX_LITE3_BUF, sys};
use crate::core::error::{Error, ErrorKind};
use crate::view::MessageView;
use std::ffi::CString;

pub(crate) struct Builder {
    words: Vec<u32>,
    len: usize,
    pub data: usize,
    json_native_safe: bool,
}
impl Builder {
    pub fn new(tags: &[&str]) -> Result<Self, Error> {
        let mut b = Self::new_without_data(tags)?;
        b.data = b.container(0, Some("data"), false)?;
        Ok(b)
    }
    pub(crate) fn new_without_data(tags: &[&str]) -> Result<Self, Error> {
        let mut b = Self {
            words: vec![0; 64],
            len: 0,
            data: 0,
            json_native_safe: true,
        };
        b.put(usize::MAX, None, sys::ValueView::default())?;
        let meta = b.container(0, Some("meta"), false)?;
        let array = b.container(meta, Some("tags"), true)?;
        for tag in tags {
            b.string(array, None, tag)?;
        }
        Ok(b)
    }
    pub(crate) fn put(
        &mut self,
        parent: usize,
        key: Option<&str>,
        value: sys::ValueView,
    ) -> Result<usize, Error> {
        // Record this while writing, without scanning the completed encoding.
        // Replacing an unsafe value leaves a conservative false result.
        self.json_native_safe &= value.kind != sys::LITE3_TYPE_BYTES
            && (value.kind != sys::LITE3_TYPE_I64
                || (-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&value.integer));
        let key = key
            .map(CString::new)
            .transpose()
            .map_err(|_| Error::new(ErrorKind::Usage).with_message("key contains NUL"))?;
        loop {
            let mut child = 0;
            let result = unsafe {
                sys::plasmite_lite3_put(
                    self.words.as_mut_ptr().cast(),
                    &mut self.len,
                    self.words.len() * 4,
                    parent,
                    key.as_ref().map_or(std::ptr::null(), |k| k.as_ptr()),
                    &value,
                    &mut child,
                )
            };
            if result == 0 {
                return Ok(child);
            }
            let errno = unsafe { sys::plasmite_lite3_last_errno() };
            if errno != libc::ENOBUFS || self.words.len() * 4 >= MAX_LITE3_BUF {
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("failed to build Lite3 value")
                    .with_source(std::io::Error::from_raw_os_error(errno)));
            }
            self.words
                .resize((self.words.len() * 2).min(MAX_LITE3_BUF / 4), 0);
        }
    }
    pub fn container(
        &mut self,
        parent: usize,
        key: Option<&str>,
        array: bool,
    ) -> Result<usize, Error> {
        self.put(
            parent,
            key,
            sys::ValueView {
                kind: if array {
                    sys::LITE3_TYPE_ARRAY
                } else {
                    sys::LITE3_TYPE_OBJECT
                },
                ..Default::default()
            },
        )
    }
    pub fn string(&mut self, parent: usize, key: Option<&str>, text: &str) -> Result<(), Error> {
        self.put(
            parent,
            key,
            sys::ValueView {
                kind: sys::LITE3_TYPE_STRING,
                text: text.as_ptr().cast(),
                text_len: text.len(),
                ..Default::default()
            },
        )
        .map(|_| ())
    }
    pub fn copy_parsed(&mut self, root: usize) -> Result<(), Error> {
        unsafe extern "C" {
            fn plasmite_json_copy_growing(
                root: usize,
                parent: usize,
                buf: *mut *mut u8,
                len: *mut usize,
                capacity: *mut usize,
                state: *mut std::ffi::c_void,
                grow: unsafe extern "C" fn(*mut std::ffi::c_void, *mut *mut u8, *mut usize) -> i32,
                depth: u32,
                json_native_safe: *mut i32,
            ) -> i32;
        }
        unsafe extern "C" fn grow(
            state: *mut std::ffi::c_void,
            buf: *mut *mut u8,
            capacity: *mut usize,
        ) -> i32 {
            let words = unsafe { &mut *state.cast::<Vec<u32>>() };
            if words.len() * 4 >= MAX_LITE3_BUF {
                return -1;
            }
            words.resize((words.len() * 2).min(MAX_LITE3_BUF / 4), 0);
            unsafe {
                *buf = words.as_mut_ptr().cast();
                *capacity = words.len() * 4;
            }
            0
        }
        let mut buf = self.words.as_mut_ptr().cast();
        let mut capacity = self.words.len() * 4;
        // The parsed owner outlives this call. C retains no pointers; growth
        // mutates only the word allocation, with len stored separately.
        let mut json_native_safe = i32::from(self.json_native_safe);
        let ret = unsafe {
            plasmite_json_copy_growing(
                root,
                self.data,
                &mut buf,
                &mut self.len,
                &mut capacity,
                (&mut self.words as *mut Vec<u32>).cast(),
                grow,
                1,
                &mut json_native_safe,
            )
        };
        if ret < 0 {
            return Err(Error::new(ErrorKind::Usage).with_message("failed to encode JSON view"));
        }
        self.json_native_safe = json_native_safe != 0;
        Ok(())
    }
    pub(crate) fn data_is_empty(&self) -> Result<bool, Error> {
        let bytes = unsafe { std::slice::from_raw_parts(self.words.as_ptr().cast(), self.len) };
        Ok(Lite3DocRef::new(bytes).count_at(self.data)? == 0)
    }
    pub fn finish(self) -> Result<MessageView, Error> {
        let bytes = unsafe { std::slice::from_raw_parts(self.words.as_ptr().cast(), self.len) };
        Lite3DocRef::new(bytes).validate_view()?;
        // The setters overwrite duplicate keys. Full validation establishes depth,
        // finite values, shape and text validity; the aligned allocation stays owned.
        Ok(unsafe {
            MessageView::from_encoded_words(self.words, self.len, 0, 0, 0, self.json_native_safe)
        })
    }
}
