//! Serde for the network control protocol, directly over native XPC objects.
//! Dictionaries require string keys; strings cannot contain NUL; integers must fit i64/u64.
//! Options use null for None and the native value for Some; null-valued Some is rejected.
//! File descriptors belong to the transport envelope. No codec-specific size limits apply.
mod de;
mod ser;
#[cfg(test)]
mod tests;

use crate::xpc::{Object, Ref};
use std::fmt;

#[derive(Debug)]
pub struct Error(pub(super) String);
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for Error {}
impl serde::ser::Error for Error {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Self(message.to_string())
    }
}
impl serde::de::Error for Error {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Self(message.to_string())
    }
}
impl From<Error> for std::io::Error {
    fn from(error: Error) -> Self {
        Self::new(std::io::ErrorKind::InvalidData, error)
    }
}
pub fn to_object<T: serde::Serialize + ?Sized>(value: &T) -> Result<Object, Error> {
    value.serialize(ser::Serializer)
}
pub fn from_object<T: serde::de::DeserializeOwned>(object: Ref<'_>) -> Result<T, Error> {
    T::deserialize(de::Deserializer::new(object))
}
