//! Optional serde integration (feature `serde`): convert Rust data
//! structures ⇄ `java.util.HashMap` / `java.util.ArrayList` value trees.
//!
//! # Why a wrapper type
//!
//! [`ToJava`] is already implemented for the scalar types (`String`, the
//! primitives, …), so a blanket `impl<T: serde::Serialize> ToJava for T`
//! would overlap with them. The serde conversions are therefore opt-in,
//! behind [`JavaMap<T>`](crate::serde::JavaMap): pass `JavaMap(&my_struct)` as a method argument to
//! build a `java.util.HashMap<String, Object>` on the fly, or annotate
//! `JavaMap<T>` as a return type to read a Java `Map` back into `T`. The
//! free function [`from_object`](crate::serde::from_object) deserializes *any* Java value — Map, List
//! or scalar — into `T` directly, without the wrapper.
//!
//! # Type mapping
//!
//! | Rust (serde)                          | Java value                     |
//! |---------------------------------------|--------------------------------|
//! | `String`, `&str`                      | `java.lang.String`             |
//! | `bool`                                | `java.lang.Boolean`            |
//! | `i8`                                  | `java.lang.Byte` (the crate's `i8` ⇄ `byte` convention) |
//! | `i16`                                 | `java.lang.Short`              |
//! | `i32`                                 | `java.lang.Integer`            |
//! | `i64`                                 | `java.lang.Long`               |
//! | `u8`                                  | `java.lang.Byte` (the crate's `u8` ⇄ `byte` convention) |
//! | `f32`                                 | `java.lang.Float`              |
//! | `f64`                                 | `java.lang.Double`             |
//! | `char`                                | `java.lang.Character` (code points above `U+FFFF` error) |
//! | `Option<T>` — `None`                  | Java `null`                    |
//! | `Vec<T>`, arrays, tuples              | `java.util.ArrayList`          |
//! | structs, `HashMap<K, V>`              | nested `java.util.HashMap`     |
//! | `()`, unit structs                    | Java `null` (and *any* Java value reads back as `()`) |
//!
//! The wrapper classes mirror the crate's [`ToJava`] primitive mapping
//! exactly (`i8` → `byte` → `Byte`, `i16` → `short` → `Short`, `f32` →
//! `float` → `Float`, …), so a boxed value unboxes into a bean setter of
//! the matching primitive type under the crate's no-widening rule.
//!
//! Serialization **errors** on: `u16`/`u32`/`u64`, `usize`/`isize`,
//! `i128`/`u128`, and enums. The no-unsigned-integers rule carries over from
//! the rest of the crate (a `usize` length, say, must be cast to `i64`
//! first). Deserialization of unsigned types *works* when the Java value fits
//! (serde range-checks the visit) — `u8` follows the crate's `u8` ⇄ `byte`
//! bit-pattern convention: a `Byte` value reads back with the unsigned
//! interpretation (`byte` `-1` → `u8` `255`), so every `u8` round-trips.
//! `java.util.Date`-like objects are unsupported on both sides.
//!
//! Java `Map` keys are read back as Java `String`s only: a struct's fields
//! are matched by key, and a non-`String` key is a deserialization error.
//! `HashMap<K, V>` targets likewise require `K` to deserialize from a Java
//! `String` key (write `HashMap<String, T>` unless you serialize the keys
//! yourself as strings).
//!
//! # Bean reflection
//!
//! Reading and writing plain Java beans through getter/setter reflection is
//! a sibling feature in the same `serde` feature: see [`crate::bean`] —
//! `JavaBean` serializes a Rust struct into a Java object (`new` plus one
//! setter call per field) and deserializes it back getter by getter. This
//! module's **value** deserializer still rejects an arbitrary Java object
//! that is not a `Map`, `List`, `String`, boxed primitive or `null`, with an
//! error naming its class — the one exception is a `JavaBean<T>` struct
//! field, whose `deserialize_newtype_struct("JavaBean", …)` marker is
//! intercepted and read as a bean (nested object → getters, marker map →
//! plain struct value; see the [bean module docs](crate::bean)).
//!
//! # Errors
//!
//! Every serde conversion failure surfaces as [`JavaError::Serde`] with an
//! actionable dynamic message — the offending Java value's class where
//! relevant, or serde's own `Error::custom` text (missing fields, type
//! mismatches, unknown keys, …). [`JavaError::InvalidArgument`] is **not**
//! produced here: its `'static` message cannot name a runtime Java class,
//! and the serde errors are inherently dynamic.

use std::fmt;
use std::marker::PhantomData;

use jni::objects::{JObject as JniObject, JString};
use jni::signature::MethodSignature;
use jni::strings::{JNIStr, JNIString};
use jni::{Env, JValue, JValueOwned};


use crate::call;
use crate::convert::{FromJava, JavaArg, ToJava};
use crate::error::{JavaError, JavaResult};
use crate::handles::JObject;

// ---------------------------------------------------------------------------
// The public wrapper
// ---------------------------------------------------------------------------

/// Opt-in serde conversion of a Rust value into a Java `HashMap` (and back).
///
/// Wraps a serde-serializable value so it can travel through the
/// [`ToJava`]/[`FromJava`] machinery without overlapping the existing scalar
/// implementations (a blanket `impl<T: Serialize> ToJava for T` would
/// collide with `String`, the primitives, …).
///
/// * **Argument side** — pass `JavaMap(&value)` wherever a `java.util.Map`
///   parameter is expected; the value is serialized into a fresh
///   `java.util.HashMap<String, Object>` (see the [module docs](self) for
///   the type mapping). [`ToJava`] is implemented with the JNI fragment
///   `Ljava/util/Map;`, so `map.call_void("putAll", (JavaMap(&v),))` fills
///   an existing `HashMap` from a Rust struct, and any method taking a
///   `Map` parameter accepts it.
/// * **Return side** — annotate a call result as `JavaMap<T>` to read a
///   Java `Map` back into `T` (struct fields are matched by key in any
///   order; unknown keys and missing fields error). `T` only needs
///   `DeserializeOwned`.
///
/// See [`from_object`] for reading a Java value directly, without the
/// wrapper.
pub struct JavaMap<T>(pub T);

