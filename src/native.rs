//! Native-method machinery: Rust functions callable from Java — the mlua
//! `Lua::create_function` analog, implemented over JNI `RegisterNatives`.
//!
//! The user-facing entry points are the [`crate::native!`] and
//! [`crate::native_inst!`] macros (proc macros from the
//! `rjava-macros` crate, re-exported at the crate root) plus
//! [`crate::JClass::register_natives`]. There are two forms:
//!
//! * **Type-derived** (primary): `native!("add", f)` — no signature string.
//!   `make_native_static` / `make_native_inst` derive the JNI descriptor
//!   at runtime from the Rust types and fix the C ABI at compile time with
//!   the shared generic trampolines below (one instantiation per `(A, R)`
//!   signature). Closures **and** fn items work. Two *different*
//!   registrations with the same `(A, R)` share one trampoline, which
//!   [`crate::JClass::register_natives`] detects and rejects — the
//!   explicit-signature form is the escape hatch.
//! * **Explicit-signature** (escape hatch): `native!("add", "(II)I", f)` —
//!   the macros generate a unique `extern "system"` trampoline per
//!   registration.
//!
//! **Parameter limits**: the type-derived form is generated for argument
//! tuples of up to 64 elements — at most 64 declared parameters for a static
//! native, 63 for an instance native (the receiver `JObject` takes the 64th
//! tuple slot). The explicit-signature form accepts any signature within the
//! JVM's own method-descriptor limit of 255 parameter units (JVMS §4.3.3:
//! `long`/`double` count as two units, everything else as one; an instance
//! method's implicit `this` consumes one of the 255, leaving 254). Real-world
//! methods rarely exceed ~20 parameters.
//!
//! In both forms the trampoline converts the raw JNI arguments into
//! [`crate::JavaArg`]s and hands them to [`dispatch`](crate::native::dispatch), which runs the user's Rust function. Panics are
//! caught and converted into Java `RuntimeException`s; `Err(…)` results are
//! thrown as Java exceptions too, so nothing unwinds across the FFI boundary.
//!
//! Everything in this module is safe, and so is the code the
//! [`crate::native!`] / [`crate::native_inst!`] macros generate: the only
//! `unsafe` in the feature (the two JNI calls behind `RegisterNatives`)
//! lives in the `rjava-helper` crate's `register_natives` helper.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, LazyLock, Mutex};

use jni::signature::RuntimeMethodSignature;
use jni::strings::JNIString;
use jni::{Env, JValueOwned};

use crate::array::{JavaArrayElement, JavaVecElement};
use crate::call;
use crate::convert::{FromJava, JavaArg, ToJava};
use crate::error::{JavaError, JavaResult};
use crate::handles::{JArray, JClass, JObject};

// ---------------------------------------------------------------------------
// NativeMethod
// ---------------------------------------------------------------------------

/// A JNI native-method descriptor: a Java method name, its JNI signature, and
/// the address of a trampoline whose C signature matches that signature.
///
/// Constructed by the [`crate::native!`] / [`crate::native_inst!`] macros and
/// passed to [`crate::JClass::register_natives`]. You never construct one by
/// hand.
pub struct NativeMethod {
    // The `name`/`sig`/`fn_ptr` fields are `pub(crate)` so the async-native
    // machinery in `crate::future` (`make_async_native`) can construct
    // descriptors for its own shared trampolines — same crate, private API.
    pub(crate) name: String,
    pub(crate) sig: String,
    pub(crate) fn_ptr: *mut std::ffi::c_void,
    /// The registered callable for **type-derived** descriptors (built by
    /// `make_native_static` / `make_native_inst`). `None` for the
    /// explicit-signature form (`native!` with a signature string), whose
    /// trampoline is unique per registration and carries no shared state.
    ///
    /// `Some(…)` is what lets [`crate::JClass::register_natives`] detect two
    /// descriptors sharing one trampoline before the JVM sees them.
    #[doc(hidden)]
    pub call: Option<Arc<dyn NativeCall>>,
}

impl NativeMethod {
    /// Create a native-method descriptor for the Java method `name` with the
    /// JNI signature `sig` (`"(II)I"`, `"()V"`, `"([D)D"`, …), backed by the
    /// trampoline at `fn_ptr`.
    ///
    /// The signature is validated with the `jni` crate's parser; garbage is
    /// rejected with [`JavaError::InvalidArgument`]. `fn_ptr` must point to a
    /// function whose C parameter and return types are exactly the JNI types
    /// for `sig` — the `native!`/`native_inst!` macros generate such
    /// trampolines (they parse `sig` at compile time), so this requirement is
    /// met for every descriptor they produce.
    pub fn new(
        name: impl Into<String>,
        sig: impl Into<String>,
        fn_ptr: *mut std::ffi::c_void,
    ) -> JavaResult<Self> {
        let name = name.into();
        let sig = sig.into();
        RuntimeMethodSignature::from_str(&sig).map_err(|_| {
            JavaError::InvalidArgument(
                "native method: invalid JNI signature (expected e.g. \"(II)I\")",
            )
        })?;
        Ok(NativeMethod {
            name,
            sig,
            fn_ptr,
            call: None,
        })
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn sig(&self) -> &str {
        &self.sig
    }

    pub(crate) fn fn_ptr(&self) -> *mut std::ffi::c_void {
        self.fn_ptr
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// The function signature of a native-method implementation (see
/// [`NativeFn`]); implemented for anything callable with an `Env` and an
/// argument tuple — fn items *and* closures — so a registered trampoline can
/// be invoked any number of times.
#[doc(hidden)]
pub trait NativeFn<A, R> {
    /// Call the function with the attached `env` and the converted arguments.
    fn call<'env>(self, env: &mut Env<'env>, args: A) -> JavaResult<R>;
}

impl<F, A, R> NativeFn<A, R> for F
where
    F: for<'a, 'env> Fn(&'a mut Env<'env>, A) -> JavaResult<R>,
{
    fn call<'env>(self, env: &mut Env<'env>, args: A) -> JavaResult<R> {
        self(env, args)
    }
}

/// A Rust argument tuple for a native method; implemented for `()` and tuples
/// `(T1,)` … `(T1, …, T64)` with [`FromJava`] element types.
#[doc(hidden)]
pub trait NativeArgs<'env>: Sized {
    /// Number of elements in the tuple.
    const ARITY: usize;

