//! Java array element types and the JNI plumbing behind [`crate::handles::JArray`].
//!
//! [`JavaArrayElement`] is implemented for the Rust types that can be stored
//! in a *primitive* Java array (`i8`, `i16`, `i32`, `i64`, `f32`, `f64`,
//! `bool`, `char`). Object arrays use [`crate::handles::JObject`] as the
//! element type and are handled by the dedicated `JArray<JObject>` impls.
//!
//! [`JavaVecElement`] is the whole-`Vec<T>` analog: what a Rust `Vec<T>` may
//! convert to/from as a *single* Java array — primitives, `u8`, and
//! reference-typed elements (`String`, `JObject`, `JClass`, `JArray<T>`,
//! `Option<T>`), see the `Vec<T>` conversions in [`crate::convert`].

use jni::objects::{Global, JObject as JniObject, JObjectArray};
use jni::strings::JNIString;
use jni::{Env, JValueOwned};

use crate::convert::{FromJava, JavaArg, ToJava};
use crate::error::{JavaError, JavaResult};
use crate::handles::{JArray, JClass, JObject};

/// The JNI element-kind of a primitive Java array.
///
/// This is an internal dispatch tag; you never construct it yourself.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrayKind {
    Bool,
    Byte,
    Char,
    Short,
    Int,
    Long,
    Float,
    Double,
}

/// A Rust type that can be stored in a primitive Java array.
///
/// Implemented for `i8` (byte[]), `i16` (short[]), `i32` (int[]), `i64`
/// (long[]), `f32` (float[]), `f64` (double[]), `bool` (boolean[]) and
/// `char` (char[]). This trait is sealed: it cannot be implemented outside
/// this crate.
pub trait JavaArrayElement: ToJava + FromJava {
    /// JNI type fragment of one element, e.g. `"I"` for `int`.
    fn element_signature() -> &'static str;

    #[doc(hidden)]
    fn __kind() -> ArrayKind;

    /// The raw JNI value of this element (primitives only).
    #[doc(hidden)]
    fn __as_java_arg(&self) -> JavaResult<JavaArg<'static>>;
}

macro_rules! impl_array_element {
    ($t:ty, $kind:ident, $sig:literal, $variant:ident) => {
        impl JavaArrayElement for $t {
            fn element_signature() -> &'static str {
                $sig
            }
            fn __kind() -> ArrayKind {
                ArrayKind::$kind
            }
            fn __as_java_arg(&self) -> JavaResult<JavaArg<'static>> {
                Ok(JavaArg::$variant(*self))
            }
        }
    };
}

impl_array_element!(bool, Bool, "Z", Bool);
impl_array_element!(i8, Byte, "B", Byte);
impl_array_element!(i16, Short, "S", Short);
impl_array_element!(i32, Int, "I", Int);
impl_array_element!(i64, Long, "J", Long);
impl_array_element!(f32, Float, "F", Float);
impl_array_element!(f64, Double, "D", Double);

// `char` needs a range check before it becomes a Java `char` (u16).
impl JavaArrayElement for char {
    fn element_signature() -> &'static str {
        "C"
    }
    fn __kind() -> ArrayKind {
        ArrayKind::Char
    }
    fn __as_java_arg(&self) -> JavaResult<JavaArg<'static>> {
        let c = jni::char_to_java(*self).map_err(|_| {
            JavaError::InvalidArgument(
                "a char above U+FFFF cannot be represented as a single Java char; \
                 use String instead",
            )
        })?;
        Ok(JavaArg::Char(c))
    }
}

// ---------------------------------------------------------------------------
// JavaVecElement — what `Vec<T>` can convert to/from as a whole
// ---------------------------------------------------------------------------

/// The JNI element-kind of a whole-`Vec<T>` conversion.
///
/// This is an internal dispatch tag; you never construct it yourself.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VecKind {
    /// A primitive array (`int[]`, `double[]`, …) built/filled via the JNI
    /// typed-array APIs ([`new_primitive_array`] / [`fill_primitive_array`]).
    Primitive(ArrayKind),
    /// An object array (`String[]`, `Object[]`, `Class[]`, arrays of arrays),
    /// built via `NewObjectArray` and read element-wise.
    Object,
    /// `u8` → `byte[]` via `i8` bit-pattern casts.
    U8,
}