impl<T: serde::Serialize> ToJava for JavaMap<T> {
    fn to_java<'env>(&self, env: &mut Env<'env>) -> JavaResult<Vec<JavaArg<'env>>> {
        let map = self
            .0
            .serialize(JavaSerializer { env })
            .map_err(SerdeError::into_java_error)?;
        Ok(vec![JavaArg::Object(map)])
    }
    fn java_args(&self) -> String {
        String::from("Ljava/util/Map;")
    }
}

impl<T: serde::de::DeserializeOwned> FromJava for JavaMap<T> {
    fn from_java<'env>(env: &mut Env<'env>, value: JValueOwned<'env>) -> JavaResult<Self> {
        let de = JavaDeserializer { env, value };
        Ok(JavaMap(
            T::deserialize(de).map_err(SerdeError::into_java_error)?,
        ))
    }
    fn java_return_type() -> String {
        String::from("Ljava/util/Map;")
    }
}

/// Deserialize a Java value into `T`, without the [`JavaMap`] wrapper.
///
/// `obj` may be a `java.util.Map` (→ structs / `HashMap<K, V>`), a
/// `java.util.List` (→ sequences), a `String`, a boxed primitive, or `null`
/// (→ `Option::None`, or `()` — see the [module docs](self) for the full
/// mapping). The Java value's *runtime* type selects the shape, so the
/// annotation must match it: reading a `HashMap` into a struct works, while
/// reading one into an `i32` is a serde type error.
///
/// The caller provides an attached JNI environment — e.g. inside a
/// `with_env` helper over `jni::JavaVM::singleton()` (see the integration
/// tests).
pub fn from_object<T: serde::de::DeserializeOwned>(
    env: &mut Env<'_>,
    obj: &JObject,
) -> JavaResult<T> {
    let local = env.new_local_ref(&*obj.global)?;
    let de = JavaDeserializer {
        env,
        value: JValueOwned::Object(local),
    };
    T::deserialize(de).map_err(SerdeError::into_java_error)
}

// ---------------------------------------------------------------------------
// Error plumbing
// ---------------------------------------------------------------------------

/// The error type of the serde `Serializer`/`Deserializer`: wraps a
/// [`JavaError`] so `?` works on JNI/`JavaError`-producing calls inside the
/// conversion code, and implements `serde::ser::Error`/`serde::de::Error`
/// (whose `custom` messages become [`JavaError::Serde`]).
///
/// `pub(crate)` because the bean mapping (`crate::bean`) uses the same error
/// type for its own serializer/deserializer wrappers, so their messages stay
/// uniform with this module's.
#[derive(Debug)]
pub(crate) struct SerdeError(JavaError);

impl SerdeError {
    /// Unwrap the inner [`JavaError`] (the reverse of `From<JavaError>`).
    pub(crate) fn into_java_error(self) -> JavaError {
        self.0
    }
    /// A `serde::ser::Error::custom`-style error (dynamic message).
    pub(crate) fn ser_custom(msg: impl fmt::Display) -> Self {
        SerdeError(JavaError::Serde(msg.to_string()))
    }
    /// A `serde::de::Error::custom`-style error (dynamic message).
    pub(crate) fn de_custom(msg: impl fmt::Display) -> Self {
        SerdeError(JavaError::Serde(msg.to_string()))
    }
}

impl fmt::Display for SerdeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for SerdeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

impl From<JavaError> for SerdeError {
    fn from(e: JavaError) -> Self {
        SerdeError(e)
    }
}

impl From<jni::errors::Error> for SerdeError {
    fn from(e: jni::errors::Error) -> Self {
        SerdeError(JavaError::from(e))
    }
}

impl serde::ser::Error for SerdeError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        SerdeError(JavaError::Serde(msg.to_string()))
    }
}

impl serde::de::Error for SerdeError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        SerdeError(JavaError::Serde(msg.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Shared JNI helpers (all JDK calls, exact signatures, exception-checked)
// ---------------------------------------------------------------------------

/// Box a primitive into its wrapper class via `Wrapper.valueOf(...)`.
fn value_of<'env>(
    env: &mut Env<'env>,
    class: &str,
    sig: MethodSignature<'static, 'static>,
    arg: JValue<'_>,
) -> JavaResult<JniObject<'env>> {
    let cls = call::find_class(env, JNIString::from(class))?;
    let r = call::with_check(env, |env| {
        env.call_static_method(cls, jni::jni_str!("valueOf"), sig, &[arg])
    })?;
    match r {
        JValueOwned::Object(o) => Ok(o),
        _ => Err(JavaError::InvalidArgument(
            "internal error: a wrapper valueOf call did not return an object",
        )),
    }
}

/// `map.put(key, value)` — exact erased signature, return value discarded.
fn map_put<'env>(
    env: &mut Env<'env>,
    map: &JniObject<'env>,
    key: &JniObject<'env>,
    value: &JniObject<'env>,
) -> JavaResult<()> {
    call::with_check(env, |env| {
        env.call_method(
            map,
            jni::jni_str!("put"),
            jni::jni_sig!("(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;"),
            &[JValue::Object(key), JValue::Object(value)],
        )
        .map(|_| ())
    })
}

/// `map.containsKey(key)` — exact erased signature.
fn map_contains_key<'env>(
    env: &mut Env<'env>,
    map: &JniObject<'env>,
    key: &str,
) -> JavaResult<bool> {
    let k = call::with_check(env, |env| env.new_string(key))?;
    let r = call::with_check(env, |env| {
        env.call_method(
            map,
            jni::jni_str!("containsKey"),
            jni::jni_sig!("(Ljava/lang/Object;)Z"),
            &[JValue::Object(&k)],
        )
    })?;
    match r {
        JValueOwned::Bool(b) => Ok(b),
        _ => Err(JavaError::InvalidArgument(
            "internal error: Map.containsKey() did not return a boolean",
        )),
    }
}

