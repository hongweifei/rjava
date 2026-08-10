//! # rjava — safe, ergonomic Java interop for Rust, modeled after [mlua]
//!
//! `rjava` is to the JNI what [mlua] is to the Lua C API: a small, safe,
//! ergonomic wrapper. You never touch raw JNI, never write signature strings,
//! and never manage JVM attachment by hand — and there is **no `unsafe`** in
//! `rjava`'s own code (`#![forbid(unsafe_code)]`).
//!
//! It is built on top of the [`jni`] crate (jni-rs); all the low-level FFI
//! lives there.
//!
//! # Quick start
//!
//! ```no_run
//! use rjava::prelude::*;
//!
//! # fn quick_start() -> JavaResult<()> {
//! // 1) JVM facade (mlua's `Lua` analog)
//! let java = Java::builder()
//!     .class_path("target/classes")   // optional; ";"-separated on Windows
//!     .option("-Xmx256m")             // raw JVM option, repeatable
//!     .build()?;                      // create JVM via the invocation API
//!
//! // (if a JVM already exists: a second `Java::builder().build()` reuses it;
//! //  Rust loaded inside a Java process instead wraps the `Env` its native
//! //  method receives: `let java = Java::from_env(env)?;`)
//!
//! // 2) Classes & objects
//! let clazz: JClass = java.class("java.lang.StringBuilder")?;
//! let sb: JObject = clazz.new_instance(("Hello",))?; // ctor args as tuple
//! let len: i32 = sb.call("length", ())?;
//! sb.call_void("append", (" world",))?;           // void methods: call_void
//! let s: String = sb.call("toString", ())?;
//! let rt: JClass = sb.class()?;                        // runtime class
//! let sb2: JObject = java.new_object("java.lang.StringBuilder", ("Hi",))?;
//!
//! // 3) Static members
//! let max: i32 = java.class("java.lang.Math")?.call_static("max", (3_i32, 7_i32))?;
//! let pi: f64 = java.class("java.lang.Math")?.get_static_field("PI")?;
//!
//! // 4) Arrays
//! let arr: JArray<i32> = java.new_array(10)?;         // int[10]
//! arr.set(0, 42)?;
//! let v: i32 = arr.get(0)?;
//! let objs: JArray<JObject> = java.new_object_array("java.lang.String", 3)?;
//! objs.set(0, java.new_object("java.lang.String", ("a",))?)?;
//! let maybe: Option<JObject> = objs.get(1)?;          // null element -> None
//! let bytes: Vec<u8> = java
//!     .new_object("java.lang.String", ("bytes",))?
//!     .call("getBytes", ())?;                         // byte[] -> Vec<u8>
//! let parts: Vec<String> = java
//!     .new_object("java.lang.String", ("a,b,c",))?
//!     .call("split", (",",))?;                        // String[] -> Vec<String>
//! let ints = JArray::<i32>::from_vec(vec![1, 2, 3])?; // int[] from a Vec
//!
//! // 5) Collections & primitives: primitives auto-box for Object-typed
//! //    parameters (ArrayList.add(Object), HashMap.put(Object, Object), …)
//! //    and wrapper objects auto-unbox for primitive parameters
//! //    (Math.max(int, int) with an Integer object — exact wrapper type
//! //    only, no widening, and null objects never match a primitive).
//! let list = java.new_object("java.util.ArrayList", ())?;
//! list.call_void("add", (10_i32,))?;                  // autoboxed to Integer
//! let first: JObject = list.call("get", (0,))?;
//! let n: i32 = first.call("intValue", ())?;            // == 10
//! let map = java.new_object("java.util.HashMap", ())?;
//! map.call_void("put", ("k", 1_i32))?;               // 1 autoboxed
//! let n: i32 = map.call("size", ())?;                 // == 1
//! let boxed = java.new_object("java.lang.Integer", (5_i32,))?;
//! let max: i32 = java
//!     .class("java.lang.Math")?
//!     .call_static("max", (&boxed, 3_i32))?;          // == 5 — unboxed
//!
//! // 6) Errors: a thrown Java exception becomes a typed Rust error naming
//! //    the operation that failed (`JavaError::WithContext`)
//! let parsed: JavaResult<i32> =
//!     java.class("java.lang.Integer")?.call_static("parseInt", ("not-a-number",));
//! match parsed {
//!     Err(JavaError::WithContext { operation, source }) => {
//!         // operation == "calling parseInt(String) on java.lang.Integer"
//!         assert_eq!(operation, "calling parseInt(String) on java.lang.Integer");
//!         // source is the captured JavaException:
//!         // java.lang.NumberFormatException: For input string: "not-a-number"
//!         let JavaError::JavaException { class, .. } = *source else {
//!             panic!("expected a JavaException source");
//!         };
//!         assert_eq!(class, "java.lang.NumberFormatException");
//!     }
//!     _ => panic!("parseInt(\"not-a-number\") must throw"),
//! }
//! # Ok(()) }
//! # fn main() {}
//! ```
//!
//! # Calling Rust from Java (native methods)
//!
//! `rjava` can register Rust functions as Java `native` methods (the mlua
//! `Lua::create_function` analog, over JNI `RegisterNatives`). The functions
//! are plain safe Rust: the first parameter is the `jni` [`Env`](https://docs.rs/jni/latest/jni/struct.Env.html) (like mlua's
//! `lua` parameter), the second is a tuple of arguments, and the return type
//! is any single-value [`ToJava`] type. **Closures and fn items both work**;
//! a capturing closure must be `Send + Sync + 'static` (use `move` for stack
//! captures).
//!
//! ```no_run
//! use rjava::prelude::*;
//! use rjava::{native, native_inst};
//!
//! // Java:
//! //   public class NativeLib {
//! //       public static native int add(int a, int b);
//! //       public static native long addLong(long a, long b);
//! //       public native int times(int factor);
//! //       public int base;
//! //   }
//!
//! fn add(_env: &mut jni::Env, (a, b): (i32, i32)) -> JavaResult<i32> {
//!     Ok(a + b)                                 // static: receiver dropped
//! }
//! fn times(_env: &mut jni::Env, (this, f): (JObject, i32)) -> JavaResult<i32> {
//!     let base: i32 = this.get_field("base")?;   // instance: `this` is the first element
//!     Ok(base * f)
//! }
//!
//! # fn main() -> JavaResult<()> {
//! let java = Java::builder().build()?;
//! let clazz = java.class("com.example.NativeLib")?;
//! clazz.register_natives(&[
//!     native!("add", add)?,                 // type-derived: no signature string
//!     native_inst!("times", times)?,        // instance: `this` is the first element
//!     native!("addLong", |env, (a, b): (i64, i64)| -> JavaResult<i64> {
//!         let _ = env;
//!         Ok(a + b)                         // closures work too
//!     })?,
//! ])?;
//!
//! // Now Java (and Rust) calls dispatch into Rust:
//! let sum: i32 = clazz.call_static("add", (2, 3))?;   // -> 5
//! let obj = clazz.new_instance(())?;
//! obj.set_field("base", 10)?;
//! let r: i32 = obj.call("times", (2,))?;              // -> 20
//! # Ok(()) }
//! ```
//!
//! Rules and mechanics:
//!
//! * The Rust function signature is
//!   `fn(&mut jni::Env, args_tuple) -> JavaResult<R>` where `args_tuple` is
//!   `()` or `(T1, …, Tn)` (n ≤ 64) with `T: FromJava`, and `R` is one of the
//!   single-value [`ToJava`] types: `()`, `bool`, `i8`, `i16`, `i32`, `i64`,
//!   `f32`, `f64`, `char`, `String`, [`JObject`], [`JClass`], [`JArray`],
//!   `Vec<T>`, or `Option<T>` of any of those.
//! * **Type-derived form** (primary): `native!("add", add)` — **no signature
//!   string**. The JNI descriptor is derived at runtime from the Rust types
//!   via the `ToJava`/`FromJava` machinery; the C ABI is fixed at compile
//!   time by shared generic trampolines in `rjava` itself (one instantiation
//!   per signature). Two *different* registrations with the same Rust
//!   signature share one trampoline and are rejected at
//!   [`JClass::register_natives`] with a clear error; the explicit-signature
//!   form below is the escape hatch. **`JObject`-typed returns derive
//!   `Ljava/lang/Object;`** (and `Vec<JObject>` / `Option<JObject>` derive
//!   `[Ljava/lang/Object;`), which is deliberately generic: if the Java
//!   method returns a *specific* class (e.g. a factory returning `Counter`)
//!   or a concrete array type (e.g. `String[]`), `register_natives` detects
//!   the resulting `NoSuchMethodError` at registration time, resolves the
//!   exact return type via reflection, and re-registers — no explicit
//!   signature needed. Reference-typed `Vec<T>` returns are exact by
//!   contrast (`Vec<String>` derives `[Ljava/lang/String;`), so they never
//!   hit the fallback. The explicit-signature form is never auto-corrected.
//! * **Explicit-signature form** (escape hatch): `native!("add", "(II)I", add)`
//!   — the JNI signature as a **plain quoted string**, the same form JNI
//!   uses: `"(II)I"`, `"(Ljava/lang/String;)Ljava/lang/String;"`, `"([D)D"`,
//!   `"()V"`. It is parsed at compile time (a small hand-written parser in
//!   the proc-macro crate) so a malformed signature is a compile error, and
//!   each invocation generates a *unique* trampoline — use it when two
//!   natives share a Rust signature. Class names may use `/` or `.` (dots are
//!   converted to slashes); `V` is only valid as the return type. Limits:
//!   the type-derived form covers argument tuples up to 64 elements (63
//!   declared parameters for `native_inst!` — the receiver takes the 64th
//!   slot); the explicit form accepts any signature within the JVM's own
//!   method-descriptor limit of 255 parameter units (JVMS §4.3.3:
//!   `long`/`double` count as two units; an instance method's `this`
//!   consumes one of the 255, leaving 254). Real-world methods rarely exceed
//!   ~20 parameters.
//! * `native!` registers a **static** method (the raw `JClass` receiver the
//!   JVM passes is dropped); `native_inst!` registers an **instance** method
//!   and prepends the `this` object to the argument list, so the first tuple
//!   element receives a [`JObject`] handle.
//! * Exceptions: returning `Err(JavaError::JavaException { class, message })`
//!   throws that class with that message; any other `Err` (and any **panic**)
//!   throws a `java.lang.RuntimeException` carrying the error / panic payload.
//!   Panics are caught at the FFI boundary and never unwind into the JVM.
//! * A function whose argument tuple does not match the signature (arity
//!   mismatch) produces a clear error at the first call.
//! * The `unsafe` lives entirely in the `rjava-helper` crate (the two JNI
//!   calls behind `RegisterNatives`); the macro *expansions* are
//!   `unsafe`-free, so `rjava` itself stays `#![forbid(unsafe_code)]` and so
//!   can user crates. The expansions reference `::rjava::…`, so **the
//!   dependency must be named `rjava`**.
//! * Inside a native implementation you can call back into Java with
//!   [`Java::from_env`]: `let java = Java::from_env(env)?;`.
//!
//! # Typed class bindings (`bind!`)
//!
//! The dynamic call path is string-typed: `java.new_object("java.lang.StringBuilder", ("Hello",))?`
//! then `sb.call("length", ())?`. For a class you use a lot, [`bind!`] turns
//! it into a compile-time-typed binding: declare the class, the wrapper name
//! and the methods once, and every call becomes a direct typed method:
//!
//! ```no_run
//! use rjava::prelude::*;
//! use rjava::bind;
//!
//! bind! {
//!     "java.lang.StringBuilder" => StringBuilder {
//!         fn append(s: &str) -> Self;   // chainable: re-wraps the same object
//!         fn length() -> i32;
//!         fn toString() -> String;
//!     }
//! }
//!
//! # fn example() -> JavaResult<()> {
//! # let java = Java::builder().build()?;
//! let sb = java.new::<StringBuilder>(("Hello",))?;
//! sb.append(" world")?;
//! let len: i32 = sb.length()?;         // == 11
//! # Ok(()) }
//! ```
//!
//! The JNI signatures are computed at **macro time** from the declared Rust
//! types (mirroring the `ToJava`/`FromJava` machinery); `static fn`
//! declarations map to Java static methods (their generated method takes
//! `java: &Java` as the first parameter); `-> Self` re-wraps the returned
//! object. The class reference is cached per wrapper (a `OnceLock` over a
//! JNI global reference — `Send + Sync`, no `unsafe`), method IDs are
//! re-derived per call (jni-rs `MethodID` is `Env`-lifetime-bound), and the
//! exact-signature reflection fallback applies when a declared `JObject` (or
//! a wrong declared type) does not resolve directly. The expansion is
//! `unsafe`-free. See the `bind` module for the full guide
//! and the type-mapping table.
//!
//! # Rust-backed Java objects (userdata)
//!
//! The mlua `UserData` analog: a Java object whose **state lives in Rust**.
//! Java code calls a static factory — native constructors are illegal in Java
//! source, the JLS forbids `native` on constructors — and receives a normal
//! Java object whose instance methods are `native`. The Rust implementations
//! look the state up in a process-global registry keyed by the object's own
//! identity (`System.identityHashCode`), so the shell class needs **no
//! handle field**, and Java code can even construct the shell with plain
//! `new`, with the host binding state to it later. A constructor can bind
//! the state itself — its **body** calls a native `init()` that binds state
//! to `this`, so `new X()` directly yields a fully-backed object (the
//! "direct new" pattern; the constructor itself cannot be `native`, the JLS
//! forbids it). See the [userdata module docs](crate::userdata).
//!
//! ```java
//! // Counter.java — a plain Java class, no fields.
//! public class Counter {
//!     public static native Counter create();
//!     public native long increment(long by);
//!     public native long value();
//! }
//! ```
//!
//! ```rust,no_run
//! # use parking_lot::Mutex;
//! # use rjava::prelude::*;
//! # use rjava::{native, native_inst};
//! # struct Counter(Mutex<i64>);
//! # impl Counter {
//! #     fn new() -> Self { Counter(Mutex::new(0)) }
//! #     fn increment(&self, by: i64) -> i64 { let mut c = self.0.lock(); *c += by; *c }
//! #     fn value(&self) -> i64 { *self.0.lock() }
//! # }
//! fn counter_create(env: &mut jni::Env, (): ()) -> JavaResult<JObject> {
//!     rjava::userdata::create_shell(env, "com.example.Counter", Counter::new())
//! }
//!
//! fn counter_increment(env: &mut jni::Env, (this, by): (JObject, i64)) -> JavaResult<i64> {
//!     let counter = rjava::userdata::get::<Counter>(env, &this)?;
//!     Ok(counter.increment(by))
//! }
//!
//! # fn counter_value(env: &mut jni::Env, (this,): (JObject,)) -> JavaResult<i64> {
//! #     let counter = rjava::userdata::get::<Counter>(env, &this)?;
//! #     Ok(counter.value())
//! # }
//!
//! # fn main() -> JavaResult<()> {
//! # let java = Java::builder().build()?;
//! # let clazz = java.class("com.example.Counter")?;
//! # clazz.register_natives(&[
//! #     // `create` returns a concrete class (`Counter`): the type-derived
//! #     // form derives `()Ljava/lang/Object;` and register_natives resolves
//! #     // the exact return type via reflection at registration time.
//! #     native!("create", counter_create)?,
//! #     native_inst!("increment", counter_increment)?,
//! #     native_inst!("value", counter_value)?,
//! # ])?;
//! # let counter: JObject = clazz.call_static("create", ())?;
//! # let n: i64 = counter.call("increment", (5_i64,))?;
//! # assert_eq!(n, 5);
//! # Ok(()) }
//! ```
//!
//! Semantics: the state demonstrably lives in Rust — `increment(5)` then
//! `increment(3)` then `value()` yields `8`; the shell carries no data of its
//! own. The registry is keyed by `System.identityHashCode(obj)` (identity
//! collisions are detected and refused, see [`userdata`]); bindings hold a
//! **weak** global reference, so a bound object is *not* pinned — when a
//! shell becomes unreachable and is GC'd, its binding (and the Rust state)
//! is released automatically by a background cleaner thread (and lazily on
//! any userdata API call), so no `unbind` is needed for garbage-collected
//! shells. [`userdata::unbind`] remains for explicit release. The registry
//! is `Mutex`-guarded and the state is shared as `Arc<T>` (`Send + Sync`);
//! you pick the synchronization inside.
//!
//! # Plugin workflow (runtime class loading)
//!
//! The JVM's system class path is fixed when the JVM is created, so classes
//! cannot be added to it at runtime. To load an API jar and plugin jars at
//! runtime, load them into a [`JClassLoader`] — a handle over a
//! `java.net.URLClassLoader` — and look classes up through it:
//!
//! ```no_run
//! # use rjava::prelude::*;
//! # fn plugin_workflow(java: &Java) -> JavaResult<()> {
//! // API jar: interfaces + a Bridge class declaring `native` methods — the
//! // compile-time contract plugin developers build against.
//! let api = java.load_jar("target/plugin-api.jar")?;
//! let bridge = api.load_class("com.example.api.Bridge")?;
//! bridge.register_natives(&[/* native!("echo", "(Ljava/lang/String;)Ljava/lang/String;", echo)? */])?;
//! // Plugin jars: loaded with the API loader as parent, so plugin code
//! // resolves the API classes (and the Rust-backed natives) through it.
//! let plugin = java.class_loader_with_parent(&["target/plugin.jar"], &api)?;
//! let hello = plugin.load_class("com.example.plugin.Hello")?;
//! let name: String = hello.new_instance(())?.call("name", ())?;
//! # Ok(()) }
//! ```
//!
//! See the README's plugin-workflow section for the full picture.
//!
//! # Concept mapping (mlua → rjava)
//!
//! | mlua                    | rjava                                   |
//! |-------------------------|-----------------------------------------|
//! | `mlua::Lua`             | [`Java`] (JVM facade)                   |
//! | `mlua::IntoLua`         | [`ToJava`] (Rust → JNI arguments)       |
//! | `mlua::FromLua`         | [`FromJava`] (JNI value → Rust)         |
//! | `MultiValue`            | tuples (`()`, `(a, b)`, … up to 64)     |
//! | `mlua::Error`           | [`JavaError`]                           |
//! | `Lua::create_function`  | `JClass::register_natives` + [`native!`] / [`native_inst!`] |
//! | `mlua::UserData`        | [`userdata`] (`bind` / `get` / `unbind` / `create_shell`) + [`native_inst!`] |
//!
//! # Thread model
//!
//! [`Java`] is `Send + Sync`, and every public method automatically attaches
//! the calling thread to the JVM for the duration of the call (permanently —
//! the thread is detached automatically when it terminates, following the
//! `jni` crate's `attach_current_thread` semantics). You never attach by hand
//! in normal use.
//!
//! For explicit control, [`Java::attach_thread`] returns a [`JavaThread`]
//! RAII guard that detaches the thread when dropped.
//!
//! # Error model
//!
//! One error type, [`JavaError`]:
//!
//! * `Jni(jni::errors::Error)` — a raw JNI failure.
//! * `JavaException { class, message }` — a pending Java exception; **captured
//!   and cleared** before the call returns, so one failed call never poisons
//!   the thread for the next.
//! * `InvalidArgument(&'static str)` — a misuse of the API (null where a value
//!   was required, wrong type annotation, bad class name, …).
//! * `JvmStart(String)` — the JVM could not be created or located.
//! * `WithContext { source, operation }` — a dynamic call failed and the
//!   error names the operation (`calling parseInt(String) on
//!   java.lang.Integer`); `source` is the underlying error. `Display`
//!   renders like the Java exception (`java.lang.NumberFormatException:
//!   For input string: "x"` for `JavaException`).
//!
//! # Feature notes
//!
//! * `rjava` requires the `jni` crate's `invocation` feature (enabled by
//!   default here) so it can create a JVM from Rust. `JavaVM::new` returns the
//!   already-created JVM if one exists, so repeated `builder().build()` calls
//!   are harmless.
//! * `Java::run_main` runs a Java `public static void main(String[])` entry
//!   point — thin sugar over the existing call machinery
//!   ([`Java::run_main`]).
//! * The `serde` feature is opt-in: Rust structs ⇄ Java `Map` value trees via
//!   the `JavaMap` wrapper and `rjava::serde::from_object`, and Rust structs
//!   ⇄ plain Java beans via `JavaBean` and `rjava::bean::JavaBean::from_object`
//!   (getter/setter reflection — module docs enabled with the feature).
//! * [`crate::future`] bridges `java.util.concurrent.CompletableFuture` to
//!   Rust `Future`s — std-only, no executor.
//! * [`crate::rx`] (feature `interface`) bridges Java event/listener
//!   callbacks to Rust `Future`s — std-only, no executor, one-shot.
//! * [`Java::call_async`] / [`Java::call_static_async`] run a method call
//!   on a worker and return a Rust future — std-only (one std thread per
//!   call). With the optional `tokio` feature, awaiting inside a tokio
//!   runtime dispatches the call through
//!   [`tokio::task::spawn_blocking`](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html)
//!   instead; outside any runtime the std-thread fallback applies, so the
//!   feature is safe to enable for non-tokio users. [`crate::future`]
//!   itself stays std-only either way.
//! * **No unsigned integers.** Java has no `u32`/`u64` types, so `ToJava`/
//!   `FromJava` are deliberately *not* implemented for them (nor for `u16` —
//!   use [`char`], which maps to the Java `char`). `Vec<u8>` maps to `byte[]`
//!   via `i8` casts (see `Vec<i8>`); `Vec<T>` otherwise maps to `T[]` for
//!   every [`JavaVecElement`] — primitives, `u8`, and reference types
//!   (`String[]` ⇄ `Vec<String>`, `Class[]`, `Object[]`, arrays of arrays),
//!   with `Option<T>` elements for null-tolerant object arrays.
//! * Strings are converted **eagerly** to Rust `String` (there is no public
//!   `JString` handle type), including non-ASCII text; `\0` and surrogate
//!   pairs are handled correctly by the underlying MUTF-8 conversion.
//!
//! # How the JVM is located
//!
//! The JVM shared library (`jvm.dll` on Windows, `libjvm.so` on Linux,
//! `libjvm.dylib` on macOS) is found by the [java-locator] crate, which
//! consults `JAVA_HOME` first, then the `java` executable on `PATH` (and the
//! Windows registry). Set `JAVA_HOME` to a **JDK** (not a JRE) to be sure.
//! On Windows it also helps to have `%JAVA_HOME%\bin` on `PATH` so the JVM's
//! own dependencies resolve.
//!
//! [mlua]: https://docs.rs/mlua
//! [`jni`]: https://docs.rs/jni
//! [java-locator]: https://docs.rs/java-locator

