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
    failed: bool,
    growth_retries: u32,
}
impl Builder {
    pub fn new(tags: &[&str]) -> Result<Self, Error> {
        let mut b = Self::new_without_data(tags)?;
        b.data = b.container(0, Some("data"), false)?;
        Ok(b)
    }
    pub(crate) fn new_without_data(tags: &[&str]) -> Result<Self, Error> {
        Self::new_with_sender(tags, None)
    }

    pub(crate) fn new_with_sender(tags: &[&str], sender: Option<&str>) -> Result<Self, Error> {
        let mut b = Self {
            words: vec![0; 64],
            len: 0,
            data: 0,
            json_native_safe: true,
            failed: false,
            growth_retries: 0,
        };
        b.put(usize::MAX, None, sys::ValueView::default())?;
        let meta = b.container(0, Some("meta"), false)?;
        let array = b.container(meta, Some("tags"), true)?;
        for tag in tags {
            b.string(array, None, tag)?;
        }
        if let Some(sender) = sender {
            b.string(meta, Some("sender"), sender)?;
        }
        Ok(b)
    }
    pub(crate) fn put(
        &mut self,
        parent: usize,
        key: Option<&str>,
        value: sys::ValueView,
    ) -> Result<usize, Error> {
        self.check_failed()?;
        // Record this while writing, without scanning the completed encoding.
        // Replacing an unsafe value leaves a conservative false result.
        self.json_native_safe &= value.kind != sys::LITE3_TYPE_BYTES
            && (value.kind != sys::LITE3_TYPE_I64
                || (-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&value.integer));
        // Most field names fit here. Keep the terminator required by the
        // pinned native setter without allocating a temporary CString.
        let mut key_stack;
        let key_heap;
        let key_pointer = match key {
            Some(key) if key.len() < 64 => {
                if key.as_bytes().contains(&0) {
                    return Err(Error::new(ErrorKind::Usage).with_message("key contains NUL"));
                }
                key_stack = [0_u8; 64];
                key_stack[..key.len()].copy_from_slice(key.as_bytes());
                key_stack.as_ptr().cast()
            }
            Some(key) => {
                key_heap = CString::new(key)
                    .map_err(|_| Error::new(ErrorKind::Usage).with_message("key contains NUL"))?;
                key_heap.as_ptr()
            }
            None => std::ptr::null(),
        };
        loop {
            let mut child = 0;
            let result = unsafe {
                sys::plasmite_lite3_put(
                    self.words.as_mut_ptr().cast(),
                    &mut self.len,
                    self.words.len() * 4,
                    parent,
                    key_pointer,
                    &value,
                    &mut child,
                )
            };
            if result == 0 {
                return Ok(child);
            }
            let errno = unsafe { sys::plasmite_lite3_last_errno() };
            if errno != libc::ENOBUFS || self.words.len() * 4 >= MAX_LITE3_BUF {
                self.failed = true;
                return Err(Error::new(ErrorKind::Usage)
                    .with_message("failed to build Lite3 value")
                    .with_source(std::io::Error::from_raw_os_error(errno)));
            }
            self.growth_retries += 1;
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
        self.check_failed()?;
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
            self.failed = true;
            return Err(Error::new(ErrorKind::Usage).with_message("failed to encode JSON view"));
        }
        self.json_native_safe = json_native_safe != 0;
        Ok(())
    }
    fn check_failed(&self) -> Result<(), Error> {
        if self.failed {
            return Err(Error::new(ErrorKind::Usage)
                .with_message("builder cannot continue after a failed write"));
        }
        Ok(())
    }
    pub(crate) fn data_is_empty(&self) -> Result<bool, Error> {
        self.check_failed()?;
        let bytes = unsafe { std::slice::from_raw_parts(self.words.as_ptr().cast(), self.len) };
        Ok(Lite3DocRef::new(bytes).count_at(self.data)? == 0)
    }
    pub fn finish(self) -> Result<MessageView, Error> {
        self.check_failed()?;
        let bytes = unsafe { std::slice::from_raw_parts(self.words.as_ptr().cast(), self.len) };
        Lite3DocRef::new(bytes).validate_view()?;
        self.seal()
    }
    /// Seal bytes whose structure, shape, depth, text and numbers the writer proved.
    ///
    /// # Safety
    /// Every write must use the checked native setters; callers must additionally
    /// establish the full message shape, UTF-8, finite numbers and common depth
    /// limit while writing. No mutable input pointer may survive publication.
    pub(crate) unsafe fn finish_proven(self) -> Result<MessageView, Error> {
        // Debug builds independently check the construction proof. Release
        // writers retain it rather than walk the finished message again.
        if cfg!(debug_assertions) {
            self.finish()
        } else {
            self.seal()
        }
    }
    /// Match the legacy encoder, which restarts from empty on buffer growth.
    /// The private writer grows in place, so failed setters leave increments
    /// in the high 24 bits of the root's generation/type word.
    ///
    /// # Safety
    /// The caller must satisfy the construction proof for finish_proven.
    pub(crate) unsafe fn finish_legacy(mut self) -> Result<MessageView, Error> {
        self.check_failed()?;
        self.words[0] = self.words[0].wrapping_sub(self.growth_retries << 8);
        unsafe { self.finish_proven() }
    }

    fn seal(self) -> Result<MessageView, Error> {
        self.check_failed()?;
        Ok(unsafe {
            MessageView::from_encoded_words(self.words, self.len, 0, 0, 0, self.json_native_safe)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_copy_cannot_continue_or_seal_even_if_its_error_is_ignored() {
        let mut data = serde_json::json!({});
        for _ in 0..31 {
            data = serde_json::json!({"next":data});
        }
        // This source fits depth zero, but not the enclosing message's depth.
        let parsed =
            crate::core::lite3::ParsedJson::parse_view(data.to_string().as_bytes()).unwrap();
        let mut builder = Builder::new(&[]).unwrap();
        assert!(
            builder
                .copy_parsed(unsafe { sys::plasmite_json_root(parsed.ptr.as_ptr()) })
                .is_err()
        );
        assert!(builder.string(builder.data, Some("after"), "no").is_err());
        assert!(unsafe { builder.finish_proven() }.is_err());
    }
}
