use super::Error;
use crate::{
    ffi,
    xpc::{Object, Ref, c_string},
};
use serde::de::{self, DeserializeSeed, IntoDeserializer, Visitor};
use std::ffi::CString;

pub(super) struct Deserializer<'de> {
    object: Ref<'de>,
}
impl<'de> Deserializer<'de> {
    pub fn new(object: Ref<'de>) -> Self {
        Self { object }
    }
    fn map(self) -> Result<Map<'de>, Error> {
        let count = self.object.dictionary_len()?;
        let keys = self.object.keys()?;
        Ok(Map {
            object: self.object,
            keys,
            index: 0,
            key: None,
            count,
        })
    }
}
impl<'de> de::Deserializer<'de> for Deserializer<'de> {
    type Error = Error;
    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let object = self.object;
        let ty = unsafe { ffi::xpc_get_type(object.as_raw()) };
        unsafe {
            match ty {
                t if t == &raw const ffi::_xpc_type_null => visitor.visit_unit(),
                t if t == &raw const ffi::_xpc_type_bool => {
                    visitor.visit_bool(ffi::xpc_bool_get_value(object.as_raw()))
                }
                t if t == &raw const ffi::_xpc_type_int64 => {
                    visitor.visit_i64(ffi::xpc_int64_get_value(object.as_raw()))
                }
                t if t == &raw const ffi::_xpc_type_uint64 => {
                    visitor.visit_u64(ffi::xpc_uint64_get_value(object.as_raw()))
                }
                t if t == &raw const ffi::_xpc_type_double => {
                    visitor.visit_f64(ffi::xpc_double_get_value(object.as_raw()))
                }
                t if t == &raw const ffi::_xpc_type_string => {
                    let text = object.string()?;
                    visitor.visit_borrowed_str(text)
                }
                t if t == &raw const ffi::_xpc_type_data => {
                    let data = object.data()?;
                    visitor.visit_borrowed_bytes(data)
                }
                t if t == &raw const ffi::_xpc_type_array => {
                    let count = object.array_len()?;
                    let mut access = Sequence {
                        object,
                        index: 0,
                        count,
                    };
                    let value = visitor.visit_seq(&mut access)?;
                    if access.index != count {
                        return Err(Error("unconsumed XPC array elements".into()));
                    }
                    Ok(value)
                }
                t if t == &raw const ffi::_xpc_type_dictionary => {
                    let mut access = self.map()?;
                    let value = visitor.visit_map(&mut access)?;
                    if access.index != access.count || access.key.is_some() {
                        return Err(Error("unconsumed XPC dictionary entries".into()));
                    }
                    Ok(value)
                }
                _ => Err(Error(
                    "unsupported XPC body type; file descriptors belong in the envelope".into(),
                )),
            }
        }
    }
    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if self.object.is(&raw const ffi::_xpc_type_null) {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }
    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _: &'static str,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        if self.object.is(&raw const ffi::_xpc_type_string) {
            let name = self.object.string()?;
            return visitor.visit_enum(Enum {
                name: name.into(),
                value: None,
            });
        }
        if self.object.dictionary_len()? != 1 {
            return Err(Error("expected enum name or single-key dictionary".into()));
        }
        let keys = self.object.keys()?;
        let name = keys.borrow().at(0)?.string()?;
        let value = self
            .object
            .get(&c_string(name)?)?
            .ok_or_else(|| Error("missing enum value".into()))?;
        visitor.visit_enum(Enum {
            name: name.into(),
            value: Some(Deserializer::new(value)),
        })
    }

    fn is_human_readable(&self) -> bool {
        false
    }
    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf unit unit_struct seq tuple tuple_struct map struct identifier ignored_any
    }
}

struct Sequence<'de> {
    object: Ref<'de>,
    index: usize,
    count: usize,
}
impl<'de> de::SeqAccess<'de> for Sequence<'de> {
    type Error = Error;
    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        if self.index == self.count {
            return Ok(None);
        }
        let value = self.object.at(self.index)?;
        self.index += 1;
        seed.deserialize(Deserializer::new(value)).map(Some)
    }
    fn size_hint(&self) -> Option<usize> {
        Some(self.count - self.index)
    }
}
struct Map<'de> {
    object: Ref<'de>,
    keys: Object,
    index: usize,
    count: usize,
    key: Option<CString>,
}
impl<'de> de::MapAccess<'de> for Map<'de> {
    type Error = Error;
    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        if self.key.is_some() {
            return Err(Error("XPC map value not consumed".into()));
        }
        if self.index == self.count {
            return Ok(None);
        }
        let key = self.keys.borrow().at(self.index)?.string()?;
        self.key = Some(c_string(key)?);
        self.index += 1;
        seed.deserialize(key.into_deserializer()).map(Some)
    }
    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        let key = self
            .key
            .take()
            .ok_or_else(|| Error("XPC map key not consumed".into()))?;
        let value = self
            .object
            .get(&key)?
            .ok_or_else(|| Error("missing XPC map value".into()))?;
        seed.deserialize(Deserializer::new(value))
    }
    fn size_hint(&self) -> Option<usize> {
        Some(self.count - self.index)
    }
}
struct Enum<'de> {
    name: String,
    value: Option<Deserializer<'de>>,
}
struct Variant<'de>(Option<Deserializer<'de>>);
impl<'de> de::EnumAccess<'de> for Enum<'de> {
    type Error = Error;
    type Variant = Variant<'de>;
    fn variant_seed<V: DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, Self::Variant), Error> {
        Ok((
            seed.deserialize(self.name.into_deserializer())?,
            Variant(self.value),
        ))
    }
}
impl<'de> de::VariantAccess<'de> for Variant<'de> {
    type Error = Error;
    fn unit_variant(self) -> Result<(), Error> {
        match self.0 {
            None => Ok(()),
            Some(value) => serde::Deserialize::deserialize(value),
        }
    }
    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Error> {
        seed.deserialize(self.0.ok_or_else(|| Error("missing enum payload".into()))?)
    }
    fn tuple_variant<V: Visitor<'de>>(self, _: usize, visitor: V) -> Result<V::Value, Error> {
        de::Deserializer::deserialize_seq(
            self.0.ok_or_else(|| Error("missing enum payload".into()))?,
            visitor,
        )
    }
    fn struct_variant<V: Visitor<'de>>(
        self,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        de::Deserializer::deserialize_map(
            self.0.ok_or_else(|| Error("missing enum payload".into()))?,
            visitor,
        )
    }
}