#![forbid(unsafe_code)]
#![warn(missing_docs)]

// Make the crate addressable as `::rjava::…` from its own code: the
// `native!`/`native_inst!` macro expansions (generated by rjava-macros)
// reference every rjava item through that path, and the shell natives of the
// `interface` feature are registered from inside this crate.
extern crate self as rjava;

mod array;
mod call;
mod classloader;
mod convert;
mod error;
mod handles;
mod java;

/// The native-method machinery (safe dispatch, `NativeMethod`, `NativeArgs`,
/// …). Hidden from the docs; used by the `native!` / `native_inst!` macro
/// expansions.
#[doc(hidden)]
pub mod native;

pub use crate::array::{ArrayKind, JavaArrayElement};
/// The element-kind tag behind the unified `Vec<T>` conversions; hidden
/// plumbing (the trait is sealed — implement nothing for it yourself).
#[doc(hidden)]
pub use crate::array::{JavaVecElement, VecKind};
pub use crate::classloader::JClassLoader;
pub use crate::convert::{FromJava, JavaArg, ToJava};
pub use crate::error::{JavaError, JavaResult};
pub use crate::handles::{JArray, JClass, JObject, JavaThread};
pub use crate::java::{Java, JvmConfig};
pub use crate::native::NativeMethod;