/// `map.get(key)` — exact erased signature, returns the entry's value.
fn map_get<'env>(
    env: &mut Env<'env>,
    map: &JniObject<'env>,
    key: &str,
) -> JavaResult<JValueOwned<'env>> {
    let k = call::with_check(env, |env| env.new_string(key))?;
    call::with_check(env, |env| {
        env.call_method(
            map,
            jni::jni_str!("get"),
            jni::jni_sig!("(Ljava/lang/Object;)Ljava/lang/Object;"),
            &[JValue::Object(&k)],
        )
    })
}

/// `list.add(value)` — exact erased signature.
fn list_add<'env>(
    env: &mut Env<'env>,
    list: &JniObject<'env>,
    value: &JniObject<'env>,
) -> JavaResult<()> {
    call::with_check(env, |env| {
        env.call_method(
            list,
            jni::jni_str!("add"),
            jni::jni_sig!("(Ljava/lang/Object;)Z"),
            &[JValue::Object(value)],
        )
        .map(|_| ())
    })
}

/// A fresh empty `java.util.ArrayList`.
fn new_array_list<'env>(env: &mut Env<'env>) -> JavaResult<JniObject<'env>> {
    call::with_check(env, |env| {
        env.new_object(
            JNIString::from("java/util/ArrayList"),
            jni::jni_sig!("()V"),
            &[],
        )
    })
}

/// A fresh empty `java.util.HashMap`.
fn new_hash_map<'env>(env: &mut Env<'env>) -> JavaResult<JniObject<'env>> {
    call::with_check(env, |env| {
        env.new_object(
            JNIString::from("java/util/HashMap"),
            jni::jni_sig!("()V"),
            &[],
        )
    })
}

// ---------------------------------------------------------------------------
// The Serializer
// ---------------------------------------------------------------------------

/// `serde::ser::Serializer` that writes into Java values: scalars become
/// boxed wrappers / `String`, sequences become `ArrayList`, maps/structs
/// become `HashMap`. The produced value is a JNI local reference.
///
/// `pub(crate)` because the bean mapping (`crate::bean`) serializes each
/// property value with it (a bean's fields are *values*; only the top-level
/// struct turns into setters).
pub(crate) struct JavaSerializer<'a, 'env> {
    pub(crate) env: &'a mut Env<'env>,
}

impl<'a, 'env> JavaSerializer<'a, 'env> {
    /// A `str` value → a Java `String` object.
    fn java_string(&mut self, s: &str) -> Result<JniObject<'env>, SerdeError> {
        let js = call::with_check(self.env, |env| env.new_string(s))?;
        Ok(js.into())
    }
}

impl<'a, 'env> serde::Serializer for JavaSerializer<'a, 'env> {
    type Ok = JniObject<'env>;
    type Error = SerdeError;
    type SerializeSeq = SeqBuilder<'a, 'env>;
    type SerializeTuple = SeqBuilder<'a, 'env>;
    type SerializeTupleStruct = SeqBuilder<'a, 'env>;
    type SerializeTupleVariant = UnsupportedBuilder<'a, 'env>;
    type SerializeMap = MapBuilder<'a, 'env>;
    type SerializeStruct = MapBuilder<'a, 'env>;
    type SerializeStructVariant = UnsupportedBuilder<'a, 'env>;

    fn serialize_bool(self, v: bool) -> Result<Self::Ok, Self::Error> {
        Ok(value_of(
            self.env,
            "java/lang/Boolean",
            jni::jni_sig!("(Z)Ljava/lang/Boolean;"),
            JValue::Bool(v),
        )?)
    }
    fn serialize_i8(self, v: i8) -> Result<Self::Ok, Self::Error> {
        // Boxed as `Byte` — the wrapper of the Java `byte` type that `i8`
        // maps to in this crate's `ToJava` (an `Integer` box would not
        // unbox into a `byte` setter parameter).
        Ok(value_of(
            self.env,
            "java/lang/Byte",
            jni::jni_sig!("(B)Ljava/lang/Byte;"),
            JValue::Byte(v),
        )?)
    }
    fn serialize_i16(self, v: i16) -> Result<Self::Ok, Self::Error> {
        // Boxed as `Short` — the wrapper of the Java `short` type that `i16`
        // maps to in this crate's `ToJava`.
        Ok(value_of(
            self.env,
            "java/lang/Short",
            jni::jni_sig!("(S)Ljava/lang/Short;"),
            JValue::Short(v),
        )?)
    }
    fn serialize_i32(self, v: i32) -> Result<Self::Ok, Self::Error> {
        Ok(value_of(
            self.env,
            "java/lang/Integer",
            jni::jni_sig!("(I)Ljava/lang/Integer;"),
            JValue::Int(v),
        )?)
    }
    fn serialize_i64(self, v: i64) -> Result<Self::Ok, Self::Error> {
        Ok(value_of(
            self.env,
            "java/lang/Long",
            jni::jni_sig!("(J)Ljava/lang/Long;"),
            JValue::Long(v),
        )?)
    }
    fn serialize_u8(self, v: u8) -> Result<Self::Ok, Self::Error> {
        Ok(value_of(
            self.env,
            "java/lang/Byte",
            jni::jni_sig!("(B)Ljava/lang/Byte;"),
            JValue::Byte(v as i8),
        )?)
    }
    fn serialize_u16(self, _v: u16) -> Result<Self::Ok, Self::Error> {
        Err(SerdeError::ser_custom(
            "rjava serde: `u16` is not supported (Java has no unsigned 16-bit \
             type) — use i32 or char",
        ))
    }
    fn serialize_u32(self, _v: u32) -> Result<Self::Ok, Self::Error> {
        Err(SerdeError::ser_custom(
            "rjava serde: `u32` is not supported (Java has no unsigned 32-bit \
             type) — use i64",
        ))
    }
    fn serialize_u64(self, _v: u64) -> Result<Self::Ok, Self::Error> {
        Err(SerdeError::ser_custom(
            "rjava serde: `u64` is not supported (Java has no unsigned 64-bit \
             type) — use i64",
        ))
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<Self::Ok, Self::Error> {
        let mut seq = self.serialize_seq(Some(v.len()))?;
        for b in v {
            serde::ser::SerializeSeq::serialize_element(&mut seq, b)?;
        }
        serde::ser::SerializeSeq::end(seq)
    }
    fn serialize_f32(self, v: f32) -> Result<Self::Ok, Self::Error> {
        // Boxed as `Float` — the wrapper of the Java `float` type that `f32`
        // maps to in this crate's `ToJava` (an exact `float` box, so the
        // no-widening rule lets it unbox into a `float` setter parameter).
        Ok(value_of(
            self.env,
            "java/lang/Float",
            jni::jni_sig!("(F)Ljava/lang/Float;"),
            JValue::Float(v),
        )?)
    }
    fn serialize_f64(self, v: f64) -> Result<Self::Ok, Self::Error> {
        Ok(value_of(
            self.env,
            "java/lang/Double",
            jni::jni_sig!("(D)Ljava/lang/Double;"),
            JValue::Double(v),
        )?)
    }
    fn serialize_char(self, v: char) -> Result<Self::Ok, Self::Error> {
        let c = jni::char_to_java(v).map_err(|_| {
            SerdeError::ser_custom(
                "rjava serde: a Rust char above U+FFFF cannot be represented as a \
                 single Java char — use a String",
            )
        })?;
        // Boxed as `java.lang.Character` — the wrapper of the Java `char`
        // type that `char` maps to in this crate's `ToJava`.
        Ok(value_of(
            self.env,
            "java/lang/Character",
            jni::jni_sig!("(C)Ljava/lang/Character;"),
            JValue::Char(c),
        )?)
    }
    fn serialize_str(mut self, v: &str) -> Result<Self::Ok, Self::Error> {
        self.java_string(v)
    }
    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        Ok(JniObject::null())
    }
    fn serialize_some<T: ?Sized + serde::Serialize>(
        self,
        value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        Ok(JniObject::null())
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<Self::Ok, Self::Error> {
        Ok(JniObject::null())
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        Err(SerdeError::ser_custom(
            "rjava serde: enums are not supported — only structs, maps, sequences, primitives and Option",
        ))
    }
    fn serialize_newtype_struct<T: ?Sized + serde::Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + serde::Serialize>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        Err(SerdeError::ser_custom(
            "rjava serde: enums are not supported — only structs, maps, sequences, primitives and Option",
        ))
    }
    fn serialize_seq(
        self,
        _len: Option<usize>,
    ) -> Result<Self::SerializeSeq, Self::Error> {
        let list = new_array_list(&mut *self.env)?;
        Ok(SeqBuilder {
            env: self.env,
            list,
        })
    }
    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        Err(SerdeError::ser_custom(
            "rjava serde: enums are not supported — only structs, maps, sequences, primitives and Option",
        ))
    }
    fn serialize_map(
        self,
        _len: Option<usize>,
    ) -> Result<Self::SerializeMap, Self::Error> {
        let map = new_hash_map(&mut *self.env)?;
        Ok(MapBuilder {
            env: self.env,
            map,
            pending_key: None,
        })
    }
    fn serialize_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        self.serialize_map(Some(len))
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        Err(SerdeError::ser_custom(
            "rjava serde: enums are not supported — only structs, maps, sequences, primitives and Option",
        ))
    }
}

