//! RAII Java object handles: [`JObject`], [`JClass`] and [`JArray`].
//!
//! Every handle wraps an [`jni::objects::Global`] reference behind an
//! `Arc`, so:
//!
//! * **`Clone` is `O(1)`** — it shares the underlying global reference
//!   instead of creating a new one (which would require a JNI call and an
//!   attached thread). The Java object is kept alive as long as *any* handle
//!   to it exists.
//! * **Drop releases** the global reference — the underlying
//!   [`jni::objects::Global`] deletes it when the last `Arc` is dropped.
//! * All handles are **`Send + Sync`** (global references are valid on any
//!   thread) and can be shared freely between threads.

use std::marker::PhantomData;
use std::sync::Arc;

use jni::objects::{Global, JClass as JniClass, JObject as JniObject};
use jni::{Env, JavaVM, JValueOwned};

use crate::array::JavaArrayElement;
use crate::call;
use crate::convert::{FromJava, JavaArg, ToJava};
use crate::error::{JavaError, JavaResult};

// ---------------------------------------------------------------------------
// Shared machinery
// ---------------------------------------------------------------------------

/// Run `f` with the current thread attached to the JVM, using the one true
/// JVM for the process (as tracked by the `jni` crate singleton).
pub(crate) fn with_env<T>(
    f: impl for<'env> FnOnce(&mut Env<'env>) -> JavaResult<T>,
) -> JavaResult<T> {
    let vm = JavaVM::singleton().map_err(|_| {
        JavaError::InvalidArgument(
            "the JVM is not initialized — create a Java instance with \
             `Java::builder().build()?` first, or wrap the `&mut jni::Env` \
             your native method receives with `Java::from_env`",
        )
    })?;
    vm.attach_current_thread(f)
}

// ---------------------------------------------------------------------------
// JObject
// ---------------------------------------------------------------------------

/// An owned, thread-safe handle to a Java object.
///
/// Backed by a JNI *global* reference; the object cannot be garbage-collected
/// while any clone of this handle exists.
#[derive(Debug)]
pub struct JObject {
    pub(crate) global: Arc<Global<JniObject<'static>>>,
}

impl JObject {
    pub(crate) fn from_global(global: Global<JniObject<'static>>) -> Self {
        JObject {
            global: Arc::new(global),
        }
    }

    /// The runtime class of this object (`Object.getClass()`).
    pub fn class(&self) -> JavaResult<JClass> {
        with_env(|env| {
            let local = env.new_local_ref(&*self.global)?;
            let cls = call::get_object_class(env, &local)?;
            Ok(JClass::from_global(env.new_global_ref(cls)?))
        })
    }

    /// Call an instance method with `args` as the argument list; the return
    /// type `R` is chosen by the caller's annotation.
    ///
    /// ```no_run
    /// # use rjava::prelude::*;
    /// # fn example(java: &Java, sb: &JObject) -> JavaResult<()> {
    /// let len: i32 = sb.call("length", ())?;
    /// let s: String = sb.call("toString", ())?;
    /// # Ok(()) }
    /// ```
    pub fn call<A: ToJava, R: FromJava>(&self, name: &str, args: A) -> JavaResult<R> {
        with_env(|env| call::call_method(env, &self.global, name, &args))
    }

    /// Convenience for calls that return void.
    ///
    /// ```no_run
    /// # use rjava::prelude::*;
    /// # fn example(java: &Java, sb: &JObject) -> JavaResult<()> {
    /// sb.call_void("append", (" world",))?;
    /// # Ok(()) }
    /// ```
    pub fn call_void<A: ToJava>(&self, name: &str, args: A) -> JavaResult<()> {
        self.call(name, args)
    }

    /// Read an instance field. Annotate `R` with the **exact** Java type of
    /// the field — unlike method lookup, field lookup compares the full type
    /// signature.
    pub fn get_field<F: FromJava>(&self, name: &str) -> JavaResult<F> {
        with_env(|env| call::get_field(env, &self.global, name))
    }