    /// Convert the converted JNI arguments (already arity-checked by
    /// [`dispatch`]) into this tuple, consuming them.
    fn from_args(env: &mut Env<'env>, args: Vec<JavaArg<'env>>) -> JavaResult<Self>;
}

/// A single argument type of a **type-derived** native method (the
/// `native!("name", f)` form): the JNI descriptor fragment is fixed at
/// runtime via the [`ToJava`] machinery, while the C type the JVM passes is
/// fixed at compile time via the [`Self::CType`] GAT.
///
/// Sealed: implemented for exactly the single-value [`FromJava`] type set, so
/// the [`dispatch`] conversion (which runs `FromJava`) always works for the
/// values the trampoline builds.
#[doc(hidden)]
pub trait NativeArg: ToJava + private::SealedArg {
    /// The C type this argument arrives as in the trampoline's `extern`
    /// signature.
    type CType<'c>;

    /// Wrap the raw C value into an owned [`JavaArg`] for [`dispatch`].
    fn arg_to_java<'c>(v: Self::CType<'c>) -> JavaArg<'c>;

    /// The JNI type fragment for this argument type (no parentheses).
    fn arg_sig() -> String;
}

/// Marker for types that may be returned from a native method: the
/// single-value [`ToJava`] types. Sealed so only the types listed below
/// qualify (tuples, which produce multiple JNI values, are deliberately
/// excluded).
///
/// For the **type-derived** form this trait also fixes the C return type at
/// compile time ([`Self::CRet`]) and converts the dispatched owned value into
/// it; the explicit-signature form keeps using its own macro-side conversion
/// and only relies on the sealed marker (via [`dispatch`]'s bound).
#[doc(hidden)]
pub trait NativeReturn: ToJava + private::SealedRet {
    /// The C return type of the trampoline.
    type CRet<'c>;

    /// Convert the dispatched owned value into the C return type; a
    /// mismatched variant throws [`JavaError::InvalidArgument`] and returns
    /// [`Self::ret_default`] (mirroring the explicit form's `ret_conv`).
    fn ret_to_c<'c>(env: &mut Env<'c>, v: JValueOwned<'c>) -> Self::CRet<'c>;

    /// The C default value, returned when the call fails or panics.
    fn ret_default<'c>() -> Self::CRet<'c>;
}

mod private {
    pub trait SealedArg {}
    pub trait SealedRet {}
}

impl private::SealedRet for () {}
impl private::SealedRet for String {}
impl private::SealedRet for JObject {}
impl private::SealedRet for JClass {}
impl<T: JavaArrayElement> private::SealedRet for JArray<T> {}
impl private::SealedRet for JArray<JObject> {}
impl<T: JavaVecElement> private::SealedRet for Vec<T> {}
impl<T: NativeReturn> private::SealedRet for Option<T> {}

impl private::SealedArg for String {}
impl private::SealedArg for JObject {}
impl private::SealedArg for JClass {}
impl<T: JavaArrayElement> private::SealedArg for JArray<T> {}
impl private::SealedArg for JArray<JObject> {}
impl<T: JavaVecElement> private::SealedArg for Vec<T> {}
impl<T: NativeArg> private::SealedArg for Option<T> {}

impl NativeReturn for () {
    type CRet<'c> = ();
    fn ret_to_c<'c>(env: &mut Env<'c>, v: JValueOwned<'c>) -> Self::CRet<'c> {
        match v {
            JValueOwned::Void => {}
            _ => {
                let _ = throw_error(env, JavaError::InvalidArgument(MISMATCHED_RETURN));
            }
        }
    }
    fn ret_default<'c>() -> Self::CRet<'c> {}
}

/// The error message used when a dispatched value does not match the JNI
/// return type (shared by [`NativeReturn::ret_to_c`] and the explicit form's
/// macro-side `ret_conv`).
const MISMATCHED_RETURN: &str =
    "native method returned a Java value that does not match its JNI signature";

macro_rules! impl_native_prim {
    ($t:ty, $variant:ident, $ctype:ty, $sig:literal, $default:expr) => {
        impl private::SealedArg for $t {}
        impl private::SealedRet for $t {}
        impl NativeArg for $t {
            type CType<'c> = $ctype;
            fn arg_to_java<'c>(v: Self::CType<'c>) -> JavaArg<'c> {
                JavaArg::$variant(v)
            }
            fn arg_sig() -> String {
                String::from($sig)
            }
        }
        impl NativeReturn for $t {
            type CRet<'c> = $ctype;
            fn ret_to_c<'c>(env: &mut Env<'c>, v: JValueOwned<'c>) -> Self::CRet<'c> {
                match v {
                    JValueOwned::$variant(__v) => __v,
                    _ => {
                        let _ = throw_error(env, JavaError::InvalidArgument(MISMATCHED_RETURN));
                        $default
                    }
                }
            }
            fn ret_default<'c>() -> Self::CRet<'c> {
                $default
            }
        }
    };
}

impl_native_prim!(bool, Bool, ::jni::sys::jboolean, "Z", false);
impl_native_prim!(i8, Byte, ::jni::sys::jbyte, "B", 0);
impl_native_prim!(i16, Short, ::jni::sys::jshort, "S", 0);
impl_native_prim!(i32, Int, ::jni::sys::jint, "I", 0);
impl_native_prim!(i64, Long, ::jni::sys::jlong, "J", 0);
impl_native_prim!(f32, Float, ::jni::sys::jfloat, "F", 0.0);
impl_native_prim!(f64, Double, ::jni::sys::jdouble, "D", 0.0);
impl_native_prim!(char, Char, ::jni::sys::jchar, "C", 0);

