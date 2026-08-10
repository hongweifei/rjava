//! Rust ⇄ Java value conversion — the mlua `IntoLua`/`FromLua` analog.
//!
//! * [`ToJava`] converts Rust values **into** JNI arguments (and array
//!   elements). A tuple is an *argument list*: `("Hello", 5_i32)` becomes the
//!   two arguments of a constructor or method call. `()` is the empty argument
//!   list.
//! * [`FromJava`] converts a JNI return value **back into** Rust, driven by
//!   the caller's type annotation: `let len: i32 = sb.call("length", ())?;`.
//!
//! There are **no** implementations for `u32`/`u64`: Java has no unsigned
//! integer types, so those cannot map onto any JNI type (see the crate docs).
//! `u16` is likewise absent — use [`char`], which maps to the Java `char`
//! type.

use jni::objects::{JClass as JniClass, JObject as JniObject, JObjectArray, JString};
use jni::{Env, JValue, JValueOwned};

use crate::array::{
    fill_primitive_array, new_object_array_local, new_primitive_array, ArrayKind,
    JavaArrayElement, JavaVecElement, VecKind,
};
use crate::error::{JavaError, JavaResult};
use crate::handles::{JArray, JClass, JObject};

/// An *owned* Java value used while building JNI argument lists.
///
/// This mirrors [`jni::JValue`] but **owns** its object reference instead of
/// borrowing one, which lets tuple conversions build argument lists without
/// lifetime gymnastics. It is an internal plumbing type; you never construct
/// it yourself.
#[doc(hidden)]
#[derive(Debug)]
pub enum JavaArg<'env> {
    Object(JniObject<'env>),
    Bool(bool),
    Byte(i8),
    Char(u16),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
}

/// Conversion from a Rust value into JNI call arguments (mlua's `IntoLua`).
///
/// Implementations exist for:
///
/// * `()` and tuples `(T1, ..., Tn)` up to 64 elements — used as method /
///   constructor argument lists,
/// * `bool`, `i8`, `i16`, `i32`, `i64`, `f32`, `f64`, `char` (Rust `char` →
///   Java `char`; code points above `U+FFFF` are rejected with
///   [`JavaError::InvalidArgument`]),
/// * `String` / `&str` (converted eagerly, incl. non-ASCII text),
/// * `Option<T>` — `None` → Java `null`,
/// * `Vec<T>` → a Java array: primitives (`Vec<i32>` → `int[]` …), `u8`
///   (`byte[]` via bit-pattern casts), and reference-typed elements —
///   `String[]` ↔ `Vec<String>`, [`crate::handles::JObject`] (`Object[]`),
///   [`crate::handles::JClass`] (`Class[]`), [`crate::handles::JArray`]
///   (arrays of arrays) — with `Option<T>` elements for null-tolerant
///   object arrays,
/// * [`crate::handles::JObject`], [`crate::handles::JClass`],
///   [`crate::handles::JArray`] pass through as object references.
///
/// There is deliberately **no** `ToJava` for `u32`/`u64` (Java has no unsigned
/// integers) or `u16` (use `char` for the Java `char` type).
pub trait ToJava {
    /// Convert this value into JNI arguments.
    ///
    /// Tuples yield one entry per element; scalar values yield a single-entry
    /// list; `()` yields the empty list.
    fn to_java<'env>(&self, env: &mut Env<'env>) -> JavaResult<Vec<JavaArg<'env>>>;

    /// JNI type fragment(s) for this value when used as method arguments
    /// (no surrounding parentheses). E.g. `i32` → `"I"`, `&str` →
    /// `"Ljava/lang/String;"`, `(i32, String)` → `"ILjava/lang/String;"`.
    fn java_args(&self) -> String;
}

/// Conversion from a JNI return value into a Rust value (mlua's `FromLua`).
///
/// The caller picks `R` by annotating the call site; the annotation also
/// selects the JNI signature fragment used to look the method/field up, so it
/// must match the *actual* Java return type (see the crate docs for the
/// object-type rule).
///
/// `Option<T>` additionally maps a Java `null` return to `None`; plain
/// reference types ([`JObject`](crate::handles::JObject),
/// [`JClass`](crate::handles::JClass), [`JArray`](crate::handles::JArray),
/// `String`, `Vec<T>`) turn a `null` return into an error instead.
pub trait FromJava: Sized {
    /// Convert a JNI return value into `Self`.
    fn from_java<'env>(env: &mut Env<'env>, value: JValueOwned<'env>) -> JavaResult<Self>;