/// A variant builder that can never be constructed successfully: enum
/// serialization is rejected at the `serialize_*_variant` entry point, so
/// these trait impls only exist to satisfy the `Serializer` associated
/// types. The `PhantomData` pins the lifetimes; the struct is never
/// instantiated.
pub(crate) struct UnsupportedBuilder<'a, 'env> {
    _marker: PhantomData<&'a mut Env<'env>>,
}

impl<'a, 'env> serde::ser::SerializeTupleVariant for UnsupportedBuilder<'a, 'env> {
    type Ok = JniObject<'env>;
    type Error = SerdeError;
    fn serialize_field<T: ?Sized + serde::Serialize>(
        &mut self,
        _value: &T,
    ) -> Result<(), Self::Error> {
        Err(SerdeError::ser_custom(
            "rjava serde: enums are not supported",
        ))
    }
    fn end(self) -> Result<Self::Ok, Self::Error> {
        Err(SerdeError::ser_custom(
            "rjava serde: enums are not supported",
        ))
    }
}

impl<'a, 'env> serde::ser::SerializeStructVariant for UnsupportedBuilder<'a, 'env> {
    type Ok = JniObject<'env>;
    type Error = SerdeError;
    fn serialize_field<T: ?Sized + serde::Serialize>(
        &mut self,
        _key: &'static str,
        _value: &T,
    ) -> Result<(), Self::Error> {
        Err(SerdeError::ser_custom(
            "rjava serde: enums are not supported",
        ))
    }
    fn end(self) -> Result<Self::Ok, Self::Error> {
        Err(SerdeError::ser_custom(
            "rjava serde: enums are not supported",
        ))
    }
}

// ---------------------------------------------------------------------------
// Sequence / map building
// ---------------------------------------------------------------------------

/// Serializes a sequence (or tuple) into a `java.util.ArrayList`.
pub(crate) struct SeqBuilder<'a, 'env> {
    env: &'a mut Env<'env>,
    list: JniObject<'env>,
}

impl<'a, 'env> serde::ser::SerializeSeq for SeqBuilder<'a, 'env> {
    type Ok = JniObject<'env>;
    type Error = SerdeError;
    fn serialize_element<T: ?Sized + serde::Serialize>(
        &mut self,
        value: &T,
    ) -> Result<(), Self::Error> {
        let v = value.serialize(JavaSerializer {
            env: &mut *self.env,
        })?;
        list_add(self.env, &self.list, &v)?;
        Ok(())
    }
    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(self.list)
    }
}

impl<'a, 'env> serde::ser::SerializeTuple for SeqBuilder<'a, 'env> {
    type Ok = JniObject<'env>;
    type Error = SerdeError;
    fn serialize_element<T: ?Sized + serde::Serialize>(
        &mut self,
        value: &T,
    ) -> Result<(), Self::Error> {
        serde::ser::SerializeSeq::serialize_element(self, value)
    }
    fn end(self) -> Result<Self::Ok, Self::Error> {
        serde::ser::SerializeSeq::end(self)
    }
}