    /// Write an instance field. The Java type of the field is derived from
    /// `value`'s `ToJava` implementation.
    pub fn set_field<V: ToJava>(&self, name: &str, value: V) -> JavaResult<()> {
        with_env(|env| call::set_field(env, &self.global, name, &value))
    }

    /// The `String` produced by `Object.toString()`.
    pub fn to_string(&self) -> JavaResult<String> {
        self.call("toString", ())
    }
}

impl Clone for JObject {
    fn clone(&self) -> Self {
        JObject {
            global: Arc::clone(&self.global),
        }
    }
}

impl ToJava for JObject {
    fn to_java<'env>(&self, env: &mut Env<'env>) -> JavaResult<Vec<JavaArg<'env>>> {
        Ok(vec![JavaArg::Object(env.new_local_ref(&*self.global)?)])
    }
    fn java_args(&self) -> String {
        String::from("Ljava/lang/Object;")
    }
}

impl FromJava for JObject {
    fn from_java<'env>(env: &mut Env<'env>, value: JValueOwned<'env>) -> JavaResult<Self> {
        match value {
            JValueOwned::Object(o) if !o.is_null() => {
                Ok(JObject::from_global(env.new_global_ref(o)?))
            }
            JValueOwned::Object(_) => Err(JavaError::InvalidArgument(
                "method returned null where an object was expected \
                 (use Option<JObject> to accept null)",
            )),
            _ => Err(JavaError::InvalidArgument(
                "expected a Java object return value but got a primitive",
            )),
        }
    }
    fn java_return_type() -> String {
        // Initial signature guess for the lookup. Modern JVMs match the full
        // signature including the return type, so this generic Object
        // fragment will not resolve for methods returning a specific class;
        // the reflection fallback in call.rs then fixes the signature up and
        // retries the call.
        String::from("Ljava/lang/Object;")
    }
}

// ---------------------------------------------------------------------------
// JClass
// ---------------------------------------------------------------------------

/// An owned, thread-safe handle to a `java.lang.Class`.
#[derive(Debug)]
pub struct JClass {
    pub(crate) global: Arc<Global<JniClass<'static>>>,
}

impl JClass {
    pub(crate) fn from_global(global: Global<JniClass<'static>>) -> Self {
        JClass {
            global: Arc::new(global),
        }
    }

    /// Construct a new instance of this class, passing `args` to the
    /// constructor.
    ///
    /// ```no_run
    /// # use rjava::prelude::*;
    /// # fn example(java: &Java, clazz: &JClass) -> JavaResult<()> {
    /// let sb: JObject = clazz.new_instance(("Hello",))?;
    /// # Ok(()) }
    /// ```
    pub fn new_instance<A: ToJava>(&self, args: A) -> JavaResult<JObject> {
        with_env(|env| call::new_object(env, &self.global, &args))
    }

    /// Call a static method; `R` is chosen by the caller's annotation.
    pub fn call_static<A: ToJava, R: FromJava>(&self, name: &str, args: A) -> JavaResult<R> {
        with_env(|env| call::call_static_method(env, &self.global, name, &args))
    }

    /// Read a static field; annotate `R` with the **exact** Java type.
    pub fn get_static_field<F: FromJava>(&self, name: &str) -> JavaResult<F> {
        with_env(|env| call::get_static_field(env, &self.global, name))
    }

    /// Write a static field; the Java type is derived from `value`.
    pub fn set_static_field<V: ToJava>(&self, name: &str, value: V) -> JavaResult<()> {
        with_env(|env| call::set_static_field(env, &self.global, name, &value))
    }

    /// The binary name of this class (`Class.getName()`), e.g.
    /// `java.lang.StringBuilder`.
    pub fn name(&self) -> JavaResult<String> {
        with_env(|env| {
            let local: JniClass = env.new_local_ref(&*self.global)?;
            call::class_name(env, &local)
        })
    }