    /// JNI type fragment for this type as a method/field return value, e.g.
    /// `i32` → `"I"`, `String` → `"Ljava/lang/String;"`, `()` → `"V"`.
    fn java_return_type() -> String;
}

// ---------------------------------------------------------------------------
// () — the empty argument list and the void return type
// ---------------------------------------------------------------------------

impl<T: ToJava> ToJava for &T {
    fn to_java<'env>(&self, env: &mut Env<'env>) -> JavaResult<Vec<JavaArg<'env>>> {
        (*self).to_java(env)
    }
    fn java_args(&self) -> String {
        (*self).java_args()
    }
}

impl ToJava for () {
    fn to_java<'env>(&self, _env: &mut Env<'env>) -> JavaResult<Vec<JavaArg<'env>>> {
        Ok(Vec::new())
    }
    fn java_args(&self) -> String {
        String::new()
    }
}

impl FromJava for () {
    fn from_java<'env>(_env: &mut Env<'env>, _value: JValueOwned<'env>) -> JavaResult<Self> {
        // `()` means "call it and discard whatever it returns" — this is what
        // makes `call_void("append", ...)` work even though `append` returns a
        // StringBuilder.
        Ok(())
    }
    fn java_return_type() -> String {
        // The initial signature guess for the lookup; the reflection fallback
        // fixes it up when the method returns something else.
        String::from("V")
    }
}

// ---------------------------------------------------------------------------
// Primitives
// ---------------------------------------------------------------------------

macro_rules! impl_primitive {
    ($t:ty, $variant:ident, $letter:literal, $expected:literal) => {
        impl ToJava for $t {
            fn to_java<'env>(&self, _env: &mut Env<'env>) -> JavaResult<Vec<JavaArg<'env>>> {
                Ok(vec![JavaArg::$variant(*self)])
            }
            fn java_args(&self) -> String {
                String::from($letter)
            }
        }
        impl FromJava for $t {
            fn from_java<'env>(_env: &mut Env<'env>, value: JValueOwned<'env>) -> JavaResult<Self> {
                match value {
                    JValueOwned::$variant(v) => Ok(v),
                    _ => Err(JavaError::InvalidArgument($expected)),
                }
            }
            fn java_return_type() -> String {
                String::from($letter)
            }
        }
    };
}

impl_primitive!(
    bool,
    Bool,
    "Z",
    "expected a Java `boolean` return value but got a different Java type"
);
impl_primitive!(
    i8,
    Byte,
    "B",
    "expected a Java `byte` return value but got a different Java type"
);
impl_primitive!(
    i16,
    Short,
    "S",
    "expected a Java `short` return value but got a different Java type"
);
impl_primitive!(
    i32,
    Int,
    "I",
    "expected a Java `int` return value but got a different Java type"
);
impl_primitive!(
    i64,
    Long,
    "J",
    "expected a Java `long` return value but got a different Java type"
);
impl_primitive!(
    f32,
    Float,
    "F",
    "expected a Java `float` return value but got a different Java type"
);
impl_primitive!(
    f64,
    Double,
    "D",
    "expected a Java `double` return value but got a different Java type"
);

// `u8` maps to the Java `byte` type — Java's `byte` is signed, so the value
// is cast through `i8` (the bit pattern is preserved, mirroring the `Vec<u8>`
// → `byte[]` conversion). This is what makes `u8` usable as a `Vec` element
// (see [`crate::array::JavaVecElement`]).
impl ToJava for u8 {
    fn to_java<'env>(&self, _env: &mut Env<'env>) -> JavaResult<Vec<JavaArg<'env>>> {
        Ok(vec![JavaArg::Byte(*self as i8)])
    }
    fn java_args(&self) -> String {
        String::from("B")
    }
}