impl<'a, 'env> serde::ser::SerializeTupleStruct for SeqBuilder<'a, 'env> {
    type Ok = JniObject<'env>;
    type Error = SerdeError;
    fn serialize_field<T: ?Sized + serde::Serialize>(
        &mut self,
        value: &T,
    ) -> Result<(), Self::Error> {
        serde::ser::SerializeSeq::serialize_element(self, value)
    }
    fn end(self) -> Result<Self::Ok, Self::Error> {
        serde::ser::SerializeSeq::end(self)
    }
}

/// Serializes a map (or struct) into a `java.util.HashMap`. The key of each
/// `put` is the value produced by the most recent `serialize_key` /
/// `serialize_field` call.
pub(crate) struct MapBuilder<'a, 'env> {
    env: &'a mut Env<'env>,
    map: JniObject<'env>,
    pending_key: Option<JniObject<'env>>,
}

impl<'a, 'env> MapBuilder<'a, 'env> {
    /// Serialize `key` and store it as the pending key for the next value.
    fn start_key(&mut self, key: &str) -> Result<(), SerdeError> {
        self.pending_key = Some(self.java_string(key)?);
        Ok(())
    }
    /// Serialize `value` and `put` it under the pending key.
    fn end_key_value<T: ?Sized + serde::Serialize>(
        &mut self,
        value: &T,
    ) -> Result<(), SerdeError> {
        let v = value.serialize(JavaSerializer {
            env: &mut *self.env,
        })?;
        let k = self
            .pending_key
            .take()
            .ok_or_else(|| SerdeError::ser_custom(
                "rjava serde: internal error — a map value was serialized without a key",
            ))?;
        map_put(self.env, &self.map, &k, &v)?;
        Ok(())
    }
    fn java_string(&mut self, s: &str) -> Result<JniObject<'env>, SerdeError> {
        let js = call::with_check(self.env, |env| env.new_string(s))?;
        Ok(js.into())
    }
}

impl<'a, 'env> serde::ser::SerializeMap for MapBuilder<'a, 'env> {
    type Ok = JniObject<'env>;
    type Error = SerdeError;
    fn serialize_key<T: ?Sized + serde::Serialize>(
        &mut self,
        key: &T,
    ) -> Result<(), Self::Error> {
        let k = key.serialize(JavaSerializer {
            env: &mut *self.env,
        })?;
        self.pending_key = Some(k);
        Ok(())
    }
    fn serialize_value<T: ?Sized + serde::Serialize>(
        &mut self,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.end_key_value(value)
    }
    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(self.map)
    }
}

impl<'a, 'env> serde::ser::SerializeStruct for MapBuilder<'a, 'env> {
    type Ok = JniObject<'env>;
    type Error = SerdeError;
    fn serialize_field<T: ?Sized + serde::Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.start_key(key)?;
        self.end_key_value(value)
    }
    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(self.map)
    }
}

// ---------------------------------------------------------------------------
// The Deserializer
// ---------------------------------------------------------------------------

/// `serde::de::Deserializer` over a Java value tree: `String` →
/// `visit_string`, boxed numerics → `visit_i64`/`visit_f64`, `Boolean` →
/// `visit_bool`, `null` → `visit_unit` (or `Option::None` via
/// `deserialize_option`), `java.util.List` → `visit_seq`, `java.util.Map` →
/// `visit_map`. Anything else is rejected with an error naming its class.
///
/// `pub(crate)` because the bean mapping (`crate::bean`) feeds each getter's
/// raw `JValueOwned` through this value-level deserializer.
pub(crate) struct JavaDeserializer<'a, 'env> {
    pub(crate) env: &'a mut Env<'env>,
    pub(crate) value: JValueOwned<'env>,
}

/// The runtime binary class name of an object (`Object.getClass().getName()`).
fn runtime_class_name<'env>(
    env: &mut Env<'env>,
    obj: &JniObject<'env>,
) -> JavaResult<String> {
    let cls = call::get_object_class(env, obj)?;
    call::class_name(env, &cls)
}

/// Is `obj` an instance of the slash-separated class/interface `class`?
fn is_instance<'env>(
    env: &mut Env<'env>,
    obj: &JniObject<'env>,
    class: &str,
) -> JavaResult<bool> {
    let cls = call::find_class(env, JNIString::from(class))?;
    call::with_check(env, |env| env.is_instance_of(obj, cls))
}

/// Read a `java.lang.String` object into a Rust `String`.
///
/// `pub(crate)`: also used by the bean mapping (`crate::bean`) to read the
/// nested-bean marker's class string back out of the serde stream.
pub(crate) fn java_string_of<'env>(env: &mut Env<'env>, obj: JniObject<'env>) -> JavaResult<String> {
    let js: JString = env.cast_local::<JString>(obj)?;
    Ok(js.mutf8_chars(env)?.into())
}

/// `wrapper.xxxValue()` → the primitive `JValueOwned`.
fn wrapper_value<'env>(
    env: &mut Env<'env>,
    obj: &JniObject<'env>,
    method: &'static JNIStr,
    sig: MethodSignature<'static, 'static>,
) -> JavaResult<JValueOwned<'env>> {
    call::with_check(env, |env| env.call_method(obj, method, sig, &[]))
}

/// The fixed message for unsupported Java values (naming the class).
fn unsupported_value(name: &str) -> SerdeError {
    SerdeError(JavaError::Serde(format!(
        "unsupported Java value of class `{name}` for serde deserialization; \
         supported: String, Boolean, Byte/Short/Integer/Long/Character, \
         Float/Double, java.util.List, java.util.Map and null"
    )))
}

/// `list.size()`.
fn list_size<'env>(env: &mut Env<'env>, list: &JniObject<'env>) -> JavaResult<usize> {
    let r = call::with_check(env, |env| {
        env.call_method(list, jni::jni_str!("size"), jni::jni_sig!("()I"), &[])
    })?;
    match r {
        JValueOwned::Int(n) => Ok(n as usize),
        _ => Err(JavaError::InvalidArgument(
            "internal error: List.size() did not return an int",
        )),
    }
}