/// Rust-backed Java objects: bind Rust state to a Java object, keyed by the
/// object's own identity (`System.identityHashCode`) — the mlua `UserData`
/// analog. See the [module docs](crate::userdata) for the pattern, the
/// collision caveat and the GC semantics.
pub mod userdata;

/// Optional serde integration (feature `serde`): convert Rust structs ⇄
/// `java.util.HashMap` / `java.util.ArrayList` value trees via serde's
/// `Serialize`/`Deserialize`, through the [`JavaMap`](crate::serde::JavaMap)
/// wrapper and [`from_object`](crate::serde::from_object). See the
/// [module docs](crate::serde) for the type mapping and the error contract.
#[cfg(feature = "serde")]
pub mod serde;

/// Optional serde integration (feature `serde`): map Rust structs ⇄ plain
/// Java beans via getter/setter reflection, through the
/// [`JavaBean`](crate::bean::JavaBean) wrapper and
/// [`JavaBean::from_object`](crate::bean::JavaBean::from_object). See the
/// [module docs](crate::bean) for the supported field types, the camelCase
/// rule and the loud-error contract.
#[cfg(feature = "serde")]
pub mod bean;

/// Implement Java interfaces from Rust (feature `interface`): zero
/// user-written Java implementation classes — the JDK's
/// `java.lang.reflect.Proxy` generates the implementation at runtime, and
/// one fixed precompiled shell class routes every call to a Rust closure.
/// See the [module docs](mod@crate::interface) for the pattern and the
/// exception rules; building with the feature needs no JDK — the one shell
/// class is precompiled and committed.
#[cfg(feature = "interface")]
pub mod interface;