mod private {
    pub trait SealedVec {}
}

/// A Rust type that can be an element of a Java array converted as a whole
/// `Vec<T>`: primitives (byte[] … double[]), `u8` (byte[] via bit-pattern
/// casts), and reference types (`String[]`, `Object[]`, `Class[]`, arrays of
/// arrays, `Option<T>` for null-tolerant object arrays). Sealed: it cannot
/// be implemented outside this crate.
#[doc(hidden)]
pub trait JavaVecElement: ToJava + FromJava + private::SealedVec {
    /// Which Java array kind this element produces.
    const KIND: VecKind;

    /// JNI type fragment of one element, e.g. `I`, `Ljava/lang/String;`,
    /// `[I` for `JArray<i32>` elements.
    fn element_sig() -> String;

    /// For object arrays: the class name of the elements as passed to
    /// `NewObjectArray` (dotted, slash, or array-descriptor form, e.g.
    /// `java.lang.String`, `[I` for `JArray<i32>` elements). Unused for
    /// primitives/U8.
    fn element_class() -> String;

    /// The raw JNI value of this element for the primitive fill path; a
    /// `None` `Option` element inside a *primitive* array is rejected (only
    /// object arrays can hold `null`).
    #[doc(hidden)]
    fn __as_vec_element_arg(&self) -> JavaResult<JavaArg<'static>>;
}

/// Internal error for [`JavaVecElement::__as_vec_element_arg`]: reached only
/// if an object-typed element is ever fed to the primitive fill path, which
/// the `KIND` dispatch in the `Vec<T>` conversions prevents.
fn not_a_primitive_element() -> JavaError {
    JavaError::InvalidArgument(
        "internal error: an object-array element cannot be stored in a primitive Java array",
    )
}

macro_rules! impl_vec_element {
    ($t:ty, $kind:ident, $sig:literal, $variant:ident) => {
        impl private::SealedVec for $t {}
        impl JavaVecElement for $t {
            const KIND: VecKind = VecKind::Primitive(ArrayKind::$kind);
            fn element_sig() -> String {
                String::from($sig)
            }
            fn element_class() -> String {
                String::from("java.lang.Object")
            }
            fn __as_vec_element_arg(&self) -> JavaResult<JavaArg<'static>> {
                Ok(JavaArg::$variant(*self))
            }
        }
    };
}

impl_vec_element!(bool, Bool, "Z", Bool);
impl_vec_element!(i8, Byte, "B", Byte);
impl_vec_element!(i16, Short, "S", Short);
impl_vec_element!(i32, Int, "I", Int);
impl_vec_element!(i64, Long, "J", Long);
impl_vec_element!(f32, Float, "F", Float);
impl_vec_element!(f64, Double, "D", Double);

// `char` needs a range check before it becomes a Java `char` (u16).
impl private::SealedVec for char {}
impl JavaVecElement for char {
    const KIND: VecKind = VecKind::Primitive(ArrayKind::Char);
    fn element_sig() -> String {
        String::from("C")
    }
    fn element_class() -> String {
        String::from("java.lang.Object")
    }
    fn __as_vec_element_arg(&self) -> JavaResult<JavaArg<'static>> {
        let c = jni::char_to_java(*self).map_err(|_| {
            JavaError::InvalidArgument(
                "a char above U+FFFF cannot be represented as a single Java char; \
                 use String instead",
            )
        })?;
        Ok(JavaArg::Char(c))
    }
}

// `u8` maps to `byte[]` — Java's `byte` is signed, so the values are cast
// through `i8` (the bit pattern is preserved).
impl private::SealedVec for u8 {}
impl JavaVecElement for u8 {
    const KIND: VecKind = VecKind::U8;
    fn element_sig() -> String {
        String::from("B")
    }
    fn element_class() -> String {
        String::from("java.lang.Object")
    }
    fn __as_vec_element_arg(&self) -> JavaResult<JavaArg<'static>> {
        Ok(JavaArg::Byte(*self as i8))
    }
}