    /// Register native methods on this class (JNI `RegisterNatives`), making
    /// the Java `native` methods of this class dispatch into Rust functions.
    ///
    /// `methods` is a slice of descriptors produced by the [`crate::native!`]
    /// and [`crate::native_inst!`] macros. After this call, invoking a
    /// registered `native` method from Java (or Rust) runs the Rust function:
    ///
    /// ```no_run
    /// # use rjava::prelude::*;
    /// # use rjava::native;
    /// # fn example(java: &Java, clazz: &JClass) -> JavaResult<()> {
    /// // Java: `public static native int add(int a, int b);`
    /// fn add(_env: &mut jni::Env, (a, b): (i32, i32)) -> JavaResult<i32> {
    ///     Ok(a + b)
    /// }
    /// clazz.register_natives(&[native!("add", add)?])?; // type-derived: no signature string
    /// let sum: i32 = clazz.call_static("add", (2, 3))?;
    /// # Ok(()) }
    /// ```
    ///
    /// The [`register_natives!`](crate::register_natives) batch macro is the
    /// one-`?` form of this call for several descriptors at once (any mix of
    /// [`native!`](crate::native), [`native_inst!`](crate::native_inst) and
    /// [`async_native!`](crate::async_native) items, no array brackets).
    ///
    /// Registering a name/signature that does not match a `native` method of
    /// this class raises a Java `NoSuchMethodError`, surfaced as
    /// [`JavaError::JavaException`]. One automatic fix: a **type-derived**
    /// descriptor whose derived return fragment is the deliberately-generic
    /// `Ljava/lang/Object;` / `[Ljava/lang/Object;` marker (the `JObject` /
    /// `Vec<JObject>` / `Option<JObject>` annotations) is re-resolved against
    /// the class's actual method return type via reflection and the batch is
    /// re-registered — so a native whose Java method returns a concrete class
    /// or concrete array type works out of the box. The explicit-signature
    /// form is never auto-corrected. Registering two **type-derived**
    /// descriptor with the same Rust signature is rejected up front with
    /// [`JavaError::InvalidArgument`] (they would share one trampoline; use
    /// the explicit-signature `native!("name", "(II)I", f)` form to
    /// disambiguate). `async_native!` registrations share the same rule but
    /// have **no** explicit-signature form — register only one async method
    /// per Rust argument-tuple type. See the [crate docs](crate) for the
    /// full native-methods section.
    pub fn register_natives(&self, methods: &[crate::native::NativeMethod]) -> JavaResult<()> {
        // Type-derived descriptors share one trampoline per `(A, R)` Rust
        // signature (see `crate::native`); two *different* registrations with
        // the same signature would both point at it and the second would
        // silently replace the first's callable. Detect that here, before any
        // JNI call. `Arc::ptr_eq` distinguishes descriptors built by the same
        // `native!` evaluation (same adapter — e.g. re-registering the same
        // descriptor on another class) from genuinely different ones.
        for method in methods {
            if let Some(call) = &method.call
                && let Some(existing) = crate::native::registry_get(method.fn_ptr() as usize)
                && !Arc::ptr_eq(&existing, call)
            {
                return Err(JavaError::InvalidArgument(
                    "two native methods share the same Rust signature and would map to \
                     one trampoline; register only one of them, or give one an explicit \
                     signature: native!(name, \"(II)I\", f) (async_native! has no \
                     explicit-signature form in this version — register only one async \
                     method per Rust argument-tuple type)",
                ));
            }
        }
        with_env(|env| {
            let local: JniClass = env.new_local_ref(&*self.global)?;
            let data: Vec<(
                jni::strings::JNIString,
                jni::strings::JNIString,
                *mut std::ffi::c_void,
            )> = methods
                .iter()
                .map(|m| {
                    (
                        jni::strings::JNIString::from(m.name()),
                        jni::strings::JNIString::from(m.sig()),
                        m.fn_ptr(),
                    )
                })
                .collect();
            let result = call::with_check(env, |env| {
                rjava_helper::register_natives(env, &local, &data)
            });
            match result {
                Ok(()) => Ok(()),
                // Registration-time fallback: a type-derived descriptor whose
                // derived return fragment is the generic `Ljava/lang/Object;` /
                // `[Ljava/lang/Object;` marker may mismatch the concrete Java
                // return type (modern JVMs match the FULL descriptor). Resolve
                // the exact return type via reflection and retry the whole
                // batch; explicit-signature descriptors are never corrected.
                Err(e) if Self::is_nosuchmethod(&e) => {
                    match call::resolve_derived_native_sigs(env, &local, methods)? {
                        Some(corrected) => {
                            let data: Vec<(
                                jni::strings::JNIString,
                                jni::strings::JNIString,
                                *mut std::ffi::c_void,
                            )> = methods
                                .iter()
                                .zip(corrected.iter())
                                .map(|(m, sig)| {
                                    (
                                        jni::strings::JNIString::from(m.name()),
                                        jni::strings::JNIString::from(sig),
                                        m.fn_ptr(),
                                    )
                                })
                                .collect();
                            call::with_check(env, |env| {
                                rjava_helper::register_natives(env, &local, &data)
                            })
                        }
                        // Nothing qualified, or a qualifying method could not
                        // be resolved: the user's original error stays.
                        None => Err(e),
                    }
                }
                Err(e) => Err(e),
            }
        })
    }

