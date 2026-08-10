//! Compile-time-typed Java class bindings — the runtime support behind the
//! [`bind!`](macro@crate::bind) macro.
//!
//! The dynamic call path — `java.new_object("java.lang.StringBuilder", ("Hello",))?`
//! then `sb.call("length", ())?` — is string-typed and runtime-checked. A
//! binding declares the class and its methods **once**, with Rust types, and
//! every call becomes a direct typed method:
//!
//! ```no_run
//! use rjava::prelude::*;
//! use rjava::bind;
//!
//! bind! {
//!     "java.lang.StringBuilder" => StringBuilder {
//!         fn append(s: &str) -> Self;   // chainable: returns the same object, re-wrapped
//!         fn length() -> i32;
//!         fn toString() -> String;
//!     }
//! }
//!
//! # fn example() -> JavaResult<()> {
//! let java = Java::builder().build()?;
//! let sb = java.new::<StringBuilder>(("Hello",))?;
//! sb.append(" world")?;
//! let len: i32 = sb.length()?;
//! assert_eq!(len, 11);
//! # Ok(()) }
//! ```
//!
//! Every `bind!` expansion generates:
//!
//! * a wrapper `pub struct` holding the [`Java`] facade and the
//!   [`JObject`](crate::JObject) handle (both private),
//! * one typed method per declared method — instance methods take `&self`
//!   and call into this module's helpers with the compile-time JNI
//!   signature; `static fn` declarations generate a method taking
//!   `java: &Java` as the first parameter,
//! * a [`JavaBound`](crate::bind::JavaBound) impl carrying the class name, the class cache, and the
//!   wrap/unwrap helpers.
//!
//! # Fields and aliases
//!
//! A `field` declaration binds a Java **field** with a typed accessor pair:
//!
//! ```no_run
//! # use rjava::prelude::*;
//! # use rjava::bind;
//! bind! {
//!     "bind.Kit" => Kit {
//!         field tag: String;             // -> get_tag / set_tag over the field "tag"
//!         field active: bool;            //    (bool getter falls back to the
//!         static field MAGIC: i32;       //    get<Name>/is<Name> accessors)
//!         fn to_string() -> String [java_name = "toString"];   // alias
//!     }
//! }
//! ```
//!
//! * `field name: Type;` generates `get_name(&self) -> JavaResult<Type>` and
//!   `set_name(&self, v: Type) -> JavaResult<()>` reading/writing the Java
//!   field named exactly `name` (the JNI type is the declared type's —
//!   `String`, `i32`, `bool`, `Option<T>`, `Vec<T>`, …). A **`bool`** field's
//!   getter tries the raw field first and falls back to the bean-style
//!   accessor methods `get<Name>()` then `is<Name>()` (the `bean` module's
//!   read order) when the field does not exist — the usual shape for boolean
//!   properties. `static field NAME: Type;` generates
//!   `get_static_<name>(java: &Java)` / `set_static_<name>(java: &Java, v)`
//!   over the static field.
//! * `fn to_string() -> String [java_name = "toString"];` — the optional
//!   Java-name alias: the Rust method keeps the idiomatic name while the JNI
//!   call targets the aliased Java method. Without an alias the Java name is
//!   the Rust name verbatim.
//!
//! # Construction
//!
//! * [`Java::new::<T>(args)`](crate::Java::new) constructs a wrapper through
//!   the class's constructor (the argument tuple becomes the constructor
//!   arguments).
//! * [`Java::wrap::<T>(obj)`](crate::Java::wrap) wraps an existing object
//!   handle — no JNI call happens, and the class is resolved (and validated)
//!   lazily on the first actual call, exactly like `new`.
//!
//! The class name is **compile-time validated at first use**: the first call
//! that needs the class resolves it and caches it, so a wrong class name
//! surfaces as a clear [`JavaError`] from the first call, not at
//! declaration site.
//!
//! # Method-ID caching (and why it is what it is)
//!
//! The **class** reference is cached per wrapper type — a
//! [`OnceLock`](std::sync::OnceLock) over a JNI global reference, exactly like the `userdata`
//! module's `SYSTEM_CLASS` pattern. `Global` is `Send + Sync`, so the cache
//! needs **no `unsafe`**; the first caller does the lookup and the loser of
//! the `get_or_try_init` race drops its redundant reference. This makes
//! `FindClass` run once per wrapper instead of once per call.
//!
//! **Method IDs are re-derived on every call.** jni-rs's `MethodID` carries
//! a lifetime tied to the `Env` it was looked up on, so a cached ID would
//! have to outlive that `Env` — which requires `unsafe` to conjure the
//! lifetime. `rjava` forbids `unsafe`, so each call lets the JNI call
//! re-look-up the method ID on the cached class (`GetMethodID` /
//! `GetStaticMethodID` internally). This still skips the expensive
//! `FindClass`; the per-call method-ID lookup is a cheap JNI call that the
//! dynamic path pays too.
//!
//! The JNI signature literal is computed by the `bind!` macro at **compile
//! time** (see the macro's type-mapping table) and validated again at
//! compile time by `jni::jni_sig!`; the generated helpers only convert
//! arguments and results, and apply the same exact-signature reflection
//! fallback as the dynamic call machinery when a declared `JObject` (or a
//! wrong declared type) does not resolve directly.
//!
//! # Dynamic escape hatch
//!
//! A wrapper is *not* a sealed object: [`JavaBound::obj`](crate::bind::JavaBound::obj) borrows the
//! underlying [`JObject`](crate::JObject) handle, so typed calls and dynamic
//! `call`/`call_void`/`get_field` calls can be mixed freely on the same
//! object. [`JavaBound::into_obj`](crate::bind::JavaBound::into_obj) consumes the wrapper back into the
//! handle.