impl private::SealedVec for String {}
impl JavaVecElement for String {
    const KIND: VecKind = VecKind::Object;
    fn element_sig() -> String {
        String::from("Ljava/lang/String;")
    }
    fn element_class() -> String {
        String::from("java.lang.String")
    }
    fn __as_vec_element_arg(&self) -> JavaResult<JavaArg<'static>> {
        Err(not_a_primitive_element())
    }
}

impl private::SealedVec for JObject {}
impl JavaVecElement for JObject {
    const KIND: VecKind = VecKind::Object;
    fn element_sig() -> String {
        String::from("Ljava/lang/Object;")
    }
    fn element_class() -> String {
        String::from("java.lang.Object")
    }
    fn __as_vec_element_arg(&self) -> JavaResult<JavaArg<'static>> {
        Err(not_a_primitive_element())
    }
}

impl private::SealedVec for JClass {}
impl JavaVecElement for JClass {
    const KIND: VecKind = VecKind::Object;
    fn element_sig() -> String {
        String::from("Ljava/lang/Class;")
    }
    fn element_class() -> String {
        String::from("java.lang.Class")
    }
    fn __as_vec_element_arg(&self) -> JavaResult<JavaArg<'static>> {
        Err(not_a_primitive_element())
    }
}

// Arrays of arrays: a `JArray<T>` element is itself an array object, so
// `Vec<JArray<i32>>` is `int[][]` — the element class is the array descriptor
// `[I`, which JNI `FindClass` accepts (as does `normalize_class_name`).
impl<T: JavaArrayElement> private::SealedVec for JArray<T> {}
impl<T: JavaArrayElement> JavaVecElement for JArray<T> {
    const KIND: VecKind = VecKind::Object;
    fn element_sig() -> String {
        format!("[{}", T::element_signature())
    }
    fn element_class() -> String {
        format!("[{}", T::element_signature())
    }
    fn __as_vec_element_arg(&self) -> JavaResult<JavaArg<'static>> {
        Err(not_a_primitive_element())
    }
}

// `JArray<JObject>` is `Object[]`, so `Vec<JArray<JObject>>` is `Object[][]`.
impl private::SealedVec for JArray<JObject> {}
impl JavaVecElement for JArray<JObject> {
    const KIND: VecKind = VecKind::Object;
    fn element_sig() -> String {
        String::from("[[Ljava/lang/Object;")
    }
    fn element_class() -> String {
        String::from("[[Ljava/lang/Object;")
    }
    fn __as_vec_element_arg(&self) -> JavaResult<JavaArg<'static>> {
        Err(not_a_primitive_element())
    }
}