impl FromJava for u8 {
    fn from_java<'env>(_env: &mut Env<'env>, value: JValueOwned<'env>) -> JavaResult<Self> {
        match value {
            JValueOwned::Byte(v) => Ok(v as u8),
            _ => Err(JavaError::InvalidArgument(
                "expected a Java `byte` return value but got a different Java type",
            )),
        }
    }
    fn java_return_type() -> String {
        String::from("B")
    }
}

impl ToJava for char {
    fn to_java<'env>(&self, _env: &mut Env<'env>) -> JavaResult<Vec<JavaArg<'env>>> {
        let c = jni::char_to_java(*self).map_err(|_| {
            JavaError::InvalidArgument(
                "a Rust char above U+FFFF cannot be represented as a single Java `char`; \
                 use a String instead",
            )
        })?;
        Ok(vec![JavaArg::Char(c)])
    }
    fn java_args(&self) -> String {
        String::from("C")
    }
}

impl FromJava for char {
    fn from_java<'env>(_env: &mut Env<'env>, value: JValueOwned<'env>) -> JavaResult<Self> {
        match value {
            JValueOwned::Char(c) => jni::char_from_java(c).map_err(|_| {
                JavaError::InvalidArgument(
                    "this Java char is an unpaired UTF-16 surrogate and cannot be \
                     represented as a single Rust char",
                )
            }),
            _ => Err(JavaError::InvalidArgument(
                "expected a Java `char` return value but got a different Java type",
            )),
        }
    }
    fn java_return_type() -> String {
        String::from("C")
    }
}

// ---------------------------------------------------------------------------
// Strings — converted eagerly (no JString handle type in the public API)
// ---------------------------------------------------------------------------

impl ToJava for String {
    fn to_java<'env>(&self, env: &mut Env<'env>) -> JavaResult<Vec<JavaArg<'env>>> {
        let s: JString = env.new_string(self)?;
        Ok(vec![JavaArg::Object(s.into())])
    }
    fn java_args(&self) -> String {
        String::from("Ljava/lang/String;")
    }
}

impl ToJava for &str {
    fn to_java<'env>(&self, env: &mut Env<'env>) -> JavaResult<Vec<JavaArg<'env>>> {
        let s: JString = env.new_string(*self)?;
        Ok(vec![JavaArg::Object(s.into())])
    }
    fn java_args(&self) -> String {
        String::from("Ljava/lang/String;")
    }
}

impl FromJava for String {
    fn from_java<'env>(env: &mut Env<'env>, value: JValueOwned<'env>) -> JavaResult<Self> {
        match value {
            JValueOwned::Object(o) if !o.is_null() => {
                let s: JString = env.cast_local::<JString>(o)?;
                Ok(s.mutf8_chars(env)?.into())
            }
            JValueOwned::Object(_) => Err(JavaError::InvalidArgument(
                "method returned null where a String was expected \
                 (use Option<String> to accept null)",
            )),
            _ => Err(JavaError::InvalidArgument(
                "expected a Java `String` return value but got a different Java type",
            )),
        }
    }
    fn java_return_type() -> String {
        String::from("Ljava/lang/String;")
    }
}

// ---------------------------------------------------------------------------
// Option<T> — None ⇄ null
// ---------------------------------------------------------------------------

impl<T: ToJava> ToJava for Option<T> {
    fn to_java<'env>(&self, env: &mut Env<'env>) -> JavaResult<Vec<JavaArg<'env>>> {
        match self {
            // `null` is an object reference, so `Option<T>` as a method
            // argument only makes sense for reference types `T`.
            None => Ok(vec![JavaArg::Object(JniObject::null())]),
            Some(v) => v.to_java(env),
        }
    }
    fn java_args(&self) -> String {
        match self {
            Some(v) => v.java_args(),
            // A null value has no concrete type. The generic `Object`
            // fragment keeps the argument count honest (an empty fragment
            // would trip jni's pre-call arg-count check and never reach the
            // reflection fallback); the fallback then resolves the exact
            // parameter type — a null matches any reference parameter and
            // never a primitive one.
            None => String::from("Ljava/lang/Object;"),
        }
    }
}

impl<T: FromJava> FromJava for Option<T> {
    fn from_java<'env>(env: &mut Env<'env>, value: JValueOwned<'env>) -> JavaResult<Self> {
        if value.is_null() {
            Ok(None)
        } else {
            Ok(Some(T::from_java(env, value)?))
        }
    }
    fn java_return_type() -> String {
        T::java_return_type()
    }
}