// Reference types: the JVM passes (and expects) an object reference, which
// jni's `JObject` represents as a `repr(transparent)` wrapper over `jobject`.
macro_rules! impl_native_ref {
    ($t:ty, $sig:expr) => {
        impl NativeArg for $t {
            type CType<'c> = ::jni::objects::JObject<'c>;
            fn arg_to_java<'c>(v: Self::CType<'c>) -> JavaArg<'c> {
                JavaArg::Object(v)
            }
            fn arg_sig() -> String {
                String::from($sig)
            }
        }
        impl NativeReturn for $t {
            type CRet<'c> = ::jni::objects::JObject<'c>;
            fn ret_to_c<'c>(env: &mut Env<'c>, v: JValueOwned<'c>) -> Self::CRet<'c> {
                match v {
                    JValueOwned::Object(__v) => __v,
                    _ => {
                        let _ = throw_error(env, JavaError::InvalidArgument(MISMATCHED_RETURN));
                        ::jni::objects::JObject::null()
                    }
                }
            }
            fn ret_default<'c>() -> Self::CRet<'c> {
                ::jni::objects::JObject::null()
            }
        }
    };
}

impl_native_ref!(String, "Ljava/lang/String;");
impl_native_ref!(JObject, "Ljava/lang/Object;");
impl_native_ref!(JClass, "Ljava/lang/Class;");
impl_native_ref!(JArray<JObject>, "[Ljava/lang/Object;");

impl<T: JavaArrayElement> NativeArg for JArray<T> {
    type CType<'c> = ::jni::objects::JObject<'c>;
    fn arg_to_java<'c>(v: Self::CType<'c>) -> JavaArg<'c> {
        JavaArg::Object(v)
    }
    fn arg_sig() -> String {
        format!("[{}", T::element_signature())
    }
}
impl<T: JavaArrayElement> NativeReturn for JArray<T> {
    type CRet<'c> = ::jni::objects::JObject<'c>;
    fn ret_to_c<'c>(env: &mut Env<'c>, v: JValueOwned<'c>) -> Self::CRet<'c> {
        match v {
            JValueOwned::Object(__v) => __v,
            _ => {
                let _ = throw_error(env, JavaError::InvalidArgument(MISMATCHED_RETURN));
                ::jni::objects::JObject::null()
            }
        }
    }
    fn ret_default<'c>() -> Self::CRet<'c> {
        ::jni::objects::JObject::null()
    }
}
impl<T: JavaVecElement> NativeArg for Vec<T> {
    type CType<'c> = ::jni::objects::JObject<'c>;
    fn arg_to_java<'c>(v: Self::CType<'c>) -> JavaArg<'c> {
        JavaArg::Object(v)
    }
    fn arg_sig() -> String {
        format!("[{}", T::element_sig())
    }
}
impl<T: JavaVecElement> NativeReturn for Vec<T> {
    type CRet<'c> = ::jni::objects::JObject<'c>;
    fn ret_to_c<'c>(env: &mut Env<'c>, v: JValueOwned<'c>) -> Self::CRet<'c> {
        match v {
            JValueOwned::Object(__v) => __v,
            _ => {
                let _ = throw_error(env, JavaError::InvalidArgument(MISMATCHED_RETURN));
                ::jni::objects::JObject::null()
            }
        }
    }
    fn ret_default<'c>() -> Self::CRet<'c> {
        ::jni::objects::JObject::null()
    }
}
impl<T: NativeArg> NativeArg for Option<T> {
    type CType<'c> = ::jni::objects::JObject<'c>;
    fn arg_to_java<'c>(v: Self::CType<'c>) -> JavaArg<'c> {
        JavaArg::Object(v)
    }
    fn arg_sig() -> String {
        T::arg_sig()
    }
}
impl<T: NativeReturn> NativeReturn for Option<T> {
    type CRet<'c> = ::jni::objects::JObject<'c>;
    fn ret_to_c<'c>(env: &mut Env<'c>, v: JValueOwned<'c>) -> Self::CRet<'c> {
        match v {
            JValueOwned::Object(__v) => __v,
            _ => {
                let _ = throw_error(env, JavaError::InvalidArgument(MISMATCHED_RETURN));
                ::jni::objects::JObject::null()
            }
        }
    }
    fn ret_default<'c>() -> Self::CRet<'c> {
        ::jni::objects::JObject::null()
    }
}

/// Run the user's Rust function with the converted JNI arguments and turn the
/// result into a single owned Java value.
///
/// This is the safe heart of the native-method feature, called from the
/// trampolines that [`crate::native!`] / [`crate::native_inst!`] generate:
///
/// 1. the tuple arity of `A` is checked against the number of converted
///    arguments (a mismatch means the Rust function's signature does not match
///    the JNI signature, which is the most common mistake),
/// 2. the arguments are converted into `A` via [`NativeArgs`],
/// 3. the function is called inside [`catch_unwind`] — a panic is turned into
///    a `JavaError` carrying a `java.lang.RuntimeException` payload so it can
///    be thrown into Java instead of crossing the FFI boundary,
/// 4. on success the return value is converted with [`ToJava`] and must yield
///    exactly zero or one JNI value (zero → `Void`).
///
/// Errors are *returned*, not thrown — the trampoline calls [`throw_error`]
/// with them and returns the signature's default value.
#[doc(hidden)]
pub fn dispatch<'env, F, A, R>(
    env: &mut Env<'env>,
    args: Vec<JavaArg<'env>>,
    f: F,
) -> JavaResult<JValueOwned<'env>>
where
    F: NativeFn<A, R>,
    A: NativeArgs<'env>,
    R: NativeReturn,
{
    if A::ARITY != args.len() {
        return Err(JavaError::InvalidArgument(
            "native method: the Rust function takes a different number of \
             arguments than its JNI signature declares (arity mismatch)",
        ));
    }
    let a = A::from_args(env, args)?;
    // The closure only reborrows `env`, so the borrow ends when it returns
    // and `env` stays usable below for the return-value conversion.
    let r = catch_unwind(AssertUnwindSafe(|| f.call(&mut *env, a))).map_err(|payload| {
        JavaError::JavaException {
            class: "java.lang.RuntimeException".to_string(),
            message: format!("native method panicked: {}", panic_message(&payload)),
        }
    })??;
    let mut out = r.to_java(env)?;
    match out.len() {
        0 => Ok(JValueOwned::Void),
        1 => Ok(java_arg_to_owned(out.pop().expect("len == 1"))),
        _ => Err(JavaError::InvalidArgument(
            "native method: the Rust function returned more than one Java value",
        )),
    }
}