// `Option<T>` is a null-tolerance wrapper: the underlying array is `T[]` (so
// `Vec<Option<String>>` is `String[]` with `null` allowed). Inside a
// *primitive* array a `None` element is rejected by `__as_vec_element_arg`.
impl<T: JavaVecElement> private::SealedVec for Option<T> {}
impl<T: JavaVecElement> JavaVecElement for Option<T> {
    const KIND: VecKind = T::KIND;
    fn element_sig() -> String {
        T::element_sig()
    }
    fn element_class() -> String {
        T::element_class()
    }
    fn __as_vec_element_arg(&self) -> JavaResult<JavaArg<'static>> {
        match self {
            Some(v) => v.__as_vec_element_arg(),
            None => Err(JavaError::InvalidArgument(
                "a null element cannot be stored in a primitive Java array",
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers used by `JArray` (handles.rs) and the `Vec<T>` conversions
// ---------------------------------------------------------------------------

type JArrayObj<T> = JArray<T>;

impl JArrayObj<JObject> {
    /// Wrap a `JValueOwned::Object` (a local ref to an array) into an owned
    /// `JArray<JObject>` handle, validating that it really is an object array.
    pub(crate) fn from_value_obj<'env>(
        env: &mut Env<'env>,
        value: JValueOwned<'env>,
    ) -> JavaResult<Self> {
        match value {
            JValueOwned::Object(o) if !o.is_null() => {
                let arr: JObjectArray<'env> = JObjectArray::<JniObject>::cast_local(env, o)?;
                let obj: JniObject = arr.into();
                let global: Global<JniObject<'static>> = env.new_global_ref(obj)?;
                Ok(JArrayObj::from_global_obj(global))
            }
            JValueOwned::Object(_) => Err(JavaError::InvalidArgument(
                "method returned null where an array was expected",
            )),
            _ => Err(JavaError::InvalidArgument(
                "expected a Java array return value but got a primitive",
            )),
        }
    }

    /// Build an `Object[]` from a Rust `Vec<JObject>` as a **local** reference
    /// (used by `JArray::<JObject>::from_vec` / `Java::new_object_array_from`).
    pub(crate) fn from_vec_local_obj<'env>(
        env: &mut Env<'env>,
        values: &[crate::handles::JObject],
    ) -> JavaResult<JniObject<'env>> {
        let arr =
            JObjectArray::<JniObject>::new(env, values.len(), JniObject::null())?;
        for (i, v) in values.iter().enumerate() {
            let local = env.new_local_ref(&*v.global)?;
            arr.set_element(env, i, &local)?;
        }
        Ok(arr.into())
    }

    /// Copy an `Object[]` into a Rust `Vec<JObject>`, erroring on null elements.
    pub(crate) fn to_vec_local_obj<'env>(
        &self,
        env: &mut Env<'env>,
    ) -> JavaResult<Vec<JObject>> {
        let local = env.new_local_ref(&*self.global)?;
        let arr: JObjectArray<'env> = JObjectArray::<JniObject>::cast_local(env, local)?;
        let len = crate::call::array_len_object(env, &self.global)?;
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            let e = crate::call::array_get_object_local(env, &arr, i)?;
            match e {
                Some(o) => out.push(JObject::from_global(env.new_global_ref(o)?)),
                None => {
                    return Err(JavaError::InvalidArgument(
                        "array element is null; use to_vec_options to accept null elements",
                    ))
                }
            }
        }
        Ok(out)
    }
}

