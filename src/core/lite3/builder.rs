//! Grow one aligned encoding without reevaluating the caller's input.
use super::{Lite3DocRef, MAX_LITE3_BUF, sys};
use crate::core::error::{Error, ErrorKind};
use crate::view::MessageView;
use std::ffi::CString;

pub(crate) struct Builder {
    words: Vec<u32>,
    len: usize,
    pub data: usize,
}
impl Builder {
    pub fn new(tags: &[&str]) -> Result<Self, Error> {
        let mut b = Self {
            words: vec![0; 64],
            len: 0,
            data: 0,
        };
        b.put(usize::MAX, None, sys::ValueView::default())?;
        let meta = b.container(0, Some("meta"), false)?;
        let array = b.container(meta, Some("tags"), true)?;
        for tag in tags {
            b.string(array, None, tag)?;
        }
        b.data = b.container(0, Some("data"), false)?;
        Ok(b)
    }
    fn put(
        &mut self,
        parent: usize,
        key: Option<&str>,
        value: sys::ValueView,
    ) -> Result<usize, Error> {
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
    pub fn scalar(
        &mut self,
        parent: usize,
        key: Option<&str>,
        value: sys::ValueView,
    ) -> Result<(), Error> {
        self.put(parent, key, value).map(|_| ())
    }
    pub fn finish(self) -> Result<MessageView, Error> {
        let bytes = unsafe { std::slice::from_raw_parts(self.words.as_ptr().cast(), self.len) };
        Lite3DocRef::new(bytes).validate_view()?;
        // The setters overwrite duplicate keys. Full validation establishes depth,
        // finite values, shape and text validity; the aligned allocation stays owned.
        Ok(unsafe { MessageView::from_encoded_words(self.words, self.len, 0, 0, 0) })
    }
}