/// The human-readable payload of a caught panic.
///
/// Panics may be caught and re-thrown (`resume_unwind`) by nested
/// `catch_unwind` wrappers (e.g. jni's local-frame helpers), which re-boxes
/// the payload as `Box<dyn Any + Send>`; unwrap those recursively.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(b) = payload.downcast_ref::<Box<dyn std::any::Any + Send>>() {
        panic_message(b.as_ref())
    } else {
        "unknown panic payload".to_string()
    }
}

// ---------------------------------------------------------------------------
// Throwing
// ---------------------------------------------------------------------------

/// Throw `err` into Java as a pending exception, leaving it pending so the
/// JVM delivers it when the native method returns.
///
/// * [`JavaError::JavaException`]`{ class, message }` → throw an instance of
///   `class` with `message` (the dotted name is converted to JNI form);
/// * any other error → throw a `java.lang.RuntimeException` carrying the
///   `Debug` rendering of the error (this is how panics and arity mismatches
///   surface on the Java side).
///
/// Errors from the throw itself are returned; the pending exception is *not*
/// cleared (unlike [`crate::call::with_check`], which is exactly what we must
/// *not* do here).
#[doc(hidden)]
pub fn throw_error(env: &mut Env<'_>, err: JavaError) -> JavaResult<()> {
    let (class, message) = match err {
        JavaError::JavaException { class, message } => (class, message),
        other => (
            "java.lang.RuntimeException".to_string(),
            format!("{other:?}"),
        ),
    };
    let message = JNIString::from(message);
    let class = JNIString::from(class.replace('.', "/"));
    let cls = match call::find_class(env, class.clone()) {
        Ok(cls) => cls,
        Err(_) => call::find_class(env, JNIString::from("java/lang/RuntimeException"))?,
    };
    env.throw_new(cls, message).map_err(JavaError::from)
}

// ---------------------------------------------------------------------------
// NativeArgs impls (tuples, 0..=64 elements)
// ---------------------------------------------------------------------------

macro_rules! count_args {
    () => { 0usize };
    // Non-recursive: the number of identifiers is the array length (a const
    // fn), keeping macro expansion depth bounded at 64-deep @peel + 2 even
    // for the largest generated arity.
    ($($t:ident),+) => { [$(count_args!(@unit $t)),+].len() };
    (@unit $_t:ident) => { 1usize };
}

macro_rules! impl_native_args {
    () => {
        impl<'env> NativeArgs<'env> for () {
            const ARITY: usize = 0;
            fn from_args(_env: &mut Env<'env>, _args: Vec<JavaArg<'env>>) -> JavaResult<Self> {
                Ok(())
            }
        }
    };
    ($($t:ident),+) => {
        impl_native_args!(@peel [$($t),*] [] [] __env; __args; __it);
    };
    // Last element: emit the impl. `__env`/`__args`/`__it` are spliced tokens
    // from the top-level arm so they refer to the fn's own parameters and
    // locals (macro_rules hygiene otherwise resolves them at the recursion
    // site). The iterator is consumed sequentially; dispatch already checked
    // the arity, so `next()` cannot return `None`.
    (@peel [$t:ident] [$($ty:ident)*] [$($acc:tt)*] $envvar:tt; $argsvar:tt; $itvar:tt) => {
        impl<'env, $($ty: FromJava,)* $t: FromJava> NativeArgs<'env> for ($($ty,)* $t,) {
            const ARITY: usize = count_args!($($ty,)* $t);
            #[allow(non_snake_case)]
            fn from_args($envvar: &mut Env<'env>, $argsvar: Vec<JavaArg<'env>>) -> JavaResult<Self> {
                let mut $itvar = $argsvar.into_iter();
                Ok(($($acc)* $t::from_java(
                    $envvar,
                    java_arg_to_owned($itvar.next().expect("arity checked by dispatch")),
                )?,))
            }
        }
    };
    (@peel [$t:ident, $($rest:ident),*] [$($ty:ident)*] [$($acc:tt)*] $envvar:tt; $argsvar:tt; $itvar:tt) => {
        impl_native_args!(@peel [$($rest),*] [$($ty)* $t] [$($acc)* $t::from_java(
            $envvar,
            java_arg_to_owned($itvar.next().expect("arity checked by dispatch")),
        )?,] $envvar; $argsvar; $itvar);
    };
}

impl_native_args!();
impl_native_args!(A1);
impl_native_args!(A1, A2);
impl_native_args!(A1, A2, A3);
impl_native_args!(A1, A2, A3, A4);
impl_native_args!(A1, A2, A3, A4, A5);
impl_native_args!(A1, A2, A3, A4, A5, A6);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61, A62);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61, A62, A63);
impl_native_args!(A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61, A62, A63, A64);

// ---------------------------------------------------------------------------
// Owned JavaArg → JValueOwned conversion (used by dispatch and NativeArgs)
// ---------------------------------------------------------------------------

/// Convert an owned [`JavaArg`] into an owned [`JValueOwned`] (1:1 variant
/// mapping; object references pass through).
fn java_arg_to_owned(arg: JavaArg<'_>) -> JValueOwned<'_> {
    match arg {
        JavaArg::Object(o) => JValueOwned::Object(o),
        JavaArg::Bool(v) => JValueOwned::Bool(v),
        JavaArg::Byte(v) => JValueOwned::Byte(v),
        JavaArg::Char(v) => JValueOwned::Char(v),
        JavaArg::Short(v) => JValueOwned::Short(v),
        JavaArg::Int(v) => JValueOwned::Int(v),
        JavaArg::Long(v) => JValueOwned::Long(v),
        JavaArg::Float(v) => JValueOwned::Float(v),
        JavaArg::Double(v) => JValueOwned::Double(v),
    }
}

// ---------------------------------------------------------------------------
// Type-derived native methods: shared trampolines + registry
//
// The explicit-signature form generates a *unique* trampoline per `native!`
// call. The type-derived form (`native!("name", f)` — no signature string)
// cannot: the C signature is fixed at compile time by the generic extern
// trampolines below, one instantiation per `(A, R)` type combination, shared
// by every registration with that signature. The user's callable therefore
// lives in a process-global registry keyed by the trampoline's own address,
// and two *different* registrations with the same `(A, R)` are detected and
// rejected at `register_natives` time (see `JClass::register_natives`).
// ---------------------------------------------------------------------------

/// A registered callable behind a shared trampoline.
#[doc(hidden)]
pub trait NativeCall: Send + Sync {
    /// Run the user's function with the converted JNI arguments and turn the
    /// result into a single owned Java value (the [`dispatch`] logic).
    fn call<'env>(
        &self,
        env: &mut Env<'env>,
        args: Vec<JavaArg<'env>>,
    ) -> JavaResult<JValueOwned<'env>>;
}