impl<T: JavaArrayElement> JArrayObj<T> {
    /// Wrap a `JValueOwned::Object` (a local ref to an array) into an owned
    /// `JArray<T>` handle, validating that it really is an array of `T`.
    pub(crate) fn from_value<'env>(
        env: &mut Env<'env>,
        value: JValueOwned<'env>,
    ) -> JavaResult<Self> {
        match value {
            JValueOwned::Object(o) if !o.is_null() => {
                validate_array_kind(env, &o, T::__kind())?;
                let global: Global<JniObject<'static>> = env.new_global_ref(o)?;
                Ok(JArrayObj::from_global_obj(global))
            }
            JValueOwned::Object(_) => Err(JavaError::InvalidArgument(
                "method returned null where an array was expected",
            )),
            _ => Err(JavaError::InvalidArgument(
                "expected a Java array return value but got a primitive",
            )),
        }
    }

    /// Build a primitive array from a Rust `Vec<T>` as a **local** reference
    /// (used by `ToJava for Vec<T>` and `JArray::from_vec`).
    pub(crate) fn from_vec_local<'env>(
        env: &mut Env<'env>,
        values: &[T],
    ) -> JavaResult<JniObject<'env>> {
        let kind = T::__kind();
        let len = values.len();
        let array = new_primitive_array(env, len, kind)?;
        let mut args: Vec<JavaArg<'static>> = Vec::with_capacity(values.len());
        for v in values { args.push(v.__as_java_arg()?); }
        fill_primitive_array(env, &array, kind, &args)?;
        Ok(array)
    }

    /// Copy a Java array into a Rust `Vec<T>` (used by `FromJava for Vec<T>`
    /// and `JArray::to_vec`).
    pub(crate) fn to_vec_local<'env>(
        &self,
        env: &mut Env<'env>,
    ) -> JavaResult<Vec<T>> {
        let kind = T::__kind();
        let local = env.new_local_ref(&*self.global)?;
        let len = crate::call::array_len_kind(env, &self.global, kind)?;
        match kind {
            ArrayKind::Bool => {
                let arr: jni::objects::JBooleanArray<'env> = jni::objects::JBooleanArray::cast_local(env, local)?;
                let mut buf = vec![false; len];
                arr.get_region(env, 0, &mut buf)?;
                buf.into_iter()
                    .map(|b| T::from_java(env, JValueOwned::Bool(b)))
                    .collect()
            }
            ArrayKind::Byte => {
                let arr: jni::objects::JByteArray<'env> = jni::objects::JByteArray::cast_local(env, local)?;
                let mut buf = vec![0i8; len];
                arr.get_region(env, 0, &mut buf)?;
                buf.into_iter()
                    .map(|b| T::from_java(env, JValueOwned::Byte(b)))
                    .collect()
            }
            ArrayKind::Char => {
                let arr: jni::objects::JCharArray<'env> = jni::objects::JCharArray::cast_local(env, local)?;
                let mut buf = vec![0u16; len];
                arr.get_region(env, 0, &mut buf)?;
                buf.into_iter()
                    .map(|c| T::from_java(env, JValueOwned::Char(c)))
                    .collect()
            }
            ArrayKind::Short => {
                let arr: jni::objects::JShortArray<'env> = jni::objects::JShortArray::cast_local(env, local)?;
                let mut buf = vec![0i16; len];
                arr.get_region(env, 0, &mut buf)?;
                buf.into_iter()
                    .map(|s| T::from_java(env, JValueOwned::Short(s)))
                    .collect()
            }
            ArrayKind::Int => {
                let arr: jni::objects::JIntArray<'env> = jni::objects::JIntArray::cast_local(env, local)?;
                let mut buf = vec![0i32; len];
                arr.get_region(env, 0, &mut buf)?;
                buf.into_iter()
                    .map(|i| T::from_java(env, JValueOwned::Int(i)))
                    .collect()
            }
            ArrayKind::Long => {
                let arr: jni::objects::JLongArray<'env> = jni::objects::JLongArray::cast_local(env, local)?;
                let mut buf = vec![0i64; len];
                arr.get_region(env, 0, &mut buf)?;
                buf.into_iter()
                    .map(|l| T::from_java(env, JValueOwned::Long(l)))
                    .collect()
            }
            ArrayKind::Float => {
                let arr: jni::objects::JFloatArray<'env> = jni::objects::JFloatArray::cast_local(env, local)?;
                let mut buf = vec![0f32; len];
                arr.get_region(env, 0, &mut buf)?;
                buf.into_iter()
                    .map(|f| T::from_java(env, JValueOwned::Float(f)))
                    .collect()
            }
            ArrayKind::Double => {
                let arr: jni::objects::JDoubleArray<'env> = jni::objects::JDoubleArray::cast_local(env, local)?;
                let mut buf = vec![0f64; len];
                arr.get_region(env, 0, &mut buf)?;
                buf.into_iter()
                    .map(|d| T::from_java(env, JValueOwned::Double(d)))
                    .collect()
            }
        }
    }
}

/// Create a primitive array of the given kind (used by `Java::new_array` and
/// the `Vec<T>` conversions).
pub(crate) fn new_primitive_array<'env>(
    env: &mut Env<'env>,
    len: usize,
    kind: ArrayKind,
) -> JavaResult<JniObject<'env>> {
    let arr: JniObject<'env> = match kind {
        ArrayKind::Bool => jni::objects::JBooleanArray::new(env, len)?.into(),
        ArrayKind::Byte => jni::objects::JByteArray::new(env, len)?.into(),
        ArrayKind::Char => jni::objects::JCharArray::new(env, len)?.into(),
        ArrayKind::Short => jni::objects::JShortArray::new(env, len)?.into(),
        ArrayKind::Int => jni::objects::JIntArray::new(env, len)?.into(),
        ArrayKind::Long => jni::objects::JLongArray::new(env, len)?.into(),
        ArrayKind::Float => jni::objects::JFloatArray::new(env, len)?.into(),
        ArrayKind::Double => jni::objects::JDoubleArray::new(env, len)?.into(),
    };
    Ok(arr)
}