/// `list.get(i)` → the element (object or null).
fn list_get<'env>(
    env: &mut Env<'env>,
    list: &JniObject<'env>,
    index: usize,
) -> JavaResult<JValueOwned<'env>> {
    call::with_check(env, |env| {
        env.call_method(
            list,
            jni::jni_str!("get"),
            jni::jni_sig!("(I)Ljava/lang/Object;"),
            &[JValue::Int(index as i32)],
        )
    })
}

/// `map.entrySet().iterator()` — the live iterator over the map's entries.
fn map_iterator<'env>(
    env: &mut Env<'env>,
    map: &JniObject<'env>,
) -> JavaResult<JniObject<'env>> {
    let set = call::with_check(env, |env| {
        env.call_method(
            map,
            jni::jni_str!("entrySet"),
            jni::jni_sig!("()Ljava/util/Set;"),
            &[],
        )
    })?;
    let set = match set {
        JValueOwned::Object(o) => o,
        _ => {
            return Err(JavaError::InvalidArgument(
                "internal error: Map.entrySet() did not return a Set",
            ))
        }
    };
    let iter = call::with_check(env, |env| {
        env.call_method(
            &set,
            jni::jni_str!("iterator"),
            jni::jni_sig!("()Ljava/util/Iterator;"),
            &[],
        )
    })?;
    match iter {
        JValueOwned::Object(o) => Ok(o),
        _ => Err(JavaError::InvalidArgument(
            "internal error: Set.iterator() did not return an Iterator",
        )),
    }
}

/// Is `obj` a `java.util.Map` holding exactly the reserved `JavaBean`
/// marker entries (`__rjava_bean_class` + `value`)? Serializing a nested
/// [`JavaBean`](crate::bean::JavaBean) through the **value** tree
/// (`JavaMap` / `from_object`) produces exactly such a map; reading it back
/// unwraps the marker so the field round-trips as a plain struct value.
fn is_bean_marker_map<'env>(
    env: &mut Env<'env>,
    obj: &JniObject<'env>,
) -> JavaResult<bool> {
    if !is_instance(env, obj, "java/util/Map")? {
        return Ok(false);
    }
    if list_size(env, obj)? != 2 {
        return Ok(false);
    }
    Ok(map_contains_key(env, obj, crate::bean::BEAN_MARKER_CLASS_KEY)?
        && map_contains_key(env, obj, crate::bean::BEAN_MARKER_VALUE_KEY)?)
}

/// Dispatch a Java value to the visitor, driven by the runtime type.
fn visit_value<'de, 'env, V: serde::de::Visitor<'de>>(
    env: &mut Env<'env>,
    value: JValueOwned<'env>,
    visitor: V,
) -> Result<V::Value, SerdeError> {
    match value {
        JValueOwned::Bool(b) => visitor.visit_bool::<SerdeError>(b),
        JValueOwned::Int(i) => visitor.visit_i64::<SerdeError>(i as i64),
        JValueOwned::Long(l) => visitor.visit_i64::<SerdeError>(l),
        JValueOwned::Short(s) => visitor.visit_i64::<SerdeError>(s as i64),
        JValueOwned::Byte(b) => visitor.visit_i64::<SerdeError>(b as i64),
        JValueOwned::Float(f) => visitor.visit_f64::<SerdeError>(f as f64),
        JValueOwned::Double(d) => visitor.visit_f64::<SerdeError>(d),
        JValueOwned::Char(c) => match jni::char_from_java(c) {
            Ok(ch) => visitor.visit_char::<SerdeError>(ch),
            Err(_) => Err(SerdeError::de_custom(
                "rjava serde: this Java char is an unpaired UTF-16 surrogate",
            )),
        },
        JValueOwned::Object(o) if o.is_null() => {
            visitor.visit_unit::<SerdeError>()
        }
        JValueOwned::Object(o) => visit_object(env, o, visitor),
        JValueOwned::Void => Err(SerdeError::de_custom(
            "rjava serde: cannot deserialize a Java void value",
        )),
    }
}

/// Dispatch an object (non-null) by its runtime class.
fn visit_object<'de, 'env, V: serde::de::Visitor<'de>>(
    env: &mut Env<'env>,
    o: JniObject<'env>,
    visitor: V,
) -> Result<V::Value, SerdeError> {
    let name = runtime_class_name(env, &o)?;
    match name.as_str() {
        "java.lang.String" => {
            let s = java_string_of(env, o)?;
            visitor.visit_string::<SerdeError>(s)
        }
        "java.lang.Boolean" => {
            let r = wrapper_value(env, &o, jni::jni_str!("booleanValue"), jni::jni_sig!("()Z"))?;
            match r {
                JValueOwned::Bool(b) => visitor.visit_bool::<SerdeError>(b),
                _ => Err(internal_primitive()),
            }
        }
        "java.lang.Integer" => {
            let r = wrapper_value(env, &o, jni::jni_str!("intValue"), jni::jni_sig!("()I"))?;
            match r {
                JValueOwned::Int(i) => visitor.visit_i64::<SerdeError>(i as i64),
                _ => Err(internal_primitive()),
            }
        }
        "java.lang.Long" => {
            let r = wrapper_value(env, &o, jni::jni_str!("longValue"), jni::jni_sig!("()J"))?;
            match r {
                JValueOwned::Long(l) => visitor.visit_i64::<SerdeError>(l),
                _ => Err(internal_primitive()),
            }
        }
        "java.lang.Short" => {
            let r = wrapper_value(env, &o, jni::jni_str!("shortValue"), jni::jni_sig!("()S"))?;
            match r {
                JValueOwned::Short(s) => visitor.visit_i64::<SerdeError>(s as i64),
                _ => Err(internal_primitive()),
            }
        }
        "java.lang.Byte" => {
            let r = wrapper_value(env, &o, jni::jni_str!("byteValue"), jni::jni_sig!("()B"))?;
            match r {
                JValueOwned::Byte(b) => visitor.visit_i64::<SerdeError>(b as i64),
                _ => Err(internal_primitive()),
            }
        }
        "java.lang.Character" => {
            let r = wrapper_value(env, &o, jni::jni_str!("charValue"), jni::jni_sig!("()C"))?;
            match r {
                JValueOwned::Char(c) => match jni::char_from_java(c) {
                    Ok(ch) => visitor.visit_char::<SerdeError>(ch),
                    Err(_) => Err(SerdeError::de_custom(
                        "rjava serde: this Java char is an unpaired UTF-16 surrogate",
                    )),
                },
                _ => Err(internal_primitive()),
            }
        }
        "java.lang.Double" => {
            let r = wrapper_value(env, &o, jni::jni_str!("doubleValue"), jni::jni_sig!("()D"))?;
            match r {
                JValueOwned::Double(d) => visitor.visit_f64::<SerdeError>(d),
                _ => Err(internal_primitive()),
            }
        }
        "java.lang.Float" => {
            let r = wrapper_value(env, &o, jni::jni_str!("floatValue"), jni::jni_sig!("()F"))?;
            match r {
                JValueOwned::Float(f) => visitor.visit_f64::<SerdeError>(f as f64),
                _ => Err(internal_primitive()),
            }
        }
        _ => {
            if is_instance(env, &o, "java/util/List")? {
                let len = list_size(env, &o)?;
                visitor.visit_seq(SeqAccess { env, list: o, index: 0, len })
            } else if is_instance(env, &o, "java/util/Map")? {
                let iter = map_iterator(env, &o)?;
                visitor.visit_map(MapAccess { env, iter, current: None })
            } else {
                Err(unsupported_value(&name))
            }
        }
    }
}