// ---------------------------------------------------------------------------
// Handle pass-through
// ---------------------------------------------------------------------------

// `JObject`'s `ToJava`/`FromJava` impls live next to the type in
// `crate::handles` (they need access to its private `global` field).

impl ToJava for JClass {
    fn to_java<'env>(&self, env: &mut Env<'env>) -> JavaResult<Vec<JavaArg<'env>>> {
        let cls: JniClass = env.new_local_ref(&*self.global)?;
        Ok(vec![JavaArg::Object(cls.into())])
    }
    fn java_args(&self) -> String {
        String::from("Ljava/lang/Class;")
    }
}

impl FromJava for JClass {
    fn from_java<'env>(env: &mut Env<'env>, value: JValueOwned<'env>) -> JavaResult<Self> {
        match value {
            JValueOwned::Object(o) if !o.is_null() => {
                let cls: JniClass = env.cast_local::<JniClass>(o)?;
                Ok(JClass::from_global(env.new_global_ref(cls)?))
            }
            JValueOwned::Object(_) => Err(JavaError::InvalidArgument(
                "method returned null where a Class was expected",
            )),
            _ => Err(JavaError::InvalidArgument(
                "expected a java.lang.Class return value but got a different Java type",
            )),
        }
    }
    fn java_return_type() -> String {
        String::from("Ljava/lang/Class;")
    }
}

impl<T: JavaArrayElement> ToJava for JArray<T> {
    fn to_java<'env>(&self, env: &mut Env<'env>) -> JavaResult<Vec<JavaArg<'env>>> {
        Ok(vec![JavaArg::Object(env.new_local_ref(&*self.global)?)])
    }
    fn java_args(&self) -> String {
        format!("[{}", T::element_signature())
    }
}

impl ToJava for JArray<JObject> {
    fn to_java<'env>(&self, env: &mut Env<'env>) -> JavaResult<Vec<JavaArg<'env>>> {
        Ok(vec![JavaArg::Object(env.new_local_ref(&*self.global)?)])
    }
    fn java_args(&self) -> String {
        String::from("[Ljava/lang/Object;")
    }
}

impl<T: JavaArrayElement> FromJava for JArray<T> {
    fn from_java<'env>(env: &mut Env<'env>, value: JValueOwned<'env>) -> JavaResult<Self> {
        JArray::from_value(env, value)
    }
    fn java_return_type() -> String {
        format!("[{}", T::element_signature())
    }
}

impl FromJava for JArray<JObject> {
    fn from_java<'env>(env: &mut Env<'env>, value: JValueOwned<'env>) -> JavaResult<Self> {
        JArray::from_value_obj(env, value)
    }
    fn java_return_type() -> String {
        String::from("[Ljava/lang/Object;")
    }
}

// ---------------------------------------------------------------------------
// Vec<T> — Rust vectors map to Java arrays (primitives, u8, and reference
// types; `T` is a JavaVecElement, see crate::array)
// ---------------------------------------------------------------------------

/// Bulk-read a primitive array into a `Vec<T>` with one JNI region call.
///
/// `E` is the array's element type (one of the [`JavaArrayElement`]
/// primitives — equal to `T` for every primitive `Vec<T>`); `wrap` turns a
/// raw element into the [`JValueOwned`] variant `T::from_java` expects, and
/// `T::from_java` re-checks the variant (the `E == T` cases always match).
/// Shared by the `Primitive` arm of `FromJava for Vec<T>`.
fn read_primitive_vec<'env, E, T>(
    env: &mut Env<'env>,
    value: JValueOwned<'env>,
    wrap: impl Fn(E) -> JValueOwned<'env>,
) -> JavaResult<Vec<T>>
where
    E: JavaArrayElement,
    T: FromJava,
{
    let arr = JArray::<E>::from_value(env, value)?;
    let elems = arr.to_vec_local(env)?;
    elems.into_iter().map(|e| T::from_java(env, wrap(e))).collect()
}