/// Fill a freshly-created primitive array from converted element values.
pub(crate) fn fill_primitive_array<'env>(
    env: &mut Env<'env>,
    array: &JniObject<'env>,
    kind: ArrayKind,
    args: &[JavaArg<'static>],
) -> JavaResult<()> {
    macro_rules! fill {
        ($arr:ty, $variant:ident, $expected:literal) => {{
            let arr = env.as_cast::<$arr>(array)?;
            let mut buf = Vec::with_capacity(args.len());
            for arg in args {
                match arg {
                    JavaArg::$variant(v) => buf.push(*v),
                    _ => {
                        return Err(JavaError::InvalidArgument($expected));
                    }
                }
            }
            arr.set_region(env, 0, &buf)?;
            Ok(())
        }};
    }
    match kind {
        ArrayKind::Bool => fill!(jni::objects::JBooleanArray, Bool, "expected all elements to be booleans"),
        ArrayKind::Byte => fill!(jni::objects::JByteArray, Byte, "expected all elements to be bytes"),
        ArrayKind::Char => fill!(jni::objects::JCharArray, Char, "expected all elements to be chars"),
        ArrayKind::Short => fill!(jni::objects::JShortArray, Short, "expected all elements to be shorts"),
        ArrayKind::Int => fill!(jni::objects::JIntArray, Int, "expected all elements to be ints"),
        ArrayKind::Long => fill!(jni::objects::JLongArray, Long, "expected all elements to be longs"),
        ArrayKind::Float => fill!(jni::objects::JFloatArray, Float, "expected all elements to be floats"),
        ArrayKind::Double => fill!(jni::objects::JDoubleArray, Double, "expected all elements to be doubles"),
    }
}

/// Create an object array of element class `class_name` as a **local**
/// reference (used by the `ToJava for Vec<T>` conversion for reference-typed
/// elements). `class_name` may be dotted (`java.lang.String`), slash-separated
/// (`java/lang/String`) or an array descriptor (`[I` — arrays of arrays).
pub(crate) fn new_object_array_local<'env>(
    env: &mut Env<'env>,
    class_name: &str,
    len: usize,
) -> JavaResult<JObjectArray<'env>> {
    let name = if class_name.contains('/') {
        class_name.to_string()
    } else {
        class_name.replace('.', "/")
    };
    let cls = crate::call::find_class(env, JNIString::from(name))?;
    let arr = env
        .new_object_array(len as i32, &cls, JniObject::null())
        .map_err(JavaError::from)?;
    Ok(arr)
}

/// Runtime `IsInstanceOf` check that a local reference is an array of `kind`.
pub(crate) fn validate_array_kind(env: &mut Env<'_>, obj: &JniObject<'_>, kind: ArrayKind) -> JavaResult<()> {
    // `as_cast` performs the runtime check without consuming `obj`.
    let result: JavaResult<()> = match kind {
        ArrayKind::Bool => env.as_cast::<jni::objects::JBooleanArray>(obj).map(|_| ()).map_err(JavaError::from),
        ArrayKind::Byte => env.as_cast::<jni::objects::JByteArray>(obj).map(|_| ()).map_err(JavaError::from),
        ArrayKind::Char => env.as_cast::<jni::objects::JCharArray>(obj).map(|_| ()).map_err(JavaError::from),
        ArrayKind::Short => env.as_cast::<jni::objects::JShortArray>(obj).map(|_| ()).map_err(JavaError::from),
        ArrayKind::Int => env.as_cast::<jni::objects::JIntArray>(obj).map(|_| ()).map_err(JavaError::from),
        ArrayKind::Long => env.as_cast::<jni::objects::JLongArray>(obj).map(|_| ()).map_err(JavaError::from),
        ArrayKind::Float => env.as_cast::<jni::objects::JFloatArray>(obj).map(|_| ()).map_err(JavaError::from),
        ArrayKind::Double => env.as_cast::<jni::objects::JDoubleArray>(obj).map(|_| ()).map_err(JavaError::from),
    };
    result.map_err(|_| {
        JavaError::InvalidArgument(
            "the returned Java object is not an array of the annotated element type",
        )
    })
}
