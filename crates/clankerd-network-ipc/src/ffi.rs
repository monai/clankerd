use std::ffi::{c_char, c_void};

pub type Raw = *mut c_void;
#[repr(C)]
pub struct Type {
    _opaque: [u8; 0],
}

unsafe extern "C" {
    pub static _xpc_type_null: Type;
    pub static _xpc_type_bool: Type;
    pub static _xpc_type_int64: Type;
    pub static _xpc_type_uint64: Type;
    pub static _xpc_type_double: Type;
    pub static _xpc_type_string: Type;
    pub static _xpc_type_data: Type;
    pub static _xpc_type_array: Type;
    pub static _xpc_type_dictionary: Type;
    pub static _xpc_type_fd: Type;
    pub static _xpc_type_error: Type;

    pub fn xpc_get_type(object: Raw) -> *const Type;
    pub fn xpc_release(object: Raw);
    pub fn xpc_null_create() -> Raw;
    pub fn xpc_bool_create(value: bool) -> Raw;
    pub fn xpc_int64_create(value: i64) -> Raw;
    pub fn xpc_uint64_create(value: u64) -> Raw;
    pub fn xpc_double_create(value: f64) -> Raw;
    pub fn xpc_string_create(value: *const c_char) -> Raw;
    pub fn xpc_data_create(value: *const c_void, length: usize) -> Raw;
    pub fn xpc_array_create(values: *const Raw, count: usize) -> Raw;
    pub fn xpc_array_append_value(array: Raw, value: Raw);
    pub fn xpc_array_get_count(array: Raw) -> usize;
    pub fn xpc_array_get_value(array: Raw, index: usize) -> Raw;
    pub fn xpc_dictionary_create(
        keys: *const *const c_char,
        values: *const Raw,
        count: usize,
    ) -> Raw;
    pub fn xpc_dictionary_get_count(dictionary: Raw) -> usize;
    pub fn xpc_dictionary_set_value(dictionary: Raw, key: *const c_char, value: Raw);
    pub fn xpc_dictionary_get_value(dictionary: Raw, key: *const c_char) -> Raw;
    pub fn xpc_dictionary_set_fd(dictionary: Raw, key: *const c_char, fd: i32);
    pub fn xpc_dictionary_dup_fd(dictionary: Raw, key: *const c_char) -> i32;
    pub fn xpc_bool_get_value(value: Raw) -> bool;
    pub fn xpc_int64_get_value(value: Raw) -> i64;
    pub fn xpc_uint64_get_value(value: Raw) -> u64;
    pub fn xpc_double_get_value(value: Raw) -> f64;
    pub fn xpc_string_get_string_ptr(value: Raw) -> *const c_char;
    pub fn xpc_data_get_length(value: Raw) -> usize;
    pub fn xpc_data_get_bytes_ptr(value: Raw) -> *const c_void;
    pub fn xpc_connection_cancel(connection: Raw);
    pub fn xpc_connection_send_message_with_reply_sync(connection: Raw, message: Raw) -> Raw;

    // These APIs require Blocks; the C adapter supplies the closures.
    pub fn clankerd_xpc_connect() -> Raw;
    pub fn clankerd_xpc_dictionary_keys(dictionary: Raw) -> Raw;
    pub fn clankerd_xpc_serve(
        context: Raw,
        request: extern "C" fn(Raw, Raw, u32, i32, u64, Raw) -> bool,
        disconnected: extern "C" fn(u64, Raw),
        shutdown: extern "C" fn(Raw),
    ) -> !;
}