// Process-global registry: trampoline address → registered callable. Entries
// live for the process lifetime; a given address holds at most the *last*
// callable inserted for it, which is exactly what makes two same-signature
// derived registrations detectable (the first registration would otherwise
// silently run the second function).
static NATIVE_REGISTRY: LazyLock<Mutex<HashMap<usize, Arc<dyn NativeCall>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Store `call` under the trampoline address `addr` (see [`NATIVE_REGISTRY`]).
pub(crate) fn registry_insert(addr: usize, call: Arc<dyn NativeCall>) {
    NATIVE_REGISTRY
        .lock()
        .expect("native registry lock poisoned")
        .insert(addr, call);
}

/// Look up the registered callable for trampoline address `addr`.
pub(crate) fn registry_get(addr: usize) -> Option<Arc<dyn NativeCall>> {
    NATIVE_REGISTRY
        .lock()
        .expect("native registry lock poisoned")
        .get(&addr)
        .cloned()
}

/// Per-arity generic `extern "system"` trampolines, generated below. Each
/// instantiation's C signature is the exact JNI ABI for its `(A, R)` types:
/// the parameter C types come from [`NativeArg::CType`], the return type from
/// [`NativeReturn::CRet`] — both sealed, so only the FFI-safe monomorphizations
/// listed above exist. `#[allow(improper_ctypes)]` is justified by that
/// sealing: every reachable instantiation has `jni::sys` primitive or
/// `repr(transparent)`-over-`jobject` parameters and a like return type.
macro_rules! impl_trampolines {
    ($static:ident, $inst:ident, $($A:ident),*) => {
        #[doc(hidden)]
        #[allow(improper_ctypes, non_snake_case)]
        pub extern "system" fn $static<'caller, $($A: NativeArg,)* R: NativeReturn>(
            mut __env: ::jni::EnvUnowned<'caller>,
            _receiver: ::jni::objects::JClass<'caller>,
            $($A: <$A as NativeArg>::CType<'caller>,)*
        ) -> <R as NativeReturn>::CRet<'caller> {
            let __key = ($static::<$($A,)* R> as for<'c> extern "system" fn(
                ::jni::EnvUnowned<'c>,
                ::jni::objects::JClass<'c>,
                $(<$A as NativeArg>::CType<'c>,)*
            ) -> <R as NativeReturn>::CRet<'c>) as usize;
            match __env.with_env(
                |__env| -> ::std::result::Result<<R as NativeReturn>::CRet<'caller>, ::jni::errors::Error> {
                    let __arc = ::std::sync::Arc::clone(&registry_get(__key).expect(
                        "rjava: native method not registered (was the NativeMethod built by native!?)",
                    ));
                    let __args: ::std::vec::Vec<JavaArg<'caller>> = ::std::vec![
                        $(<$A as NativeArg>::arg_to_java($A),)*
                    ];
                    match __arc.call(__env, __args) {
                        ::std::result::Result::Ok(__v) => {
                            ::std::result::Result::Ok(<R as NativeReturn>::ret_to_c(__env, __v))
                        }
                        ::std::result::Result::Err(__e) => {
                            let _ = throw_error(__env, __e);
                            ::std::result::Result::Ok(<R as NativeReturn>::ret_default())
                        }
                    }
                },
            ).into_outcome() {
                ::jni::Outcome::Ok(__v) => __v,
                // The non-`Ok` arms (its own catch_unwind already covers the
                // panics of `dispatch`) are rare; returning the signature's
                // default value is acceptable — the JVM observes a default.
                _ => <R as NativeReturn>::ret_default(),
            }
        }

        #[doc(hidden)]
        #[allow(improper_ctypes, non_snake_case)]
        pub extern "system" fn $inst<'caller, $($A: NativeArg,)* R: NativeReturn>(
            mut __env: ::jni::EnvUnowned<'caller>,
            receiver: ::jni::objects::JObject<'caller>,
            $($A: <$A as NativeArg>::CType<'caller>,)*
        ) -> <R as NativeReturn>::CRet<'caller> {
            let __key = ($inst::<$($A,)* R> as for<'c> extern "system" fn(
                ::jni::EnvUnowned<'c>,
                ::jni::objects::JObject<'c>,
                $(<$A as NativeArg>::CType<'c>,)*
            ) -> <R as NativeReturn>::CRet<'c>) as usize;
            match __env.with_env(
                |__env| -> ::std::result::Result<<R as NativeReturn>::CRet<'caller>, ::jni::errors::Error> {
                    let __arc = ::std::sync::Arc::clone(&registry_get(__key).expect(
                        "rjava: native method not registered (was the NativeMethod built by native!?)",
                    ));
                    let __args: ::std::vec::Vec<JavaArg<'caller>> = ::std::vec![
                        JavaArg::Object(receiver),
                        $(<$A as NativeArg>::arg_to_java($A),)*
                    ];
                    match __arc.call(__env, __args) {
                        ::std::result::Result::Ok(__v) => {
                            ::std::result::Result::Ok(<R as NativeReturn>::ret_to_c(__env, __v))
                        }
                        ::std::result::Result::Err(__e) => {
                            let _ = throw_error(__env, __e);
                            ::std::result::Result::Ok(<R as NativeReturn>::ret_default())
                        }
                    }
                },
            ).into_outcome() {
                ::jni::Outcome::Ok(__v) => __v,
                _ => <R as NativeReturn>::ret_default(),
            }
        }
    };
}