    /// Does `e` represent the `NoSuchMethodError` a failed batch
    /// registration raises? The `jni` crate's `register_native_methods`
    /// catches the JVM's `NoSuchMethodError` itself and surfaces it as
    /// `jni::errors::Error::NoSuchMethod` (the exception is already cleared),
    /// so the raw-JNI form is the one that actually occurs; the
    /// `JavaException` form is accepted too, in case the mismatch ever
    /// surfaces as a pending exception on another JVM or jni version.
    fn is_nosuchmethod(e: &JavaError) -> bool {
        match e {
            JavaError::Jni(jni::errors::Error::NoSuchMethod(_)) => true,
            JavaError::JavaException { class, .. } => class.contains("NoSuchMethodError"),
            _ => false,
        }
    }
}

impl Clone for JClass {
    fn clone(&self) -> Self {
        JClass {
            global: Arc::clone(&self.global),
        }
    }
}

// ---------------------------------------------------------------------------
// JArray
// ---------------------------------------------------------------------------

/// An owned, thread-safe handle to a Java array of element type `T`.
///
/// `T` is either a primitive (`i8`, `i16`, `i32`, `i64`, `f32`, `f64`, `bool`,
/// `char`) or [`JObject`]. Arrays of objects additionally support `null`
/// elements: read them with `get::<Option<JObject>>(i)` (null → `None`) or
/// `get::<JObject>(i)` (null → error).
#[derive(Debug)]
pub struct JArray<T> {
    pub(crate) global: Arc<Global<JniObject<'static>>>,
    pub(crate) _marker: PhantomData<T>,
}

impl JArray<JObject> {
    /// Create a new `Object[]` array filled from `values` (all elements are
    /// guaranteed non-null by the `JObject` type). The facade equivalent is
    /// [`Java::new_object_array_from`](crate::Java::new_object_array_from).
    pub fn from_vec(values: Vec<JObject>) -> JavaResult<Self> {
        with_env(|env| {
            let arr = JArray::from_vec_local_obj(env, &values)?;
            Ok(JArray::from_global_obj(env.new_global_ref(&arr)?))
        })
    }

    /// Read element `index`. A `null` element becomes `None`; annotate
    /// `get::<JObject>` to turn nulls into an error instead.
    pub fn get<R: FromJava>(&self, index: usize) -> JavaResult<R> {
        with_env(|env| {
            let v = call::array_get_object(env, &self.global, index)?;
            let v = match v {
                Some(o) => JValueOwned::Object(o),
                None => JValueOwned::Object(JniObject::null()),
            };
            R::from_java(env, v)
        })
    }

    /// Read element `index`, erroring on `null` elements.
    pub fn get_required(&self, index: usize) -> JavaResult<JObject> {
        self.get(index)
    }