/// Bridge Java event/listener callbacks to Rust `Future`s (feature
/// `interface`): build a listener as an
/// [`interface::proxy`](crate::interface::proxy) handler and await its
/// first event. std-only, no executor. See the [module docs](crate::rx).
#[cfg(feature = "interface")]
pub mod rx;

/// Bridge `java.util.concurrent.CompletableFuture` → Rust `Future`s (std-only, no executor or helper classes). See [`crate::future`].
pub mod future;

/// Re-export of the [`jni`] crate, so the `native!` / `native_inst!` macro
/// expansions (which are generated in *your* crate) can reach the raw JNI
/// types (`::rjava::jni::Env`, `::rjava::jni::sys::jint`, …) through the
/// `rjava` path they reference.
///
/// [`jni`]: https://docs.rs/jni
pub use jni;

/// The `native!` macro: register a Rust function as a Java **static** native
/// method. Two forms: `native!("add", f)` (type-derived — no signature
/// string; closures and fn items both work) and `native!("add", "(II)I", f)`
/// (explicit-signature escape hatch). See the [crate docs](crate) for the
/// full guide.
pub use rjava_macros::native;

/// The `native_inst!` macro: register a Rust function as a Java **instance**
/// native method (the `this` receiver is the first tuple element). Two forms:
/// `native_inst!("times", f)` (type-derived) and
/// `native_inst!("times", "(I)I", f)` (explicit-signature escape hatch). See
/// the [crate docs](crate) for the full guide.
pub use rjava_macros::native_inst;