impl_trampolines!(tramp_static_0, tramp_inst_0,);
impl_trampolines!(tramp_static_1, tramp_inst_1, A1);
impl_trampolines!(tramp_static_2, tramp_inst_2, A1, A2);
impl_trampolines!(tramp_static_3, tramp_inst_3, A1, A2, A3);
impl_trampolines!(tramp_static_4, tramp_inst_4, A1, A2, A3, A4);
impl_trampolines!(tramp_static_5, tramp_inst_5, A1, A2, A3, A4, A5);
impl_trampolines!(tramp_static_6, tramp_inst_6, A1, A2, A3, A4, A5, A6);
impl_trampolines!(tramp_static_7, tramp_inst_7, A1, A2, A3, A4, A5, A6, A7);
impl_trampolines!(tramp_static_8, tramp_inst_8, A1, A2, A3, A4, A5, A6, A7, A8);
impl_trampolines!(tramp_static_9, tramp_inst_9, A1, A2, A3, A4, A5, A6, A7, A8, A9);
impl_trampolines!(tramp_static_10, tramp_inst_10, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10);
impl_trampolines!(tramp_static_11, tramp_inst_11, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11);
impl_trampolines!(tramp_static_12, tramp_inst_12, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12);
impl_trampolines!(tramp_static_13, tramp_inst_13, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13);
impl_trampolines!(tramp_static_14, tramp_inst_14, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14);
impl_trampolines!(tramp_static_15, tramp_inst_15, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15);
impl_trampolines!(tramp_static_16, tramp_inst_16, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16);
impl_trampolines!(tramp_static_17, tramp_inst_17, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17);
impl_trampolines!(tramp_static_18, tramp_inst_18, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18);
impl_trampolines!(tramp_static_19, tramp_inst_19, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19);
impl_trampolines!(tramp_static_20, tramp_inst_20, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20);
impl_trampolines!(tramp_static_21, tramp_inst_21, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21);
impl_trampolines!(tramp_static_22, tramp_inst_22, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22);
impl_trampolines!(tramp_static_23, tramp_inst_23, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23);
impl_trampolines!(tramp_static_24, tramp_inst_24, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24);
impl_trampolines!(tramp_static_25, tramp_inst_25, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25);
impl_trampolines!(tramp_static_26, tramp_inst_26, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26);
impl_trampolines!(tramp_static_27, tramp_inst_27, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27);
impl_trampolines!(tramp_static_28, tramp_inst_28, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28);
impl_trampolines!(tramp_static_29, tramp_inst_29, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29);
impl_trampolines!(tramp_static_30, tramp_inst_30, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30);
impl_trampolines!(tramp_static_31, tramp_inst_31, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31);
impl_trampolines!(tramp_static_32, tramp_inst_32, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32);
impl_trampolines!(tramp_static_33, tramp_inst_33, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33);
impl_trampolines!(tramp_static_34, tramp_inst_34, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34);
impl_trampolines!(tramp_static_35, tramp_inst_35, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35);
impl_trampolines!(tramp_static_36, tramp_inst_36, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36);
impl_trampolines!(tramp_static_37, tramp_inst_37, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37);
impl_trampolines!(tramp_static_38, tramp_inst_38, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38);
impl_trampolines!(tramp_static_39, tramp_inst_39, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39);
impl_trampolines!(tramp_static_40, tramp_inst_40, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40);
impl_trampolines!(tramp_static_41, tramp_inst_41, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41);
impl_trampolines!(tramp_static_42, tramp_inst_42, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42);
impl_trampolines!(tramp_static_43, tramp_inst_43, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43);
impl_trampolines!(tramp_static_44, tramp_inst_44, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44);
impl_trampolines!(tramp_static_45, tramp_inst_45, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45);
impl_trampolines!(tramp_static_46, tramp_inst_46, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46);
impl_trampolines!(tramp_static_47, tramp_inst_47, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47);
impl_trampolines!(tramp_static_48, tramp_inst_48, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48);
impl_trampolines!(tramp_static_49, tramp_inst_49, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49);
impl_trampolines!(tramp_static_50, tramp_inst_50, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50);
impl_trampolines!(tramp_static_51, tramp_inst_51, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51);
impl_trampolines!(tramp_static_52, tramp_inst_52, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52);
impl_trampolines!(tramp_static_53, tramp_inst_53, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53);
impl_trampolines!(tramp_static_54, tramp_inst_54, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54);
impl_trampolines!(tramp_static_55, tramp_inst_55, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55);
impl_trampolines!(tramp_static_56, tramp_inst_56, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56);
impl_trampolines!(tramp_static_57, tramp_inst_57, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57);
impl_trampolines!(tramp_static_58, tramp_inst_58, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58);
impl_trampolines!(tramp_static_59, tramp_inst_59, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59);
impl_trampolines!(tramp_static_60, tramp_inst_60, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60);
impl_trampolines!(tramp_static_61, tramp_inst_61, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61);
impl_trampolines!(tramp_static_62, tramp_inst_62, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61, A62);
impl_trampolines!(tramp_static_63, tramp_inst_63, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61, A62, A63);
impl_trampolines!(tramp_static_64, tramp_inst_64, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61, A62, A63, A64);

/// Maps a user argument tuple to the shared **static** trampoline for its
/// types, fixing its address at compile time.
///
/// The casts below are plain `as` casts of fn items to fn pointers to raw
/// pointers — no `unsafe` — and the sealed [`NativeArg`]/[`NativeReturn`]
/// traits guarantee the target C signature matches the JNI descriptor derived
/// at runtime ([`Self::args_sig`] + `R::java_return_type()`).
#[doc(hidden)]
pub trait TrampBridgeStatic<R: NativeReturn>: Sized {
    /// The address of the shared static trampoline for `Self`/`R`.
    const PTR: *mut std::ffi::c_void;
    /// The JNI type fragments of the parameters (no parentheses; the
    /// receiver is not part of a static method's descriptor).
    fn args_sig() -> String;
}

/// Instance twin of [`TrampBridgeStatic`]: the user tuple starts with the
/// receiver (a [`JObject`] handle) and the remaining elements are the
/// method's declared parameters.
#[doc(hidden)]
pub trait TrampBridgeInst<R: NativeReturn>: Sized {
    const PTR: *mut std::ffi::c_void;
    fn args_sig() -> String;
}