use jni::objects::{Global, JClass as JniClass};
use jni::signature::MethodSignature;
use jni::strings::JNIString;
use jni::{Env, JValue, JValueOwned};

use crate::call;
use crate::convert::{to_jvalue, FromJava, JavaArg, ToJava};
use crate::error::{JavaError, JavaResult};
use crate::handles::JObject;
use crate::java::Java;

/// The contract every [`bind!`](macro@crate::bind) wrapper implements.
///
/// Implemented by the macro expansion (a wrapper struct plus the class
/// cache); you interact with it through [`Java::new`](crate::Java::new),
/// [`Java::wrap`](crate::Java::wrap) and the wrapper's own methods. The
/// trait is public so user code can name it in bounds (e.g. a function
/// accepting any binding of a class).
pub trait JavaBound: Sized {
    /// The JNI (slash-form) class name behind this binding, e.g.
    /// `java/lang/StringBuilder`.
    fn class_name() -> &'static str;

    /// The class as a process-global reference, resolved and cached on first
    /// use.
    ///
    /// `OnceLock<Global>` is `Send + Sync`, so the cache needs no `unsafe`
    /// (see the [module docs](self) for the method-ID decision). A wrong
    /// class name surfaces here — as a clear [`JavaError`] from the first
    /// call that needs the class.
    fn class(env: &mut Env<'_>) -> JavaResult<&'static Global<JniClass<'static>>>;

    /// Wrap an existing Java object into this binding.
    fn wrap(java: Java, obj: JObject) -> Self;

    /// Borrow the underlying object handle — the dynamic escape hatch: mix
    /// `bind!`-typed calls with the dynamic
    /// [`JObject::call`](crate::JObject::call) / `call_void` / field calls
    /// on the same object.
    fn obj(&self) -> &JObject;

    /// Unwrap this binding back into its object handle.
    fn into_obj(self) -> JObject;
}

/// Look up `name` (a slash-form JNI class name) and return it as a global
/// reference; the caller caches it. Used by the generated
/// [`JavaBound::class`] implementations.
#[doc(hidden)]
pub fn find_class_global(env: &mut Env<'_>, name: &str) -> JavaResult<Global<JniClass<'static>>> {
    let local = call::find_class(env, JNIString::from(name))?;
    Ok(env.new_global_ref(local)?)
}

/// One bound call: convert `args`, perform the JNI call (`call_method` for
/// instance methods, `call_static_method` for static ones), and return the
/// raw result — applying the same exact-signature reflection fallback as the
/// dynamic call machinery when the compile-time signature does not resolve
/// (a declared `JObject`, or a declared type that does not match the real
/// method).
fn invoke<'env>(
    env: &mut Env<'env>,
    class: &'static Global<JniClass<'static>>,
    name: &str,
    sig: MethodSignature<'static, 'static>,
    args: Vec<JavaArg<'env>>,
    is_static: bool,
    obj: Option<&JObject>,
) -> JavaResult<JValueOwned<'env>> {
    let name_j = JNIString::from(name);
    let result = {
        let jvalues: Vec<JValue> = args.iter().map(to_jvalue).collect();
        match obj {
            Some(obj) => {
                let obj_local = env.new_local_ref(&*obj.global)?;
                call::with_check(env, |env| env.call_method(&obj_local, name_j.clone(), sig, &jvalues))
            }
            None => call::with_check(env, |env| {
                env.call_static_method(class, name_j.clone(), sig, &jvalues)
            }),
        }
    };
    match result {
        Ok(v) => Ok(v),
        Err(JavaError::Jni(jni::errors::Error::MethodNotFound { .. })) => {
            let class_local: JniClass = env.new_local_ref(class)?;
            let (rms, adjusted) =
                call::resolve_exact_method_sig(env, &class_local, name, args, is_static)?;
            let sig: MethodSignature = (&rms).into();
            let jvalues: Vec<JValue> = adjusted.iter().map(to_jvalue).collect();
            match obj {
                Some(obj) => {
                    let obj_local = env.new_local_ref(&*obj.global)?;
                    call::with_check(env, |env| env.call_method(&obj_local, name_j, sig, &jvalues))
                }
                None => call::with_check(env, |env| {
                    env.call_static_method(class, name_j, sig, &jvalues)
                }),
            }
        }
        Err(e) => Err(e),
    }
}