impl<T: JavaVecElement> ToJava for Vec<T> {
    fn to_java<'env>(&self, env: &mut Env<'env>) -> JavaResult<Vec<JavaArg<'env>>> {
        match T::KIND {
            // Primitives: build via the JNI typed-array APIs and fill from
            // the per-element raw values (identical to the pre-unification
            // `Vec<T: JavaArrayElement>` path).
            VecKind::Primitive(kind) => {
                let len = self.len();
                let array = new_primitive_array(env, len, kind)?;
                let mut args: Vec<JavaArg<'static>> = Vec::with_capacity(len);
                for v in self {
                    args.push(v.__as_vec_element_arg()?);
                }
                fill_primitive_array(env, &array, kind, &args)?;
                Ok(vec![JavaArg::Object(array)])
            }
            // `u8` maps to `byte[]` — Java's `byte` is signed, so the values
            // are cast through `i8` (the bit pattern is preserved).
            VecKind::U8 => {
                let mut bytes: Vec<i8> = Vec::with_capacity(self.len());
                for v in self {
                    match v.__as_vec_element_arg()? {
                        JavaArg::Byte(b) => bytes.push(b),
                        _ => {
                            return Err(JavaError::InvalidArgument(
                                "internal error: a u8 element must convert to a byte",
                            ))
                        }
                    }
                }
                let arr = JArray::from_vec_local(env, &bytes)?;
                Ok(vec![JavaArg::Object(arr)])
            }
            // Reference types: `NewObjectArray(element_class, n)` + one
            // `set_element` per value. `None` `Option` elements convert to
            // `null` (their `ToJava` yields a null object reference).
            VecKind::Object => {
                let arr = new_object_array_local(env, &T::element_class(), self.len())?;
                for (i, v) in self.iter().enumerate() {
                    let mut args = v.to_java(env)?;
                    let arg = match args.pop() {
                        Some(JavaArg::Object(o)) => o,
                        _ => {
                            return Err(JavaError::InvalidArgument(
                                "internal error: an object-array element must \
                                 convert to exactly one object reference",
                            ))
                        }
                    };
                    crate::call::with_check(env, |env| arr.set_element(env, i, &arg))?;
                }
                Ok(vec![JavaArg::Object(arr.into())])
            }
        }
    }
    fn java_args(&self) -> String {
        format!("[{}", T::element_sig())
    }
}

impl<T: JavaVecElement> FromJava for Vec<T> {
    fn from_java<'env>(env: &mut Env<'env>, value: JValueOwned<'env>) -> JavaResult<Self> {
        match T::KIND {
            // Primitives: read the whole region with ONE JNI call (bulk
            // `get_region` via `JArray::to_vec_local`) and convert each
            // element through `T::from_java` — the per-element
            // `GetXxxArrayRegion` loop this replaces cost one JNI call per
            // element, which dominated the conversion. `E` is the array
            // element type (== `T` for every primitive `Vec` element).
            VecKind::Primitive(kind) => match value {
                JValueOwned::Object(o) if !o.is_null() => {
                    let out = match kind {
                        ArrayKind::Bool => read_primitive_vec::<bool, T>(env, JValueOwned::Object(o), JValueOwned::Bool)?,
                        ArrayKind::Byte => read_primitive_vec::<i8, T>(env, JValueOwned::Object(o), JValueOwned::Byte)?,
                        ArrayKind::Char => read_primitive_vec::<char, T>(env, JValueOwned::Object(o), |c| JValueOwned::Char(c as u16))?,
                        ArrayKind::Short => read_primitive_vec::<i16, T>(env, JValueOwned::Object(o), JValueOwned::Short)?,
                        ArrayKind::Int => read_primitive_vec::<i32, T>(env, JValueOwned::Object(o), JValueOwned::Int)?,
                        ArrayKind::Long => read_primitive_vec::<i64, T>(env, JValueOwned::Object(o), JValueOwned::Long)?,
                        ArrayKind::Float => read_primitive_vec::<f32, T>(env, JValueOwned::Object(o), JValueOwned::Float)?,
                        ArrayKind::Double => read_primitive_vec::<f64, T>(env, JValueOwned::Object(o), JValueOwned::Double)?,
                    };
                    Ok(out)
                }
                JValueOwned::Object(_) => Err(JavaError::InvalidArgument(
                    "method returned null where an array was expected",
                )),
                _ => Err(JavaError::InvalidArgument(
                    "expected a Java array return value but got a primitive",
                )),
            },
            // `u8` maps to `byte[]` — read as `i8` and convert each element
            // back through `T::from_java` (the bit pattern is preserved).
            VecKind::U8 => {
                let bytes: Vec<i8> = JArray::from_value(env, value)?.to_vec_local(env)?;
                bytes
                    .into_iter()
                    .map(|b| T::from_java(env, JValueOwned::Byte(b)))
                    .collect()
            }
            // Reference types: read each element and convert through
            // `T::from_java`; a `null` element becomes `JValueOwned::Object`
            // of a null reference, so `Option<T>` yields `None` and plain
            // reference types (`String`, `JObject`, …) reject it with
            // `InvalidArgument`.
            VecKind::Object => match value {
                JValueOwned::Object(o) if !o.is_null() => {
                    let arr: JObjectArray<'env> =
                        JObjectArray::<JniObject>::cast_local(env, o)?;
                    let len = crate::call::with_check(env, |env| arr.len(env))?;
                    let mut out = Vec::with_capacity(len);
                    for i in 0..len {
                        let e = crate::call::array_get_object_local(env, &arr, i)?;
                        let elem = match e {
                            Some(o) => JValueOwned::Object(o),
                            None => JValueOwned::Object(JniObject::null()),
                        };
                        out.push(T::from_java(env, elem)?);
                    }
                    Ok(out)
                }
                JValueOwned::Object(_) => Err(JavaError::InvalidArgument(
                    "method returned null where an array was expected",
                )),
                _ => Err(JavaError::InvalidArgument(
                    "expected a Java array return value but got a primitive",
                )),
            },
        }
    }
    fn java_return_type() -> String {
        format!("[{}", T::element_sig())
    }
}

