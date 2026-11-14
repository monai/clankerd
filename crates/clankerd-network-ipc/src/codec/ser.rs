use super::{Error, to_object};
use crate::{
    ffi,
    xpc::{Object, c_string},
};
use serde::{Serialize, ser};
use std::ffi::CString;

pub(super) struct Serializer;

macro_rules! integer {
    ($method:ident, $ty:ty, $constructor:ident, $wide:ty) => {
        fn $method(self, value: $ty) -> Result<Object, Error> {
            unsafe { Object::from_owned(ffi::$constructor(value as $wide)) }
        }
    };
}

impl ser::Serializer for Serializer {
    type Ok = Object;
    type Error = Error;
    type SerializeSeq = Sequence;
    type SerializeTuple = Sequence;
    type SerializeTupleStruct = Sequence;
    type SerializeTupleVariant = Sequence;
    type SerializeMap = Dictionary;
    type SerializeStruct = Dictionary;
    type SerializeStructVariant = Dictionary;

    integer!(serialize_i8, i8, xpc_int64_create, i64);
    integer!(serialize_i16, i16, xpc_int64_create, i64);
    integer!(serialize_i32, i32, xpc_int64_create, i64);
    integer!(serialize_i64, i64, xpc_int64_create, i64);
    integer!(serialize_u8, u8, xpc_uint64_create, u64);
    integer!(serialize_u16, u16, xpc_uint64_create, u64);
    integer!(serialize_u32, u32, xpc_uint64_create, u64);
    integer!(serialize_u64, u64, xpc_uint64_create, u64);
    fn serialize_i128(self, value: i128) -> Result<Object, Error> {
        self.serialize_i64(
            i64::try_from(value).map_err(|_| Error("XPC integer exceeds i64".into()))?,
        )
    }
    fn serialize_u128(self, value: u128) -> Result<Object, Error> {
        self.serialize_u64(
            u64::try_from(value).map_err(|_| Error("XPC integer exceeds u64".into()))?,
        )
    }
    fn serialize_bool(self, value: bool) -> Result<Object, Error> {
        unsafe { Object::from_owned(ffi::xpc_bool_create(value)) }
    }
    fn serialize_f32(self, value: f32) -> Result<Object, Error> {
        self.serialize_f64(value.into())
    }
    fn serialize_f64(self, value: f64) -> Result<Object, Error> {
        unsafe { Object::from_owned(ffi::xpc_double_create(value)) }
    }
    fn serialize_char(self, value: char) -> Result<Object, Error> {
        self.serialize_str(&value.to_string())
    }
    fn serialize_str(self, value: &str) -> Result<Object, Error> {
        unsafe { Object::from_owned(ffi::xpc_string_create(c_string(value)?.as_ptr())) }
    }
    fn serialize_bytes(self, value: &[u8]) -> Result<Object, Error> {
        unsafe { Object::from_owned(ffi::xpc_data_create(value.as_ptr().cast(), value.len())) }
    }
    fn serialize_none(self) -> Result<Object, Error> {
        unsafe { Object::from_owned(ffi::xpc_null_create()) }
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<Object, Error> {
        let object = to_object(value)?;
        if object.borrow().is(&raw const ffi::_xpc_type_null) {
            return Err(Error(
                "Some cannot contain a value that encodes as XPC null".into(),
            ));
        }
        Ok(object)
    }
    fn serialize_unit(self) -> Result<Object, Error> {
        unsafe { Object::from_owned(ffi::xpc_null_create()) }
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<Object, Error> {
        self.serialize_unit()
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<Object, Error> {
        self.serialize_str(variant)
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<Object, Error> {
        to_object(value)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Object, Error> {
        tagged(Some(variant), to_object(value)?)
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Sequence, Error> {
        Sequence::new(None)
    }
    fn serialize_tuple(self, _: usize) -> Result<Sequence, Error> {
        Sequence::new(None)
    }
    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Sequence, Error> {
        Sequence::new(None)
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Sequence, Error> {
        Sequence::new(Some(variant))
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Dictionary, Error> {
        Dictionary::new(None)
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Dictionary, Error> {
        Dictionary::new(None)
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Dictionary, Error> {
        Dictionary::new(Some(variant))
    }
    fn is_human_readable(&self) -> bool {
        false
    }
}

fn tagged(variant: Option<&str>, value: Object) -> Result<Object, Error> {
    if let Some(name) = variant {
        let mut dictionary = Object::dictionary()?;
        dictionary.set(&c_string(name)?, value)?;
        Ok(dictionary)
    } else {
        Ok(value)
    }
}

pub(super) struct Sequence {
    values: Object,
    variant: Option<&'static str>,
}
impl Sequence {
    fn new(variant: Option<&'static str>) -> Result<Self, Error> {
        Ok(Self {
            values: Object::array()?,
            variant,
        })
    }
    fn push<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.values.append(to_object(value)?)
    }
    fn finish(self) -> Result<Object, Error> {
        tagged(self.variant, self.values)
    }
}
impl ser::SerializeSeq for Sequence {
    type Ok = Object;
    type Error = Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.push(value)
    }
    fn end(self) -> Result<Object, Error> {
        self.finish()
    }
}
impl ser::SerializeTuple for Sequence {
    type Ok = Object;
    type Error = Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.push(value)
    }
    fn end(self) -> Result<Object, Error> {
        self.finish()
    }
}
impl ser::SerializeTupleStruct for Sequence {
    type Ok = Object;
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.push(value)
    }
    fn end(self) -> Result<Object, Error> {
        self.finish()
    }
}
impl ser::SerializeTupleVariant for Sequence {
    type Ok = Object;
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.push(value)
    }
    fn end(self) -> Result<Object, Error> {
        self.finish()
    }
}

pub(super) struct Dictionary {
    values: Object,
    key: Option<CString>,
    variant: Option<&'static str>,
}
impl Dictionary {
    fn new(variant: Option<&'static str>) -> Result<Self, Error> {
        Ok(Self {
            values: Object::dictionary()?,
            key: None,
            variant,
        })
    }
    fn insert<T: Serialize + ?Sized>(
        &mut self,
        key: &std::ffi::CStr,
        value: &T,
    ) -> Result<(), Error> {
        if self.values.borrow().get(key)?.is_some() {
            return Err(Error("duplicate XPC dictionary key".into()));
        }
        self.values.set(key, to_object(value)?)
    }
    fn finish(self) -> Result<Object, Error> {
        if self.key.is_some() {
            return Err(Error("map key without value".into()));
        }
        tagged(self.variant, self.values)
    }
}
impl ser::SerializeMap for Dictionary {
    type Ok = Object;
    type Error = Error;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Error> {
        if self.key.is_some() {
            return Err(Error("map value missing".into()));
        }
        let key = to_object(key)?;
        self.key = Some(c_string(key.borrow().string()?)?);
        Ok(())
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        let key = self
            .key
            .take()
            .ok_or_else(|| Error("map key missing".into()))?;
        self.insert(&key, value)
    }
    fn end(self) -> Result<Object, Error> {
        self.finish()
    }
}
impl ser::SerializeStruct for Dictionary {
    type Ok = Object;
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.insert(&c_string(key)?, value)
    }
    fn end(self) -> Result<Object, Error> {
        self.finish()
    }
}
impl ser::SerializeStructVariant for Dictionary {
    type Ok = Object;
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.insert(&c_string(key)?, value)
    }
    fn end(self) -> Result<Object, Error> {
        self.finish()
    }
}