/// Call an instance method of a bound wrapper, converting the result with
/// `R`'s [`FromJava`] implementation. Generated by the `bind!` macro.
#[doc(hidden)]
pub fn instance_method<B: JavaBound, R: FromJava, A: ToJava>(
    java: &Java,
    obj: &JObject,
    name: &str,
    sig: MethodSignature<'static, 'static>,
    args: A,
) -> JavaResult<R> {
    java.with_env(|env| {
        // Resolving the cached class validates the class name at first use.
        let class = B::class(env)?;
        let arg_list = args.to_java(env)?;
        let v = invoke(env, class, name, sig, arg_list, false, Some(obj))?;
        let r = R::from_java(env, v);
        call::finish(env, r)
    })
}

/// Call an instance method of a bound wrapper and re-wrap the returned
/// object into the same binding (`-> Self`): the wrapper's [`Java`] facade
/// is cloned per call (`Java` is a cheap clone — it is one `JavaVM`
/// pointer).
#[doc(hidden)]
pub fn instance_self<B: JavaBound, A: ToJava>(
    java: &Java,
    obj: &JObject,
    name: &str,
    sig: MethodSignature<'static, 'static>,
    args: A,
) -> JavaResult<B> {
    java.with_env(|env| {
        let class = B::class(env)?;
        let arg_list = args.to_java(env)?;
        let v = invoke(env, class, name, sig, arg_list, false, Some(obj))?;
        let r = <JObject as FromJava>::from_java(env, v).map(|o| B::wrap(java.clone(), o));
        call::finish(env, r)
    })
}

/// Call a static method of a bound class, converting the result with `R`'s
/// [`FromJava`] implementation. Generated by the `bind!` macro.
#[doc(hidden)]
pub fn static_method<B: JavaBound, R: FromJava, A: ToJava>(
    java: &Java,
    name: &str,
    sig: MethodSignature<'static, 'static>,
    args: A,
) -> JavaResult<R> {
    java.with_env(|env| {
        let class = B::class(env)?;
        let arg_list = args.to_java(env)?;
        let v = invoke(env, class, name, sig, arg_list, true, None)?;
        let r = R::from_java(env, v);
        call::finish(env, r)
    })
}

/// Call a static method of a bound class and re-wrap the returned object
/// into the same binding (`-> Self`, e.g. a static factory).
#[doc(hidden)]
pub fn static_self<B: JavaBound, A: ToJava>(
    java: &Java,
    name: &str,
    sig: MethodSignature<'static, 'static>,
    args: A,
) -> JavaResult<B> {
    java.with_env(|env| {
        let class = B::class(env)?;
        let arg_list = args.to_java(env)?;
        let v = invoke(env, class, name, sig, arg_list, true, None)?;
        let r = <JObject as FromJava>::from_java(env, v).map(|o| B::wrap(java.clone(), o));
        call::finish(env, r)
    })
}

// ---------------------------------------------------------------------------
// Fields (the `field` / `static field` declarations of `bind!`)
// ---------------------------------------------------------------------------