// ---------------------------------------------------------------------------
// Tuples — argument lists, 1..=64 elements (plus the `()` impl above)
// ---------------------------------------------------------------------------

macro_rules! impl_to_java_tuple {
    () => {};
    ($first:ident $(, $rest:ident)*) => {
        impl<$first: ToJava, $($rest: ToJava),*> ToJava for ($first, $($rest),*) {
            #[allow(non_snake_case)]
            fn to_java<'env>(&self, env: &mut Env<'env>) -> JavaResult<Vec<JavaArg<'env>>> {
                let ($first, $($rest),*) = self;
                let mut args = Vec::new();
                args.extend($first.to_java(env)?);
                $(args.extend($rest.to_java(env)?);)*
                Ok(args)
            }
            #[allow(non_snake_case)]
            fn java_args(&self) -> String {
                let ($first, $($rest),*) = self;
                let mut frag = String::new();
                frag.push_str(&$first.java_args());
                $(frag.push_str(&$rest.java_args());)*
                frag
            }
        }
    };
}

impl_to_java_tuple!(A1);
impl_to_java_tuple!(A1, A2);
impl_to_java_tuple!(A1, A2, A3);
impl_to_java_tuple!(A1, A2, A3, A4);
impl_to_java_tuple!(A1, A2, A3, A4, A5);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61, A62);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61, A62, A63);
impl_to_java_tuple!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61, A62, A63, A64);

// ---------------------------------------------------------------------------
// jni glue
// ---------------------------------------------------------------------------

/// Convert an owned [`JavaArg`] into a borrowed [`jni::JValue`].
///
/// The borrow lives exactly as long as the `&JavaArg` given to us; callers
/// keep the argument list alive for the duration of the JNI call.
pub(crate) fn to_jvalue<'a>(arg: &'a JavaArg<'a>) -> JValue<'a> {
    match arg {
        JavaArg::Object(o) => JValue::Object(o),
        JavaArg::Bool(v) => JValue::Bool(*v),
        JavaArg::Byte(v) => JValue::Byte(*v),
        JavaArg::Char(v) => JValue::Char(*v),
        JavaArg::Short(v) => JValue::Short(*v),
        JavaArg::Int(v) => JValue::Int(*v),
        JavaArg::Long(v) => JValue::Long(*v),
        JavaArg::Float(v) => JValue::Float(*v),
        JavaArg::Double(v) => JValue::Double(*v),
    }
}