macro_rules! impl_bridge_static {
    ($tramp:ident; $($A:ident),*) => {
        impl<$($A: NativeArg,)* R: NativeReturn> TrampBridgeStatic<R> for ($($A,)*) {
            const PTR: *mut std::ffi::c_void = ($tramp::<$($A,)* R> as for<'c> extern "system" fn(
                ::jni::EnvUnowned<'c>,
                ::jni::objects::JClass<'c>,
                $(<$A as NativeArg>::CType<'c>,)*
            ) -> <R as NativeReturn>::CRet<'c>) as *mut std::ffi::c_void;
            fn args_sig() -> String {
                let mut __frag = String::new();
                $(__frag.push_str(&<$A as NativeArg>::arg_sig());)*
                __frag
            }
        }
    };
}

macro_rules! impl_bridge_inst {
    ($tramp:ident; $($A:ident),*) => {
        impl<$($A: NativeArg,)* R: NativeReturn> TrampBridgeInst<R> for (JObject, $($A,)*) {
            const PTR: *mut std::ffi::c_void = ($tramp::<$($A,)* R> as for<'c> extern "system" fn(
                ::jni::EnvUnowned<'c>,
                ::jni::objects::JObject<'c>,
                $(<$A as NativeArg>::CType<'c>,)*
            ) -> <R as NativeReturn>::CRet<'c>) as *mut std::ffi::c_void;
            fn args_sig() -> String {
                let mut __frag = String::new();
                $(__frag.push_str(&<$A as NativeArg>::arg_sig());)*
                __frag
            }
        }
    };
}

impl_bridge_static!(tramp_static_0;);
impl_bridge_static!(tramp_static_1; A1);
impl_bridge_static!(tramp_static_2; A1, A2);
impl_bridge_static!(tramp_static_3; A1, A2, A3);
impl_bridge_static!(tramp_static_4; A1, A2, A3, A4);
impl_bridge_static!(tramp_static_5; A1, A2, A3, A4, A5);
impl_bridge_static!(tramp_static_6; A1, A2, A3, A4, A5, A6);
impl_bridge_static!(tramp_static_7; A1, A2, A3, A4, A5, A6, A7);
impl_bridge_static!(tramp_static_8; A1, A2, A3, A4, A5, A6, A7, A8);
impl_bridge_static!(tramp_static_9; A1, A2, A3, A4, A5, A6, A7, A8, A9);
impl_bridge_static!(tramp_static_10; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10);
impl_bridge_static!(tramp_static_11; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11);
impl_bridge_static!(tramp_static_12; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12);
impl_bridge_static!(tramp_static_13; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13);
impl_bridge_static!(tramp_static_14; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14);
impl_bridge_static!(tramp_static_15; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15);
impl_bridge_static!(tramp_static_16; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16);
impl_bridge_static!(tramp_static_17; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17);
impl_bridge_static!(tramp_static_18; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18);
impl_bridge_static!(tramp_static_19; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19);
impl_bridge_static!(tramp_static_20; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20);
impl_bridge_static!(tramp_static_21; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21);
impl_bridge_static!(tramp_static_22; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22);
impl_bridge_static!(tramp_static_23; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23);
impl_bridge_static!(tramp_static_24; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24);
impl_bridge_static!(tramp_static_25; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25);
impl_bridge_static!(tramp_static_26; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26);
impl_bridge_static!(tramp_static_27; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27);
impl_bridge_static!(tramp_static_28; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28);
impl_bridge_static!(tramp_static_29; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29);
impl_bridge_static!(tramp_static_30; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30);
impl_bridge_static!(tramp_static_31; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31);
impl_bridge_static!(tramp_static_32; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32);
impl_bridge_static!(tramp_static_33; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33);
impl_bridge_static!(tramp_static_34; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34);
impl_bridge_static!(tramp_static_35; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35);
impl_bridge_static!(tramp_static_36; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36);
impl_bridge_static!(tramp_static_37; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37);
impl_bridge_static!(tramp_static_38; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38);
impl_bridge_static!(tramp_static_39; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39);
impl_bridge_static!(tramp_static_40; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40);
impl_bridge_static!(tramp_static_41; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41);
impl_bridge_static!(tramp_static_42; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42);
impl_bridge_static!(tramp_static_43; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43);
impl_bridge_static!(tramp_static_44; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44);
impl_bridge_static!(tramp_static_45; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45);
impl_bridge_static!(tramp_static_46; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46);
impl_bridge_static!(tramp_static_47; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47);
impl_bridge_static!(tramp_static_48; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48);
impl_bridge_static!(tramp_static_49; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49);
impl_bridge_static!(tramp_static_50; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50);
impl_bridge_static!(tramp_static_51; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51);
impl_bridge_static!(tramp_static_52; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52);
impl_bridge_static!(tramp_static_53; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53);
impl_bridge_static!(tramp_static_54; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54);
impl_bridge_static!(tramp_static_55; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55);
impl_bridge_static!(tramp_static_56; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56);
impl_bridge_static!(tramp_static_57; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57);
impl_bridge_static!(tramp_static_58; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58);
impl_bridge_static!(tramp_static_59; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59);
impl_bridge_static!(tramp_static_60; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60);
impl_bridge_static!(tramp_static_61; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61);
impl_bridge_static!(tramp_static_62; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61, A62);
impl_bridge_static!(tramp_static_63; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61, A62, A63);
impl_bridge_static!(tramp_static_64; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61, A62, A63, A64);

impl_bridge_inst!(tramp_inst_0;);
impl_bridge_inst!(tramp_inst_1; A1);
impl_bridge_inst!(tramp_inst_2; A1, A2);
impl_bridge_inst!(tramp_inst_3; A1, A2, A3);
impl_bridge_inst!(tramp_inst_4; A1, A2, A3, A4);
impl_bridge_inst!(tramp_inst_5; A1, A2, A3, A4, A5);
impl_bridge_inst!(tramp_inst_6; A1, A2, A3, A4, A5, A6);
impl_bridge_inst!(tramp_inst_7; A1, A2, A3, A4, A5, A6, A7);
impl_bridge_inst!(tramp_inst_8; A1, A2, A3, A4, A5, A6, A7, A8);
impl_bridge_inst!(tramp_inst_9; A1, A2, A3, A4, A5, A6, A7, A8, A9);
impl_bridge_inst!(tramp_inst_10; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10);
impl_bridge_inst!(tramp_inst_11; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11);
impl_bridge_inst!(tramp_inst_12; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12);
impl_bridge_inst!(tramp_inst_13; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13);
impl_bridge_inst!(tramp_inst_14; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14);
impl_bridge_inst!(tramp_inst_15; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15);
impl_bridge_inst!(tramp_inst_16; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16);
impl_bridge_inst!(tramp_inst_17; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17);
impl_bridge_inst!(tramp_inst_18; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18);
impl_bridge_inst!(tramp_inst_19; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19);
impl_bridge_inst!(tramp_inst_20; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20);
impl_bridge_inst!(tramp_inst_21; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21);
impl_bridge_inst!(tramp_inst_22; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22);
impl_bridge_inst!(tramp_inst_23; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23);
impl_bridge_inst!(tramp_inst_24; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24);
impl_bridge_inst!(tramp_inst_25; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25);
impl_bridge_inst!(tramp_inst_26; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26);
impl_bridge_inst!(tramp_inst_27; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27);
impl_bridge_inst!(tramp_inst_28; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28);
impl_bridge_inst!(tramp_inst_29; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29);
impl_bridge_inst!(tramp_inst_30; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30);
impl_bridge_inst!(tramp_inst_31; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31);
impl_bridge_inst!(tramp_inst_32; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32);
impl_bridge_inst!(tramp_inst_33; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33);
impl_bridge_inst!(tramp_inst_34; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34);
impl_bridge_inst!(tramp_inst_35; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35);
impl_bridge_inst!(tramp_inst_36; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36);
impl_bridge_inst!(tramp_inst_37; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37);
impl_bridge_inst!(tramp_inst_38; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38);
impl_bridge_inst!(tramp_inst_39; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39);
impl_bridge_inst!(tramp_inst_40; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40);
impl_bridge_inst!(tramp_inst_41; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41);
impl_bridge_inst!(tramp_inst_42; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42);
impl_bridge_inst!(tramp_inst_43; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43);
impl_bridge_inst!(tramp_inst_44; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44);
impl_bridge_inst!(tramp_inst_45; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45);
impl_bridge_inst!(tramp_inst_46; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46);
impl_bridge_inst!(tramp_inst_47; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47);
impl_bridge_inst!(tramp_inst_48; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48);
impl_bridge_inst!(tramp_inst_49; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49);
impl_bridge_inst!(tramp_inst_50; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50);
impl_bridge_inst!(tramp_inst_51; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51);
impl_bridge_inst!(tramp_inst_52; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52);
impl_bridge_inst!(tramp_inst_53; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53);
impl_bridge_inst!(tramp_inst_54; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54);
impl_bridge_inst!(tramp_inst_55; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55);
impl_bridge_inst!(tramp_inst_56; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56);
impl_bridge_inst!(tramp_inst_57; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57);
impl_bridge_inst!(tramp_inst_58; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58);
impl_bridge_inst!(tramp_inst_59; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59);
impl_bridge_inst!(tramp_inst_60; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60);
impl_bridge_inst!(tramp_inst_61; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61);
impl_bridge_inst!(tramp_inst_62; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61, A62);
impl_bridge_inst!(tramp_inst_63; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61, A62, A63);

/// The registered callable behind a shared trampoline: holds the user's
/// function and runs the same [`dispatch`] pipeline the explicit form runs —
/// arity check, `FromJava` conversion, `catch_unwind`, `ToJava` result.
struct NativeAdapter<F, A, R>(F, PhantomData<(A, R)>);

impl<F, A, R> NativeCall for NativeAdapter<F, A, R>
where
    F: for<'a, 'env> Fn(&'a mut Env<'env>, A) -> JavaResult<R> + Send + Sync + 'static,
    A: for<'env> NativeArgs<'env> + Send + Sync + 'static,
    R: NativeReturn + Send + Sync + 'static,
{
    fn call<'env>(
        &self,
        env: &mut Env<'env>,
        args: Vec<JavaArg<'env>>,
    ) -> JavaResult<JValueOwned<'env>> {
        dispatch(env, args, &self.0)
    }
}