/// CamelCase a field name with the crate's simple word-boundary rule — the
/// exact rule the `bean` module uses (`user_id` → `UserId`, `id` → `Id`, no
/// acronym special-casing), mirrored here for the bool field getter's
/// `get<Name>` / `is<Name>` accessor fallback.
fn camel_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut word_start = true;
    for ch in name.chars() {
        if ch == '_' {
            word_start = true;
        } else if word_start {
            out.extend(ch.to_uppercase());
            word_start = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Read an instance field of a bound wrapper, converting the result with
/// `R`'s [`FromJava`] implementation. The JNI field signature is derived
/// from `R`'s declared type (`FromJava::java_return_type`), exactly like the
/// macro-time `bind!` mapping. Generated by the `bind!` macro.
#[doc(hidden)]
pub fn instance_field_get<B: JavaBound, R: FromJava>(
    java: &Java,
    obj: &JObject,
    name: &str,
) -> JavaResult<R> {
    java.with_env(|env| {
        // Resolving the cached class keeps the "wrong class name surfaces at
        // first use" contract (the raw field read itself only needs the
        // object's runtime class).
        let _class = B::class(env)?;
        call::get_field(env, &obj.global, name)
    })
}

/// Write an instance field of a bound wrapper; the Java type is derived from
/// `V`'s [`ToJava`] implementation. Generated by the `bind!` macro.
#[doc(hidden)]
pub fn instance_field_set<B: JavaBound, V: ToJava>(
    java: &Java,
    obj: &JObject,
    name: &str,
    value: V,
) -> JavaResult<()> {
    java.with_env(|env| {
        let _class = B::class(env)?;
        call::set_field(env, &obj.global, name, &value)
    })
}

/// Read a static field of a bound class, converting the result with `R`'s
/// [`FromJava`] implementation. Generated by the `bind!` macro.
#[doc(hidden)]
pub fn static_field_get<B: JavaBound, R: FromJava>(
    java: &Java,
    name: &str,
) -> JavaResult<R> {
    java.with_env(|env| {
        let class = B::class(env)?;
        call::get_static_field(env, class, name)
    })
}

/// Write a static field of a bound class; the Java type is derived from
/// `V`'s [`ToJava`] implementation. Generated by the `bind!` macro.
#[doc(hidden)]
pub fn static_field_set<B: JavaBound, V: ToJava>(
    java: &Java,
    name: &str,
    value: V,
) -> JavaResult<()> {
    java.with_env(|env| {
        let class = B::class(env)?;
        call::set_static_field(env, class, name, &value)
    })
}

/// Read a `bool` instance field of a bound wrapper, **falling back to the
/// bean-style accessor methods** `get<Name>()` then `is<Name>()` (camelCased
/// with the crate's word-boundary rule) when the field itself does not
/// exist — mirroring the `bean` module's read path, where boolean
/// properties are usually exposed through accessors rather than a direct
/// field. Generated by the `bind!` macro for `field …: bool;` declarations.
#[doc(hidden)]
pub fn instance_bool_field_get<B: JavaBound>(
    java: &Java,
    obj: &JObject,
    name: &str,
) -> JavaResult<bool> {
    java.with_env(|env| {
        // Resolving the cached class keeps the "wrong class name surfaces at
        // first use" contract.
        let _class = B::class(env)?;
        // 1. The raw field.
        match call::get_field::<bool>(env, &obj.global, name) {
            Ok(v) => return Ok(v),
            Err(JavaError::Jni(jni::errors::Error::FieldNotFound { .. })) => {}
            Err(e) => return Err(e),
        }
        // 2./3. The accessor methods, in the bean read order (get first).
        // A bean-style accessor takes no arguments and returns a boolean, so
        // the signature `()Z` is statically known — no reflection needed; a
        // missing method surfaces as `MethodNotFound` and moves to the next
        // candidate.
        let local = env.new_local_ref(&*obj.global)?;
        let mut try_accessor = |name: String| {
            let v = call::with_check(env, |env| {
                env.call_method(&local, JNIString::from(name), jni::jni_sig!("()Z"), &[])
            })?;
            match v {
                JValueOwned::Bool(b) => Ok(b),
                _ => Err(JavaError::InvalidArgument(
                    "rjava::bind: a bool field accessor did not return a boolean",
                )),
            }
        };
        let getter = format!("get{}", camel_case(name));
        match try_accessor(getter) {
            Ok(v) => return Ok(v),
            Err(JavaError::Jni(jni::errors::Error::MethodNotFound { .. })) => {}
            Err(e) => return Err(e),
        }
        let is_getter = format!("is{}", camel_case(name));
        try_accessor(is_getter)
    })
}
