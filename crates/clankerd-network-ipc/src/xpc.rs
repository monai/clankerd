use crate::{
    codec::Error,
    ffi::{self, Raw, Type},
};
use std::{
    ffi::{CStr, CString},
    marker::PhantomData,
    os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
    ptr::NonNull,
};

/// One owned retain. Borrowed dictionary/array children use Ref instead.
pub struct Object(NonNull<std::ffi::c_void>);
#[derive(Clone, Copy)]
pub struct Ref<'a> {
    raw: NonNull<std::ffi::c_void>,
    _owner: PhantomData<&'a Object>,
}
impl Object {
    /// # Safety
    /// `raw` must transfer one retain of a live XPC object, or be null.
    /// Mutable dictionary/array graphs must be exclusively owned.
    pub unsafe fn from_owned(raw: Raw) -> Result<Self, Error> {
        NonNull::new(raw)
            .map(Self)
            .ok_or_else(|| Error("XPC returned no object".into()))
    }
    pub fn borrow(&self) -> Ref<'_> {
        Ref {
            raw: self.0,
            _owner: PhantomData,
        }
    }
    pub fn as_raw(&self) -> Raw {
        self.0.as_ptr()
    }
    pub fn set(&mut self, key: &CStr, value: Object) -> Result<(), Error> {
        unsafe { self.borrow().set(key, &value) }
    }
    pub fn append(&mut self, value: Object) -> Result<(), Error> {
        self.borrow()
            .require(&raw const ffi::_xpc_type_array, "array")?;
        unsafe { ffi::xpc_array_append_value(self.as_raw(), value.as_raw()) }
        Ok(())
    }
    pub fn dictionary() -> Result<Self, Error> {
        unsafe {
            Self::from_owned(ffi::xpc_dictionary_create(
                std::ptr::null(),
                std::ptr::null(),
                0,
            ))
        }
    }
    pub fn array() -> Result<Self, Error> {
        unsafe { Self::from_owned(ffi::xpc_array_create(std::ptr::null(), 0)) }
    }
}
impl Drop for Object {
    fn drop(&mut self) {
        unsafe { ffi::xpc_release(self.as_raw()) }
    }
}
impl<'a> Ref<'a> {
    /// # Safety
    /// The object must remain live for 'a; this reference owns no retain.
    /// Its graph must not be mutated while borrowed descendants or data are in use.
    pub unsafe fn from_raw(raw: Raw) -> Result<Self, Error> {
        NonNull::new(raw)
            .map(|raw| Self {
                raw,
                _owner: PhantomData,
            })
            .ok_or_else(|| Error("missing XPC object".into()))
    }
    pub fn as_raw(self) -> Raw {
        self.raw.as_ptr()
    }
    pub fn is(self, ty: *const Type) -> bool {
        unsafe { ffi::xpc_get_type(self.as_raw()) == ty }
    }
    pub fn require(self, ty: *const Type, expected: &str) -> Result<(), Error> {
        if self.is(ty) {
            Ok(())
        } else {
            Err(Error(format!("expected XPC {expected}")))
        }
    }
    /// # Safety
    /// No descendant or data borrows may remain live across this mutation.
    pub unsafe fn set(self, key: &CStr, value: &Object) -> Result<(), Error> {
        self.require(&raw const ffi::_xpc_type_dictionary, "dictionary")?;
        unsafe { ffi::xpc_dictionary_set_value(self.as_raw(), key.as_ptr(), value.as_raw()) }
        Ok(())
    }
    /// # Safety
    /// No descendant or data borrows may remain live across this mutation.
    pub unsafe fn attach_fd(self, key: &CStr, fd: BorrowedFd<'_>) -> Result<(), Error> {
        self.require(&raw const ffi::_xpc_type_dictionary, "dictionary")?;
        unsafe { ffi::xpc_dictionary_set_fd(self.as_raw(), key.as_ptr(), fd.as_raw_fd()) }
        Ok(())
    }
    pub fn get(self, key: &CStr) -> Result<Option<Self>, Error> {
        self.require(&raw const ffi::_xpc_type_dictionary, "dictionary")?;
        let raw = unsafe { ffi::xpc_dictionary_get_value(self.as_raw(), key.as_ptr()) };
        if raw.is_null() {
            Ok(None)
        } else {
            unsafe { Self::from_raw(raw).map(Some) }
        }
    }
    pub fn array_len(self) -> Result<usize, Error> {
        self.require(&raw const ffi::_xpc_type_array, "array")?;
        Ok(unsafe { ffi::xpc_array_get_count(self.as_raw()) })
    }
    pub fn at(self, index: usize) -> Result<Self, Error> {
        if index >= self.array_len()? {
            return Err(Error("XPC array index out of bounds".into()));
        }
        unsafe { Self::from_raw(ffi::xpc_array_get_value(self.as_raw(), index)) }
    }
    pub fn dictionary_len(self) -> Result<usize, Error> {
        self.require(&raw const ffi::_xpc_type_dictionary, "dictionary")?;
        Ok(unsafe { ffi::xpc_dictionary_get_count(self.as_raw()) })
    }
    pub fn keys(self) -> Result<Object, Error> {
        self.require(&raw const ffi::_xpc_type_dictionary, "dictionary")?;
        unsafe { Object::from_owned(ffi::clankerd_xpc_dictionary_keys(self.as_raw())) }
    }
    pub fn string(self) -> Result<&'a str, Error> {
        self.require(&raw const ffi::_xpc_type_string, "string")?;
        unsafe { CStr::from_ptr(ffi::xpc_string_get_string_ptr(self.as_raw())) }
            .to_str()
            .map_err(|e| Error(e.to_string()))
    }
    pub fn data(self) -> Result<&'a [u8], Error> {
        self.require(&raw const ffi::_xpc_type_data, "data")?;
        let length = unsafe { ffi::xpc_data_get_length(self.as_raw()) };
        if length == 0 {
            return Ok(&[]);
        }
        Ok(unsafe {
            std::slice::from_raw_parts(ffi::xpc_data_get_bytes_ptr(self.as_raw()).cast(), length)
        })
    }
    pub fn receive_fd(self, key: &CStr) -> std::io::Result<Option<OwnedFd>> {
        let Some(value) = self.get(key)? else {
            return Ok(None);
        };
        value.require(&raw const ffi::_xpc_type_fd, "file descriptor")?;
        let fd = unsafe { ffi::xpc_dictionary_dup_fd(self.as_raw(), key.as_ptr()) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Some(fd))
    }
}

pub fn c_string(value: &str) -> Result<CString, Error> {
    CString::new(value).map_err(|_| Error("XPC strings cannot contain NUL".into()))
}