/// Build a type-derived **static** native-method descriptor for `name` backed
/// by `f` (a closure or fn item). The JNI signature is derived from the Rust
/// types: `A`'s parameter fragments plus `R`'s return fragment.
///
/// Called by the `native!("name", f)` macro form; the expansion is plain safe
/// code, so `#![forbid(unsafe_code)]` user crates keep compiling.
#[doc(hidden)]
pub fn make_native_static<F, A, R>(name: &str, f: F) -> JavaResult<NativeMethod>
where
    F: for<'a, 'env> Fn(&'a mut Env<'env>, A) -> JavaResult<R> + Send + Sync + 'static,
    A: for<'env> NativeArgs<'env> + TrampBridgeStatic<R> + Send + Sync + 'static,
    R: NativeReturn + FromJava + Send + Sync + 'static,
{
    let sig = format!("({}){}", A::args_sig(), R::java_return_type());
    let ptr = <A as TrampBridgeStatic<R>>::PTR;
    let adapter: Arc<dyn NativeCall> = Arc::new(NativeAdapter::<F, A, R>(f, PhantomData));
    registry_insert(ptr as usize, Arc::clone(&adapter));
    Ok(NativeMethod {
        name: name.to_string(),
        sig,
        fn_ptr: ptr,
        call: Some(adapter),
    })
}

/// Instance twin of [`make_native_static`]: the user tuple's first element is
/// the receiver (a [`JObject`] handle), which the trampoline prepends; the
/// remaining elements are the method's parameters.
#[doc(hidden)]
pub fn make_native_inst<F, A, R>(name: &str, f: F) -> JavaResult<NativeMethod>
where
    F: for<'a, 'env> Fn(&'a mut Env<'env>, A) -> JavaResult<R> + Send + Sync + 'static,
    A: for<'env> NativeArgs<'env> + TrampBridgeInst<R> + Send + Sync + 'static,
    R: NativeReturn + FromJava + Send + Sync + 'static,
{
    let sig = format!("({}){}", A::args_sig(), R::java_return_type());
    let ptr = <A as TrampBridgeInst<R>>::PTR;
    let adapter: Arc<dyn NativeCall> = Arc::new(NativeAdapter::<F, A, R>(f, PhantomData));
    registry_insert(ptr as usize, Arc::clone(&adapter));
    Ok(NativeMethod {
        name: name.to_string(),
        sig,
        fn_ptr: ptr,
        call: Some(adapter),
    })
}