/// An internal invariant failure when a wrapper read returns the wrong
/// primitive variant (cannot happen for the exact JDK signatures above).
fn internal_primitive() -> SerdeError {
    SerdeError(JavaError::InvalidArgument(
        "internal error: a wrapper xxxValue() call did not return the expected primitive",
    ))
}

impl<'a, 'env> JavaDeserializer<'a, 'env> {
    /// Is the held value Java `null` (a null object reference)?
    fn is_null(&self) -> bool {
        self.value.is_null()
    }
}

/// Every deserializer entry point other than `any`, `option`, `identifier`
/// and `unit` dispatches on the *runtime* Java type: the value selects the
/// shape, and serde's own visitors range/type-check against the annotated
/// Rust type (e.g. an `Integer` visit reaches `i32`, `u8` and `i64` fields
/// alike, with a range error when the value does not fit).
macro_rules! delegate_to_any {
    ($($method:ident),* $(,)?) => {
        $(
            fn $method<V: serde::de::Visitor<'de>>(
                self,
                visitor: V,
            ) -> Result<V::Value, Self::Error> {
                self.deserialize_any(visitor)
            }
        )*
    };
}

impl<'de, 'a, 'env> serde::de::Deserializer<'de> for JavaDeserializer<'a, 'env> {
    type Error = SerdeError;

    fn deserialize_any<V: serde::de::Visitor<'de>>(
        self,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let JavaDeserializer { env, value } = self;
        visit_value(env, value, visitor)
    }

    /// `Option<T>`: Java `null` → `visit_none`, anything else →
    /// `visit_some` (the inner value deserializes in the usual way).
    fn deserialize_option<V: serde::de::Visitor<'de>>(
        self,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        if self.is_null() {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    /// `u8`: a Java `byte` is signed and `u8` maps to it **by bit pattern**
    /// (the crate's `u8` ⇄ `byte` convention — see the [module docs](self)),
    /// so a `Byte` value reads back with the unsigned interpretation:
    /// `byte` `-1` → `u8` `255`. Every other numeric shape dispatches as
    /// usual and serde range-checks the visit.
    fn deserialize_u8<V: serde::de::Visitor<'de>>(
        self,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let JavaDeserializer { env, value } = self;
        let signed_byte = match &value {
            JValueOwned::Byte(b) => Some(*b),
            JValueOwned::Object(o) if !o.is_null() => {
                if runtime_class_name(env, o)? == "java.lang.Byte" {
                    match wrapper_value(env, o, jni::jni_str!("byteValue"), jni::jni_sig!("()B"))? {
                        JValueOwned::Byte(b) => Some(b),
                        _ => return Err(internal_primitive()),
                    }
                } else {
                    None
                }
            }
            _ => None,
        };
        match signed_byte {
            Some(b) => visitor.visit_u8::<SerdeError>(b as u8),
            None => {
                let de = JavaDeserializer { env, value };
                de.deserialize_any(visitor)
            }
        }
    }

    /// Map keys deserializing into struct fields: only Java `String` keys
    /// are accepted (anything else is an error naming the key's class).
    fn deserialize_identifier<V: serde::de::Visitor<'de>>(
        self,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let JavaDeserializer { env, value } = self;
        match value {
            JValueOwned::Object(o) if !o.is_null() => {
                let name = runtime_class_name(env, &o)?;
                if name == "java.lang.String" {
                    let s = java_string_of(env, o)?;
                    visitor.visit_string::<SerdeError>(s)
                } else {
                    Err(SerdeError(JavaError::Serde(format!(
                        "map keys must be Java Strings when deserializing into a Rust \
                         struct or map; found a key of class `{name}`"
                    ))))
                }
            }
            _ => Err(SerdeError(JavaError::Serde(
                "map keys must be Java Strings when deserializing into a Rust \
                 struct or map; found a null or primitive key"
                    .to_string(),
            ))),
        }
    }

    /// `()` — the crate's "call it and discard" semantics: *any* Java value
    /// (including `null`) reads back as `()`, mirroring `FromJava for ()`.
    fn deserialize_unit<V: serde::de::Visitor<'de>>(
        self,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_unit()
    }

    fn deserialize_unit_struct<V: serde::de::Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.deserialize_unit(visitor)
    }

    fn deserialize_newtype_struct<V: serde::de::Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        // A nested `JavaBean<T>` field routes through
        // `deserialize_newtype_struct("JavaBean", …)` (see the `Deserialize`
        // impl in `crate::bean`); the bean read needs the JNI environment,
        // which only this deserializer carries, so the marker name is
        // intercepted here: a bean object is read property by property
        // through its getters (runtime class derived — no class string
        // needed in the stream), and the reserved marker map the *value*
        // tree (`JavaMap` / `from_object`) produces is unwrapped back into
        // the plain struct. Any other newtype struct keeps the ordinary
        // value-tree semantics.
        if name == crate::bean::BEAN_MARKER_STRUCT {
            let JavaDeserializer { env, value } = self;
            return match value {
                JValueOwned::Object(o) if !o.is_null() => {
                    if is_bean_marker_map(env, &o)? {
                        let inner = map_get(env, &o, crate::bean::BEAN_MARKER_VALUE_KEY)?;
                        visitor.visit_newtype_struct(JavaDeserializer { env, value: inner })
                    } else {
                        let global = env.new_global_ref(o)?;
                        visitor.visit_newtype_struct(crate::bean::BeanDeserializer {
                            env,
                            obj: &global,
                        })
                    }
                }
                _ => Err(SerdeError::de_custom(
                    "a `JavaBean<T>` field requires a non-null Java object (read through \
                     its getters) or the `__rjava_bean_class` marker map (read through the \
                     value tree); got null or a primitive value",
                )),
            };
        }
        self.deserialize_any(visitor)
    }

    fn deserialize_enum<V: serde::de::Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value, Self::Error> {
        Err(SerdeError::de_custom(
            "rjava serde: enums are not supported — only structs, maps, \
             sequences, primitives and Option",
        ))
    }

    fn deserialize_ignored_any<V: serde::de::Visitor<'de>>(
        self,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.deserialize_any(visitor)
    }

    delegate_to_any!(
        deserialize_bool,
        deserialize_i8,
        deserialize_i16,
        deserialize_i32,
        deserialize_i64,
        deserialize_i128,
        deserialize_u16,
        deserialize_u32,
        deserialize_u64,
        deserialize_u128,
        deserialize_f32,
        deserialize_f64,
        deserialize_char,
        deserialize_str,
        deserialize_string,
        deserialize_bytes,
        deserialize_byte_buf,
        deserialize_seq,
        deserialize_map,
    );

    fn deserialize_tuple<V: serde::de::Visitor<'de>>(
        self,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.deserialize_any(visitor)
    }

    fn deserialize_tuple_struct<V: serde::de::Visitor<'de>>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.deserialize_any(visitor)
    }

    fn deserialize_struct<V: serde::de::Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.deserialize_any(visitor)
    }
}