    /// Write element `index`.
    pub fn set(&self, index: usize, value: JObject) -> JavaResult<()> {
        with_env(|env| call::array_set_object(env, &self.global, index, &value.global))
    }

    /// Copy all elements into a Rust `Vec`, erroring if any element is null.
    pub fn to_vec(&self) -> JavaResult<Vec<JObject>> {
        with_env(|env| self.to_vec_local_obj(env))
    }

    /// Copy all elements into a Rust `Vec<Option<JObject>>` (null → `None`).
    pub fn to_vec_options(&self) -> JavaResult<Vec<Option<JObject>>> {
        with_env(|env| {
            let len = call::array_len_object(env, &self.global)?;
            let mut out = Vec::with_capacity(len);
            for i in 0..len {
                match call::array_get_object(env, &self.global, i)? {
                    Some(o) => out.push(Some(JObject::from_global(env.new_global_ref(o)?))),
                    None => out.push(None),
                }
            }
            Ok(out)
        })
    }
}

impl<T: JavaArrayElement> JArray<T> {
    /// Create a new Java array filled from `values`. The facade equivalent is
    /// [`Java::new_array_from`](crate::Java::new_array_from).
    pub fn from_vec(values: Vec<T>) -> JavaResult<Self> {
        with_env(|env| {
            let arr = JArray::from_vec_local(env, &values)?;
            Ok(JArray::from_global_obj(env.new_global_ref(&arr)?))
        })
    }

    /// Read element `index` (bounds-checked by the JVM). The Rust type `R`
    /// is chosen by the caller's annotation and must match the element type.
    pub fn get<R: FromJava>(&self, index: usize) -> JavaResult<R> {
        with_env(|env| {
            let v = call::array_get(env, &self.global, index, T::__kind())?;
            R::from_java(env, v)
        })
    }

    /// Write element `index`.
    pub fn set(&self, index: usize, value: T) -> JavaResult<()> {
        let arg = value.__as_java_arg()?;
        with_env(|env| call::array_set(env, &self.global, index, &arg, T::__kind()))
    }

    /// Copy all elements into a Rust `Vec`.
    pub fn to_vec(&self) -> JavaResult<Vec<T>> {
        with_env(|env| self.to_vec_local(env))
    }

    /// Number of elements in the array.
    pub fn len(&self) -> JavaResult<usize> {
        with_env(|env| call::array_len_kind(env, &self.global, T::__kind()))
    }

    /// Whether the array has no elements.
    pub fn is_empty(&self) -> JavaResult<bool> {
        Ok(self.len()? == 0)
    }
}

impl JArray<JObject> {
    /// Number of elements in the array.
    pub fn len(&self) -> JavaResult<usize> {
        with_env(|env| call::array_len_object(env, &self.global))
    }

    /// Whether the array has no elements.
    pub fn is_empty(&self) -> JavaResult<bool> {
        Ok(self.len()? == 0)
    }
}

impl<T> JArray<T> {
    pub(crate) fn from_global_obj(global: Global<JniObject<'static>>) -> Self {
        JArray {
            global: Arc::new(global),
            _marker: PhantomData,
        }
    }
}

impl<T> Clone for JArray<T> {
    fn clone(&self) -> Self {
        JArray {
            global: Arc::clone(&self.global),
            _marker: PhantomData,
        }
    }
}

// ---------------------------------------------------------------------------
// JavaThread — RAII attachment guard
// ---------------------------------------------------------------------------

/// A RAII handle to a thread attached to the JVM.
///
/// Obtained via [`Java::attach_thread`](crate::Java::attach_thread). While it
/// exists, the calling thread is attached to the JVM; when it is dropped, the
/// thread is detached again (so Java calls from that thread afterwards
/// re-attach automatically).
#[derive(Debug)]
pub struct JavaThread {
    pub(crate) vm: JavaVM,
}

impl Drop for JavaThread {
    fn drop(&mut self) {
        // Detach errors (e.g. the JVM is already shutting down) are not
        // reportable from Drop; ignore them.
        let _ = self.vm.detach_current_thread();
    }
}