/// The `async_native!` macro: register a Rust **async** function as a Java
/// native method whose Java-visible return type is
/// `java.util.concurrent.CompletableFuture` — Java code awaits the Rust
/// future, and Rust code can `await` the same work back through
/// [`crate::future::java_future`] (the full circle). The registered function
/// returns a `CompletableFuture` handle; see the
/// [`future`] module docs for the contract and the
/// [`native!`] type-derived rules it shares.
pub use rjava_macros::async_native;

/// The `bind!` macro: declare a compile-time-typed binding for a Java class
/// — the class name, a wrapper name, and the methods with their Rust types.
/// JNI signatures are computed at macro time and every call becomes a direct
/// typed method. See the `bind` module docs for the full guide and
/// the type-mapping table.
pub use rjava_macros::bind;

/// The `interface!` macro (feature `interface`): implement a Java interface
/// from Rust with a typed `trait` — the mirror of `bind!` for the implement
/// side. Declare the interface once with Rust types; the macro generates a
/// Rust trait, you implement it for your state struct, and
/// [`proxy_typed`](crate::interface::proxy_typed) builds the
/// generic handler behind the scenes. See the `interface` module docs for
/// the full guide.
#[cfg(feature = "interface")]
pub use rjava_macros::interface;

/// Compile-time-typed Java class bindings (the `bind!` macro's runtime
/// support): the [`JavaBound`](crate::bind::JavaBound) trait, the class
/// cache and the call helpers behind every generated wrapper.
pub mod bind;

/// Everything you need to get started: the facade, the handles, the error
/// type and the conversion traits.
pub mod prelude {
    pub use crate::{
        Java, JavaError, JavaResult, JavaThread, JArray, JClass, JClassLoader, JObject, FromJava,
        ToJava,
    };
    pub use crate::bind::JavaBound;
}