// ---------------------------------------------------------------------------
// SeqAccess / MapAccess
// ---------------------------------------------------------------------------

/// Sequential access over a `java.util.ArrayList` (indexed `get(i)`).
struct SeqAccess<'a, 'env> {
    env: &'a mut Env<'env>,
    list: JniObject<'env>,
    index: usize,
    len: usize,
}

impl<'de, 'a, 'env> serde::de::SeqAccess<'de> for SeqAccess<'a, 'env> {
    type Error = SerdeError;
    fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
    where
        T: serde::de::DeserializeSeed<'de>,
    {
        if self.index >= self.len {
            return Ok(None);
        }
        let value = list_get(self.env, &self.list, self.index)?;
        self.index += 1;
        let de = JavaDeserializer {
            env: &mut *self.env,
            value,
        };
        seed.deserialize(de).map(Some)
    }
    fn size_hint(&self) -> Option<usize> {
        Some(self.len.saturating_sub(self.index))
    }
}

/// Key/value access over a `java.util.Map` (via `entrySet().iterator()`).
///
/// [`MapAccess::next_key_seed`] advances the iterator and caches the entry;
/// [`MapAccess::next_value_seed`] reads that entry's value. Keys are read
/// through the deserializer's `deserialize_identifier` path, so a
/// non-`String` key is rejected.
struct MapAccess<'a, 'env> {
    env: &'a mut Env<'env>,
    iter: JniObject<'env>,
    current: Option<JniObject<'env>>,
}

impl<'a, 'env> MapAccess<'a, 'env> {
    /// `iter.hasNext()`.
    fn has_next(&mut self) -> Result<bool, SerdeError> {
        let r = call::with_check(self.env, |env| {
            env.call_method(
                &self.iter,
                jni::jni_str!("hasNext"),
                jni::jni_sig!("()Z"),
                &[],
            )
        })?;
        match r {
            JValueOwned::Bool(b) => Ok(b),
            _ => Err(SerdeError(JavaError::InvalidArgument(
                "internal error: Iterator.hasNext() did not return a boolean",
            ))),
        }
    }

    /// `iter.next()` → the next `Map.Entry`.
    fn next_entry(&mut self) -> Result<JniObject<'env>, SerdeError> {
        let r = call::with_check(self.env, |env| {
            env.call_method(
                &self.iter,
                jni::jni_str!("next"),
                jni::jni_sig!("()Ljava/lang/Object;"),
                &[],
            )
        })?;
        match r {
            JValueOwned::Object(o) => Ok(o),
            _ => Err(SerdeError(JavaError::InvalidArgument(
                "internal error: Iterator.next() did not return an entry",
            ))),
        }
    }

    /// `entry.getKey()` / `entry.getValue()`.
    fn entry_side(
        &mut self,
        entry: &JniObject<'env>,
        method: &'static JNIStr,
    ) -> Result<JValueOwned<'env>, SerdeError> {
        call::with_check(self.env, |env| {
            env.call_method(entry, method, jni::jni_sig!("()Ljava/lang/Object;"), &[])
        })
        .map_err(SerdeError)
    }
}

impl<'de, 'a, 'env> serde::de::MapAccess<'de> for MapAccess<'a, 'env> {
    type Error = SerdeError;
    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: serde::de::DeserializeSeed<'de>,
    {
        if !self.has_next()? {
            return Ok(None);
        }
        let entry = self.next_entry()?;
        let key = self.entry_side(&entry, jni::jni_str!("getKey"))?;
        self.current = Some(entry);
        let de = JavaDeserializer {
            env: &mut *self.env,
            value: key,
        };
        seed.deserialize(de).map(Some)
    }
    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::DeserializeSeed<'de>,
    {
        let entry = self
            .current
            .take()
            .ok_or_else(|| SerdeError::de_custom(
                "rjava serde: internal error — a map value was requested without a key",
            ))?;
        let value = self.entry_side(&entry, jni::jni_str!("getValue"))?;
        let de = JavaDeserializer {
            env: &mut *self.env,
            value,
        };
        seed.deserialize(de)
    }
}
