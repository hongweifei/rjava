//! Implement Java interfaces from Rust (feature `interface`) — the mlua
//! `create_function` analog for Java, with **zero user-written Java
//! implementation classes**.
//!
//! # How it works
//!
//! The JDK's own `java.lang.reflect.Proxy` generates the interface
//! implementation at runtime — a JVM mechanism, not rjava codegen. rjava
//! ships exactly **one fixed precompiled shell class**,
//! `rjava.shell.InvocationHandlerShell` (a `java.lang.reflect.InvocationHandler`),
//! whose `invoke` method is `native` and routes every proxied method call to
//! the Rust closure the host registered. The closure's captured state lives in
//! rjava's [userdata registry](crate::userdata) (identity-keyed, released
//! automatically when the proxy — and therefore the shell — becomes
//! unreachable). The shell's `.class` bytes are precompiled once (with
//! `javac --release 8` for maximum JVM compatibility — class-file version
//! 52, Java 8) and **committed** to the repo at
//! `interface/java/rjava/shell/InvocationHandlerShell.class`, then embedded
//! in the library; building with this feature needs **no `javac` at all**.
//! At first use the bytes are written to a per-process temp directory and
//! loaded through a `URLClassLoader`, so nothing is ever generated or
//! compiled at runtime.
//!
//! # Usage
//!
//! ```no_run
//! use rjava::interface;
//! use rjava::jni::JValueOwned;
//! use rjava::prelude::*;
//!
//! # fn example() -> JavaResult<()> {
//! # let java = Java::builder().class_path("target/classes").build()?;
//! // Java side — the interface is ordinary Java, nothing rjava-specific:
//! //   public interface Greeter { String greet(String name); int add(int a, int b); }
//!
//! // Rust side — one closure implements every method, dispatched on the
//! // method name (and, for overloads, on `call.param_types`). The
//! // `Arc<dyn Handler>` parameter makes the closure's signature infer:
//! // no lifetime annotations needed.
//! use std::sync::Arc;
//! let proxy = interface::proxy(
//!     Arc::new(|env: &mut jni::Env, call: interface::Call| {
//!         let mut args = call.args.into_iter();
//!         match call.name.as_str() {
//!             "greet" => {
//!                 let who = String::from_java(env, args.next().expect("greet takes 1 arg"))?;
//!                 let out = JValueOwned::Object(env.new_string(format!("Hello, {who}"))?.into());
//!                 Ok(interface::box_value(env, out)?)
//!             }
//!             "add" => {
//!                 let a = i32::from_java(env, args.next().expect("add takes 2 args"))?;
//!                 let b = i32::from_java(env, args.next().expect("add takes 2 args"))?;
//!                 Ok(interface::box_value(env, JValueOwned::Int(a + b))?)
//!             }
//!             other => Err(JavaError::InvalidArgument("unexpected method")),
//!         }
//!     }),
//!     &["com.example.Greeter"],   // interface binary names
//! )?;
//!
//! // Hand the proxy to Java like any other object:
//! let greeting: String = java
//!     .class("com.example.Host")?
//!     .call_static("greet", (&proxy, "world"))?;
//! # Ok(()) }
//! ```
//!
//! Each proxied call arrives as a [`Call`](crate::interface::Call): the
//! method name, the declared parameter types (binary names —
//! `"java.lang.String"`, `"int"`,
//! `"[Ljava.lang.String;"` — read from `Method.getParameterTypes()` via
//! `Class.getName()`) and the owned argument values. Dispatch on
//! `call.name.as_str()`; use `call.param_types` to tell overloads apart
//! (`"add"` with `["int", "int"]` vs `["long", "long"]`). The argument
//! values are **owned**, because jni-rs's [`JValueOwned`](jni::JValueOwned)
//! is neither `Copy` nor `Clone`. Primitive parameters arrive **unboxed**
//! ([`JValueOwned::Bool`](jni::JValueOwned::Bool)/
//! [`Int`](jni::JValueOwned::Int)/… — rjava unboxes the wrapper the proxy
//! built, guided by the parameter types) and convert with the existing
//! [`FromJava`](crate::FromJava) impls
//! (`i32::from_java(env, call.args.into_iter().next()?)?`); reference
//! parameters arrive as [`JValueOwned::Object`](jni::JValueOwned::Object).
//! The closure returns a [`JObject`](crate::JObject) handle built with
//! [`box_value`](crate::interface::box_value) — which also boxes primitive
//! results into their wrapper classes, unboxed again by the proxy's
//! generated code for primitive-returning interface methods — or an error to
//! throw; `void` methods return [`null`](crate::interface::null).
//!
//! # What the library handles for you
//!
//! Three JDK-`Proxy` pitfalls are handled by the library, before the
//! handler is ever consulted:
//!
//! * **`Object` methods** — `toString()`, `hashCode()` and
//!   `equals(Object)` are intercepted with the JDK's own identity
//!   semantics: the standard `ClassName@hexhash` string (built in Rust),
//!   `System.identityHashCode(proxy)`, and reference identity. The
//!   interception matches the **exact declared signature** (name +
//!   parameter types), never the name alone, so a domain method named
//!   `toString(int)` still reaches the handler. These three are
//!   **library-reserved** in v1 — a future typed layer may make them
//!   customizable.
//! * **`default` methods** — auto-forwarded to their Java default
//!   implementation via `InvocationHandler.invokeDefault` (Java 16+),
//!   located reflectively so the shell class stays compiled with
//!   `--release 8`. On JVMs where `invokeDefault` does not exist
//!   (Java < 16) the default method is dispatched to the handler like any
//!   other method.
//!
//! # Typed interfaces (`interface!`)
//!
//! The closure handler above is the **dynamic** path: one closure dispatches
//! on `call.name` (and `call.param_types`). The [`interface!`](macro@crate::interface)
//! macro is the **typed** path — the mirror of `bind!` for the implement
//! side. Declare the interface once with Rust types; the macro generates a
//! Rust `trait`; you implement the trait for your state struct; and
//! [`proxy_typed`](crate::interface::proxy_typed) builds the generic handler behind the scenes — no manual
//! `JValue` conversion, no name-match dispatch:
//!
//! ```no_run
//! use std::sync::Arc;
//! use rjava::interface;
//! use rjava::prelude::*;
//!
//! rjava::interface! {
//!     "com.example.Greeter" => Greeter {       // generates: pub trait Greeter
//!         fn greet(name: String) -> String;
//!         fn add(a: i32, b: i32) -> i32;
//!         fn ping();                            // void -> JavaResult<()>
//!         fn apply(words: Vec<String>) -> Option<String>;
//!     }
//! }
//!
//! struct MyGreeter { prefix: String }
//! impl Greeter for MyGreeter {
//!     fn greet(&self, _env: &mut jni::Env, name: String) -> JavaResult<String> {
//!         Ok(format!("{}{name}", self.prefix))
//!     }
//!     fn add(&self, _env: &mut jni::Env, a: i32, b: i32) -> JavaResult<i32> {
//!         Ok(a + b)
//!     }
//!     fn ping(&self, _env: &mut jni::Env) -> JavaResult<()> {
//!         Ok(())
//!     }
//!     fn apply(&self, _env: &mut jni::Env, words: Vec<String>) -> JavaResult<Option<String>> {
//!         Ok(if words.is_empty() { None } else { Some(words.join(",")) })
//!     }
//! }
//!
//! # fn example() -> JavaResult<()> {
//! # let java = Java::builder().build()?;
//! let proxy: JObject = interface::proxy_typed::<dyn Greeter + Send + Sync>(
//!     Arc::new(MyGreeter { prefix: "Hi, ".into() }),
//!     &["com.example.Greeter"],
//! )?;
//! let greeting: String = java
//!     .class("com.example.Host")?
//!     .call_static("greet", (&proxy, "world"))?;
//! # Ok(()) }
//! ```
//!
//! Every `interface!` expansion generates:
//!
//! * a `pub trait` — one typed method per declared method, each taking
//!   `&self`, the [`jni::Env`](jni::Env), the declared parameter types, and returning
//!   `JavaResult<R>` (`()` for a `void` method),
//! * a hidden [`InterfaceTrait`](crate::interface::InterfaceTrait) impl **on the trait object**
//!   (`dyn Greeter + Send + Sync` — a local self type, so the impl satisfies
//!   the orphan rule and any number of interfaces can be implemented by one
//!   state struct): the interface's binary name plus a generated `dispatch`
//!   that matches `(name, param_types)` against the declared methods,
//!   converts each argument with the declared type's [`FromJava`](crate::FromJava)
//!   implementation (no widening — a declared `i32` receives the unboxed
//!   `int`, never a widened `Long`), calls the trait method on `self`, and
//!   converts the return with [`ToJava`](crate::ToJava) + [`box_value`](crate::interface::box_value).
//!
//! [`proxy_typed`](crate::interface::proxy_typed) takes the state as `Arc<I>` — write
//! `proxy_typed::<dyn Greeter + Send + Sync>(Arc::new(state), &["com.example.Greeter"])`.
//! The state is shared across calls and must be `Send + Sync + 'static`.
//!
//! The type mapping reuses `bind!`'s: `bool`, `i8`/`u8`, `i16`, `i32`,
//! `i64`, `f32`, `f64`, `char`, `String`, `JObject`, `Option<T>` and
//! `Vec<T>` (minus `Self` and `&str`, which need an owned conversion the
//! dispatch cannot provide). A declared `JObject` parameter matches any
//! reference parameter type; every other declared type must match exactly.
//! `static fn` is not supported in v1 — static interface methods cannot be
//! trait items; keep them on the generic [`proxy`](crate::interface::proxy) handler path. The three
//! library-reserved `Object` methods must not be declared (the macro
//! rejects the exact intercepted signatures).
//!
//! A call whose `(name, param_types)` matches no declared method is a clear
//! error — a thrown `java.lang.RuntimeException` (a
//! [`JavaError::JavaException`](crate::JavaError::JavaException) to Rust callers) naming the method and the
//! arriving parameter types, exactly like the dynamic path's unknown-method
//! behavior. Everything else — the reserved `Object` interception and the
//! `default`-method auto-forward — runs before the handler, so it keeps
//! working unchanged for typed proxies.
//!
//! # Exceptions
//!
//! A handler error becomes a Java exception, mirroring the native-dispatch
//! rules:
//!
//! * [`Err(JavaError::JavaException{ class, message })`](JavaError::JavaException)
//!   → throw an instance of `class` with `message` (Java sees it; callers on
//!   the Java side can catch it, Rust callers get the exception back);
//! * any other `Err` → a `java.lang.RuntimeException` carrying the error's
//!   [`Debug`] rendering;
//! * a handler panic → a `java.lang.RuntimeException` ("native method
//!   panicked: …").
//!
//! # Feature and build requirements
//!
//! `interface` is an optional feature, off by default (like `serde`).
//! Building with it needs **no JDK**: the one shell class is precompiled
//! (`javac --release 8`) and committed at
//! `interface/java/rjava/shell/InvocationHandlerShell.class`, and its bytes
//! are embedded at compile time. A regression test recompiles the `.java`
//! (when `javac` is available) and fails if the committed `.class` is stale
//! — regenerate the `.class` whenever the `.java` changes.
//!
//! # Limits
//!
//! * Every proxy needs **at least one interface**; the interfaces should be
//!   visible from one classloader (they usually all are — the system
//!   classloader or the loader of the first interface).
//! * This is a safe wrapper over JNI: everything here is `unsafe`-free, like
//!   the rest of rjava.

use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};

use jni::{Env, JValue, JValueOwned};

use crate::call;
use crate::classloader::JClassLoader;
use crate::convert::{JavaArg, ToJava};
use crate::error::{JavaError, JavaResult};
use crate::handles::{with_env, JClass, JObject};
use crate::java::Java;
use crate::userdata;
use crate::{native_inst};

/// The embedded `.class` bytes of `rjava.shell.InvocationHandlerShell`,
/// precompiled (`javac --release 8`, class-file version 52) and committed
/// at `interface/java/rjava/shell/InvocationHandlerShell.class`, embedded
/// at compile time. This is the one Java artifact the whole feature needs;
/// the JVM's `Proxy` machinery does the interface implementation at
/// runtime.
const SHELL_CLASS_BYTES: &[u8] =
    include_bytes!("../interface/java/rjava/shell/InvocationHandlerShell.class");

// ---------------------------------------------------------------------------
// The handler contract
// ---------------------------------------------------------------------------

/// One proxied interface-method call: the Java method name, its declared
/// parameter types, and the argument values.
///
/// * `name` — the Java method name (`"greet"`, `"run"`, …), from
///   `Method.getName()`.
/// * `param_types` — the declared parameter types in **binary-name** form
///   (`Class.getName()`), from `Method.getParameterTypes()`:
///   `"java.lang.String"` for a reference, `"int"` for a primitive, and
///   `"[Ljava.lang.String;"` for an array. Two overloads with the same name
///   differ here, so dispatch on `(call.name.as_str(),
///   call.param_types.as_slice())` to implement overloads.
/// * `args` — the argument values exactly as Java passed them, **owned** by
///   the closure: primitive parameters arrive **unboxed**
///   ([`JValueOwned::Int`] for an `int`, [`JValueOwned::Bool`] for a
///   `boolean`, …) — rjava unboxes the wrapper the proxy built, guided by
///   the parameter types — and reference parameters arrive as
///   [`JValueOwned::Object`] (local refs, valid for the duration of the
///   call). The values are passed as an owned [`Vec`] (not a slice) because
///   jni-rs's [`JValueOwned`] is neither `Copy` nor `Clone` — consume them
///   with `call.args.into_iter()`, and convert with the existing
///   [`FromJava`](crate::FromJava) impls:
///   `i32::from_java(env, call.args.into_iter().next()?)?`,
///   `String::from_java(env, call.args.into_iter().next()?)?`, …
///
/// The `Object` methods `toString()`, `hashCode()` and `equals(Object)` are
/// **library-reserved** — the library intercepts them before the handler
/// (see the [module docs](self)), so a handler never receives them; a
/// domain method with the same name but a different signature
/// (`toString(int)`) still arrives here.
pub struct Call<'env> {
    /// The Java method name (`Method.getName()`).
    pub name: String,
    /// The declared parameter types, binary names (`Class.getName()`).
    pub param_types: Vec<String>,
    /// The unboxed argument values, owned by the closure.
    pub args: Vec<JValueOwned<'env>>,
}

/// A Rust implementation of a Java interface: the closure invoked for every
/// proxied method call.
///
/// Signature: `(&mut Env, Call) -> result`.
///
/// * **The call** — a [`Call`] carrying the method name, the declared
///   parameter types (binary names) and the owned argument values; see its
///   docs for the details.
/// * **Result** — a [`JObject`] handle to return, or an error to throw (see
///   the [module docs](self) for the exception rules). Build it from a value
///   with [`box_value`] (which also boxes primitives into their wrappers —
///   the proxy's generated code unboxes them again for primitive-returning
///   interface methods) or from scratch; return [`null`] for `void` methods
///   (the JVM sees a `null` `Object`).
///
/// The return type is rjava's [`JObject`] (a `'static` global-reference
/// handle) rather than an env-tied [`JValueOwned`]: a Rust closure cannot
/// spell `for<'env> Fn(…) -> JavaResult<JValueOwned<'env>>` (closure
/// lifetimes are never tied to the return), so the closure hands back an
/// owned handle and the conversion to the JNI return value happens inside
/// the native call.
///
/// The closure must be `Send + Sync + 'static`; captured state (counters,
/// channels, whatever) is shared across calls and released when the proxy
/// becomes unreachable.
///
/// [`FromJava`]: crate::FromJava
pub type Handler = dyn for<'env> Fn(&mut Env<'env>, Call<'env>) -> JavaResult<JObject>
    + Send
    + Sync
    + 'static;

/// The Rust state bound to one shell: the user's handler closure.
struct HandlerBinding {
    handler: Arc<Handler>,
}

// ---------------------------------------------------------------------------
// Bootstrap: temp-dir class load + native registration (once per process)
// ---------------------------------------------------------------------------

/// The result of the one-time bootstrap: the temp-dir loader that can load
/// the shell class, and the loaded shell class itself (natives registered).
struct Bootstrap {
    /// The `URLClassLoader` over the per-process temp dir that holds the
    /// embedded shell `.class` bytes. Also the fallback proxy loader for
    /// bootstrap-loaded interfaces (its parent is the system classloader).
    loader: JClassLoader,
    /// The loaded `rjava.shell.InvocationHandlerShell` class, with its
    /// `init`/`invoke` natives registered.
    shell_class: JClass,
}

/// The one-time bootstrap, cached per process: writing the embedded `.class`
/// bytes to a per-process temp dir, loading the class through a
/// `URLClassLoader`, and registering its natives. `Err` carries the reason
/// (reported once; later callers see the same error).
static BOOTSTRAP: LazyLock<Result<Arc<Bootstrap>, String>> = LazyLock::new(bootstrap);

/// How many times the bootstrap has run — a diagnostic proving the temp-dir
/// write + class load + registration happens exactly once per process.
/// Hidden from the docs; the integration tests assert on it.
#[doc(hidden)]
pub fn bootstrap_count() -> usize {
    BOOTSTRAP_COUNT.load(Ordering::SeqCst)
}

static BOOTSTRAP_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Run the one-time bootstrap; see [`BOOTSTRAP`]. Increments
/// [`BOOTSTRAP_COUNT`] on entry so the "once per process" property is
/// observable.
fn bootstrap() -> Result<Arc<Bootstrap>, String> {
    BOOTSTRAP_COUNT.fetch_add(1, Ordering::SeqCst);
    let result = (|| -> JavaResult<Arc<Bootstrap>> {
        let java = with_env(Java::from_env)?;
        // Per-process temp dir: the `.class` is a fixed build-time artifact,
        // and the `rjava-<pid>` prefix keeps concurrent processes from
        // racing each other's writes.
        let dir = std::env::temp_dir().join(format!("rjava-interface-{}", std::process::id()));
        let class_file = dir.join("rjava").join("shell").join("InvocationHandlerShell.class");
        let parent = class_file
            .parent()
            .expect("the shell class file path always has a parent");
        std::fs::create_dir_all(parent).map_err(|e| {
            JavaError::JvmStart(format!(
                "rjava::interface: cannot create the temp class directory `{}`: {e}",
                parent.display()
            ))
        })?;
        std::fs::write(&class_file, SHELL_CLASS_BYTES).map_err(|e| {
            JavaError::JvmStart(format!(
                "rjava::interface: cannot write the shell class to `{}`: {e}",
                class_file.display()
            ))
        })?;
        // A URLClassLoader over a *directory* is the existing
        // `Java::class_loader` contract — no API extension needed.
        let dir_str = dir.to_string_lossy().into_owned();
        let loader = java.class_loader(&[dir_str])?;
        let shell_class = loader.load_class("rjava.shell.InvocationHandlerShell")?;
        // Explicit-signature natives: each gets a unique trampoline, so the
        // interface feature never occupies (or collides with) the shared
        // type-derived trampoline slots of user registrations.
        shell_class.register_natives(&[
            native_inst!("init", "()V", shell_init)?,
            native_inst!(
                "invoke",
                "(Ljava/lang/Object;Ljava/lang/reflect/Method;[Ljava/lang/Object;)Ljava/lang/Object;",
                shell_invoke
            )?,
        ])?;
        Ok(Arc::new(Bootstrap { loader, shell_class }))
    })();
    result.map_err(|e| e.to_string())
}

/// The cached bootstrap result, or a user-facing error.
///
/// [`JvmStart`](JavaError::JvmStart) carries the (dynamic) bootstrap
/// failure detail — a temp-dir write problem, a class-load failure, or a
/// native-registration failure.
fn bootstrap_arc() -> JavaResult<Arc<Bootstrap>> {
    BOOTSTRAP
        .as_ref()
        .map(Arc::clone)
        .map_err(|e| {
            JavaError::JvmStart(format!("rjava::interface: bootstrap failed: {e}"))
        })
}

// ---------------------------------------------------------------------------
// The ctor-bind natives (registered on the shell class)
// ---------------------------------------------------------------------------

// The handler the *next* `new InvocationHandlerShell()` on this thread must
// bind. A thread-local rather than a global so concurrent proxies on
// different threads never interfere; `init()` runs on the same thread as
// the `new` call (JNI calls are synchronous), so no lock is needed.
thread_local! {
    static PENDING_HANDLER: RefCell<Option<Arc<Handler>>> = const { RefCell::new(None) };
}

/// `InvocationHandlerShell.init()` — the ctor-bind: binds the pending
/// handler state to the shell under construction (see the userdata module
/// docs for the pattern). Constructing a shell without a pending handler
/// (i.e. anywhere but inside `proxy`) is a misuse and fails with a clear
/// error.
fn shell_init(env: &mut Env<'_>, (this,): (JObject,)) -> JavaResult<()> {
    let handler = PENDING_HANDLER
        .with(|slot| slot.borrow_mut().take())
        .ok_or_else(|| {
            JavaError::InvalidArgument(
                "rjava::interface: an InvocationHandlerShell was constructed outside \
                 `interface::proxy`, so there is no pending Rust handler to bind; \
                 construct shells only through `proxy`",
            )
        })?;
    userdata::bind(env, &this, HandlerBinding { handler })
}

/// `InvocationHandlerShell.invoke(Object, Method, Object[])` — routes one
/// proxied interface method call to the Rust handler bound to this shell.
///
/// 1. the handler state is fetched from the userdata registry,
/// 2. the method name comes from `Method.getName()` and the declared
///    parameter types from `Method.getParameterTypes()` (binary names),
/// 3. the three library-reserved `Object` methods — `toString()`,
///    `hashCode()`, `equals(Object)` — are intercepted with identity
///    semantics (see [`intercept_object_method`]),
/// 4. a `default` method is auto-forwarded to its Java default
///    implementation (see [`invoke_default`]),
/// 5. the `Object[]` arguments become local [`JValueOwned`]s — primitive
///    parameters are **unboxed** so the handler sees `JValueOwned::Int` for
///    an `int`, not an `Integer` object (the proxy always boxes); reference
///    parameters stay [`JValueOwned::Object`]. `Proxy` passes `null` for
///    the array of zero-parameter methods, which becomes the empty `Vec`.
/// 6. the handler is called with a [`Call`]; its error/panic becomes a
///    thrown Java exception through the standard native-dispatch rules (the
///    trampoline's [`throw_error`](crate::native::throw_error)),
/// 7. the returned [`JObject`] is handed back to the JVM (the trampoline
///    converts it into the native return value on the way out).
fn shell_invoke(
    env: &mut Env<'_>,
    (this, proxy, method, args): (JObject, JObject, JObject, Option<JObject>),
) -> JavaResult<JObject> {
    let binding = userdata::get::<HandlerBinding>(env, &this)?;
    let name: String = method.call("getName", ())?;
    let param_types = param_types_of(env, &method)?;

    // The library-reserved Object methods (exact signatures only) and the
    // default-method auto-forward both come *before* the user handler.
    if let Some(value) = intercept_object_method(env, &proxy, &name, &param_types, args.as_ref())? {
        return Ok(value);
    }
    if is_default_method(env, &method)?
        && let Some(value) = invoke_default(env, &proxy, &method, args.as_ref())?
    {
        return Ok(value);
    }
    // `InvocationHandler.invokeDefault` does not exist (Java < 16): fall
    // back to dispatching the default method to the user handler (the
    // pre-harden behavior).

    let arg_values = build_arg_values(env, args.as_ref(), &param_types)?;
    let value = (binding.handler)(env, Call {
        name,
        param_types,
        args: arg_values,
    })?;
    // The handler's `JObject` is a global-reference handle; the trampoline
    // converts it into the JNI return value (a local ref) on the way out.
    Ok(value)
}

/// The declared parameter types of the proxied method, in binary-name form
/// (`Class.getName()`): `"java.lang.String"`, `"int"`,
/// `"[Ljava.lang.String;"`, … — read once per call from
/// `Method.getParameterTypes()`. Doubles as the unboxing guide: the eight
/// primitive names select the wrapper-unboxing method for the arguments.
fn param_types_of<'env>(
    env: &mut Env<'env>,
    method: &JObject,
) -> JavaResult<Vec<String>> {
    let local = env.new_local_ref(&*method.global)?;
    let v = call::with_check(env, |env| {
        env.call_method(
            &local,
            jni::jni_str!("getParameterTypes"),
            jni::jni_sig!("()[Ljava/lang/Class;"),
            &[],
        )
    })?;
    let arr = match v {
        JValueOwned::Object(o) => o,
        _ => {
            return Err(JavaError::InvalidArgument(
                "rjava::interface: Method.getParameterTypes() did not return an array",
            ))
        }
    };
    let array: jni::objects::JObjectArray<'_> =
        jni::objects::JObjectArray::<jni::objects::JObject>::cast_local(env, arr)?;
    let n = array.len(env)?;
    let mut types = Vec::with_capacity(n);
    for i in 0..n {
        let cls = array.get_element(env, i)?;
        types.push(call::class_name_of(env, &cls)?);
    }
    Ok(types)
}

/// The three library-reserved `Object` methods — `toString()`, `hashCode()`
/// and `equals(Object)` — intercepted before the user handler, with the
/// JDK's own identity semantics:
///
/// * `toString()` → `proxy.getClass().getName() + "@" +
///   Integer.toHexString(System.identityHashCode(proxy))` (the standard
///   `Object.toString` format; the string is built in Rust),
/// * `hashCode()` → `System.identityHashCode(proxy)` boxed as an
///   `Integer`,
/// * `equals(Object)` → `proxy == arg` boxed as a `Boolean` (reference
///   identity, consistent with the hash).
///
/// Interception is by the **exact declared signature** (name + parameter
/// types), never by the name alone, so a domain method named `toString(int)`
/// — or `equals` with a non-`Object` parameter — still reaches the handler.
/// These three are library-reserved in v1; a future typed layer may make
/// them customizable.
///
/// Returns `Ok(Some(handle))` when the call was a reserved method (the
/// handle is the result to return to the JVM), `Ok(None)` otherwise.
fn intercept_object_method<'env>(
    env: &mut Env<'env>,
    proxy: &JObject,
    name: &str,
    param_types: &[String],
    args: Option<&JObject>,
) -> JavaResult<Option<JObject>> {
    match name {
        "toString" if param_types.is_empty() => {
            let local = env.new_local_ref(&*proxy.global)?;
            let class = call::get_object_class(env, &local)?;
            let class_name = call::class_name(env, &class)?;
            let hash = identity_hash_code(env, &local)?;
            let s = env.new_string(format!("{class_name}@{hash:x}"))?;
            Ok(Some(box_value(env, JValueOwned::Object(s.into()))?))
        }
        "hashCode" if param_types.is_empty() => {
            let local = env.new_local_ref(&*proxy.global)?;
            let hash = identity_hash_code(env, &local)?;
            Ok(Some(box_value(env, JValueOwned::Int(hash))?))
        }
        "equals" if param_types == ["java.lang.Object"] => {
            let local = env.new_local_ref(&*proxy.global)?;
            let arg = sole_argument(env, args)?;
            let same = call::with_check(env, |env| env.is_same_object(&local, &arg))?;
            Ok(Some(box_value(env, JValueOwned::Bool(same))?))
        }
        _ => Ok(None),
    }
}

/// `System.identityHashCode(obj)` — the identity hash backing the reserved
/// `hashCode()`/`toString()` implementations (identical to what the JDK's
/// own `Object.hashCode` produces for the proxy).
fn identity_hash_code<'env>(
    env: &mut Env<'env>,
    obj: &jni::objects::JObject<'env>,
) -> JavaResult<i32> {
    let system = env.find_class(jni::jni_str!("java/lang/System"))?;
    let v = call::with_check(env, |env| {
        env.call_static_method(
            &system,
            jni::jni_str!("identityHashCode"),
            jni::jni_sig!("(Ljava/lang/Object;)I"),
            &[JValue::Object(obj)],
        )
    })?;
    match v {
        JValueOwned::Int(hash) => Ok(hash),
        _ => Err(JavaError::InvalidArgument(
            "rjava::interface: System.identityHashCode did not return an int",
        )),
    }
}

/// The sole argument of `equals(Object)` as a local reference — `null`
/// when the argument array is missing or its element is `null`.
fn sole_argument<'env>(
    env: &mut Env<'env>,
    args: Option<&JObject>,
) -> JavaResult<jni::objects::JObject<'env>> {
    let Some(args) = args else {
        return Ok(jni::objects::JObject::null());
    };
    let local = env.new_local_ref(&*args.global)?;
    let array: jni::objects::JObjectArray<'_> =
        jni::objects::JObjectArray::<jni::objects::JObject>::cast_local(env, local)?;
    let n = array.len(env)?;
    if n != 1 {
        return Err(JavaError::InvalidArgument(
            "rjava::interface: equals(Object) received an argument array of length != 1",
        ));
    }
    Ok(array.get_element(env, 0)?)
}

/// Is the proxied method a `default` method? (`Method.isDefault()`.)
fn is_default_method<'env>(env: &mut Env<'env>, method: &JObject) -> JavaResult<bool> {
    let local = env.new_local_ref(&*method.global)?;
    let v = call::with_check(env, |env| {
        env.call_method(&local, jni::jni_str!("isDefault"), jni::jni_sig!("()Z"), &[])
    })?;
    match v {
        JValueOwned::Bool(is_default) => Ok(is_default),
        _ => Err(JavaError::InvalidArgument(
            "rjava::interface: Method.isDefault() did not return a boolean",
        )),
    }
}

/// Forward a `default` method to its Java default implementation via
/// `InvocationHandler.invokeDefault` (Java 16+), located and invoked
/// reflectively so the shell class stays compiled with `--release 8`.
///
/// Returns `Ok(None)` when `InvocationHandler.invokeDefault` does not exist
/// (Java < 16); the caller then falls back to dispatching the default
/// method to the user handler.
fn invoke_default<'env>(
    env: &mut Env<'env>,
    proxy: &JObject,
    method: &JObject,
    args: Option<&JObject>,
) -> JavaResult<Option<JObject>> {
    // Locate `InvocationHandler.invokeDefault(Object, Method, Object[])`
    // via reflection — `Class.getMethod("invokeDefault", Object.class,
    // Method.class, Object[].class)`. On JVMs older than 16 this throws
    // NoSuchMethodException, the fallback signal.
    let invoke_default = {
        let ih_class = env.find_class(jni::jni_str!("java/lang/reflect/InvocationHandler"))?;
        let name = env.new_string("invokeDefault")?;
        let object_cls = env.find_class(jni::jni_str!("java/lang/Object"))?;
        let method_cls = env.find_class(jni::jni_str!("java/lang/reflect/Method"))?;
        let object_arr_cls = env.find_class(jni::jni_str!("[Ljava/lang/Object;"))?;
        let class_cls = env.find_class(jni::jni_str!("java/lang/Class"))?;
        let types_arr = env.new_object_array(3, &class_cls, jni::objects::JObject::null())?;
        types_arr.set_element(env, 0, &object_cls)?;
        types_arr.set_element(env, 1, &method_cls)?;
        types_arr.set_element(env, 2, &object_arr_cls)?;
        // `Class.getMethod(String, Class<?>...)` is an *instance* method on
        // the `InvocationHandler` class object.
        let v = call::with_check(env, |env| {
            env.call_method(
                &ih_class,
                jni::jni_str!("getMethod"),
                jni::jni_sig!("(Ljava/lang/String;[Ljava/lang/Class;)Ljava/lang/reflect/Method;"),
                &[JValue::Object(&name), JValue::Object(&types_arr)],
            )
        });
        match v {
            Ok(JValueOwned::Object(m)) => m,
            Ok(_) => {
                return Err(JavaError::InvalidArgument(
                    "rjava::interface: Class.getMethod did not return a Method",
                ))
            }
            Err(JavaError::JavaException { class, .. })
                if class == "java.lang.NoSuchMethodException" =>
            {
                // Java < 16: `InvocationHandler.invokeDefault` is absent.
                return Ok(None);
            }
            Err(e) => {
                // Anything other than NoSuchMethodException is unexpected
                // here; surface it with context (a `JavaException` keeps its
                // class).
                return Err(match e {
                    JavaError::JavaException { class, message } => {
                        let message = if message.is_empty() {
                            format!(
                                "rjava::interface: cannot locate \
                                 InvocationHandler.invokeDefault ({class})"
                            )
                        } else {
                            format!(
                                "rjava::interface: cannot locate \
                                 InvocationHandler.invokeDefault: {message}"
                            )
                        };
                        JavaError::JavaException { class, message }
                    }
                    other => other,
                });
            }
        }
    };

    // Invoke it with (proxy, method, args) — `Method.invoke(null, ...)`,
    // since `invokeDefault` is static. The call is done on raw JNI rather
    // than through `call::with_check` because `Method.invoke` wraps an
    // exception thrown by the default method's body in an
    // InvocationTargetException, which we unwrap so the caller sees the
    // real exception.
    let proxy_local = env.new_local_ref(&*proxy.global)?;
    let method_local = env.new_local_ref(&*method.global)?;
    let args_local = match args {
        Some(a) => env.new_local_ref(&*a.global)?,
        None => jni::objects::JObject::null(),
    };
    let null_receiver = jni::objects::JObject::null();
    let object_cls = env.find_class(jni::jni_str!("java/lang/Object"))?;
    let varargs = env.new_object_array(3, &object_cls, jni::objects::JObject::null())?;
    varargs.set_element(env, 0, &proxy_local)?;
    varargs.set_element(env, 1, &method_local)?;
    varargs.set_element(env, 2, &args_local)?;
    let result = env.call_method(
        &invoke_default,
        jni::jni_str!("invoke"),
        jni::jni_sig!("(Ljava/lang/Object;[Ljava/lang/Object;)Ljava/lang/Object;"),
        &[
            JValue::Object(&null_receiver),
            JValue::Object(&varargs),
        ],
    );
    match result {
        Ok(JValueOwned::Object(o)) => Ok(Some(JObject::from_global(env.new_global_ref(&o)?))),
        Ok(JValueOwned::Void) => Ok(Some(null())),
        Ok(_) => Err(JavaError::InvalidArgument(
            "rjava::interface: invokeDefault returned a non-object value",
        )),
        Err(_) if env.exception_check() => {
            let throwable = match env.exception_occurred() {
                Some(t) => t,
                None => {
                    return Err(JavaError::InvalidArgument(
                        "rjava::interface: reflective invokeDefault failed with no pending exception",
                    ))
                }
            };
            env.exception_clear();
            // If the default method's body threw, `Method.invoke` wrapped it
            // in InvocationTargetException: surface the *cause* so the
            // caller sees the real exception.
            let ite_class =
                env.find_class(jni::jni_str!("java/lang/reflect/InvocationTargetException"))?;
            let cause = if env.is_instance_of(&*throwable, &ite_class)? {
                let c = call::with_check(env, |env| {
                    env.call_method(
                        &*throwable,
                        jni::jni_str!("getCause"),
                        jni::jni_sig!("()Ljava/lang/Throwable;"),
                        &[],
                    )
                })?;
                match c {
                    JValueOwned::Object(o) if !o.is_null() => o,
                    // No cause (the ITE itself is the failure); surface it.
                    _ => jni::objects::JObject::from(throwable),
                }
            } else {
                jni::objects::JObject::from(throwable)
            };
            let class = call::get_object_class(env, &cause)?;
            let class_name = call::class_name(env, &class)?;
            let message = throwable_message(env, &cause)?;
            Err(JavaError::JavaException {
                class: class_name,
                message,
            })
        }
        Err(e) => {
            // A genuine JNI-level failure with no pending exception (a
            // reflection-call plumbing error, not a default-method body
            // exception); surface the raw jni error.
            Err(JavaError::Jni(e))
        }
    }
}

/// The `Throwable.getMessage()` of a throwable as a Rust string, `""` when
/// the message is `null`.
fn throwable_message<'env>(
    env: &mut Env<'env>,
    throwable: &jni::objects::JObject<'env>,
) -> JavaResult<String> {
    let v = call::with_check(env, |env| {
        env.call_method(
            throwable,
            jni::jni_str!("getMessage"),
            jni::jni_sig!("()Ljava/lang/String;"),
            &[],
        )
    })?;
    match v {
        JValueOwned::Object(o) if !o.is_null() => {
            let s = env.cast_local::<jni::objects::JString>(o)?;
            Ok(s.mutf8_chars(env)?.into())
        }
        _ => Ok(String::new()),
    }
}

/// The owned argument values for the handler, unboxing proxy-boxed
/// primitive wrappers guided by `param_types` (the binary names of the
/// declared parameter types). `Proxy` passes `null` for the argument array
/// of zero-parameter methods — that is an empty argument list.
fn build_arg_values<'env>(
    env: &mut Env<'env>,
    args: Option<&JObject>,
    param_types: &[String],
) -> JavaResult<Vec<JValueOwned<'env>>> {
    let Some(args) = args else {
        return Ok(Vec::new());
    };
    let local = env.new_local_ref(&*args.global)?;
    let array: jni::objects::JObjectArray<'_> =
        jni::objects::JObjectArray::<jni::objects::JObject>::cast_local(env, local)?;
    let n = array.len(env)?;
    if n != param_types.len() {
        return Err(JavaError::InvalidArgument(
            "rjava::interface: the argument array length does not match the method's parameter count",
        ));
    }
    let mut values = Vec::with_capacity(n);
    for (i, param) in param_types.iter().enumerate() {
        let o = array.get_element(env, i)?;
        let value = if call::is_primitive_name(param) {
            // A primitive parameter: unbox the (never-null) wrapper the
            // proxy built.
            unbox_wrapper(env, &o, param)?
        } else {
            // A reference parameter: hand the object through as-is.
            JValueOwned::Object(o)
        };
        values.push(value);
    }
    Ok(values)
}

/// Unbox a proxy-boxed primitive argument via `Wrapper.xxxValue()` — an
/// `Integer` → `intValue()`. The proxy's generated code always boxes
/// primitive arguments into their exact wrapper class, so the parameter
/// name directly selects the unboxing method.
fn unbox_wrapper<'env>(
    env: &mut Env<'env>,
    o: &jni::objects::JObject<'env>,
    param: &str,
) -> JavaResult<JValueOwned<'env>> {
    call::with_check(env, |env| match param {
        "boolean" => env.call_method(o, jni::jni_str!("booleanValue"), jni::jni_sig!("()Z"), &[]),
        "byte" => env.call_method(o, jni::jni_str!("byteValue"), jni::jni_sig!("()B"), &[]),
        "char" => env.call_method(o, jni::jni_str!("charValue"), jni::jni_sig!("()C"), &[]),
        "short" => env.call_method(o, jni::jni_str!("shortValue"), jni::jni_sig!("()S"), &[]),
        "int" => env.call_method(o, jni::jni_str!("intValue"), jni::jni_sig!("()I"), &[]),
        "long" => env.call_method(o, jni::jni_str!("longValue"), jni::jni_sig!("()J"), &[]),
        "float" => env.call_method(o, jni::jni_str!("floatValue"), jni::jni_sig!("()F"), &[]),
        "double" => env.call_method(o, jni::jni_str!("doubleValue"), jni::jni_sig!("()D"), &[]),
        // primitive_param_flags only passes primitive names; this is unreachable.
        _ => Err(jni::errors::Error::WrongObjectType),
    })
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Implement one or more Java interfaces with a single Rust closure and
/// return the JDK-generated proxy object.
///
/// # Parameters
///
/// * `handler` — the [`Handler`] closure implementing every interface
///   method (dispatch on `call.name` and, for overloads, on
///   `call.param_types`), with its captured state, as an [`Arc`]. The
///   concrete `Arc<Handler>` parameter type is deliberate: the coercion
///   target lets rustc infer the closure's higher-ranked signature from it,
///   so the closure can be written as
///   `Arc::new(|env: &mut jni::Env, call: Call| { … })` without naming any
///   lifetimes. The handler is shared (not cloned) — the same `Arc` may
///   back several proxies.
/// * `interfaces` — the binary names of the interfaces to implement
///   (`"com.example.Greeter"`, `"java.lang.Runnable"`,
///   `"java.util.function.Function"`, …). At least one is required.
///
/// # How it works
///
/// 1. On first use, the embedded shell class is written to a per-process
///    temp dir, loaded through a `URLClassLoader`, and its natives are
///    registered — once per process, cached forever.
/// 2. A shell instance is constructed; its constructor runs the native
///    `init()`, which binds `handler` to the shell in the userdata registry
///    (identity-keyed, auto-released when the proxy becomes unreachable).
/// 3. `Proxy.newProxyInstance` generates the interface implementation over
///    the shell. The proxy's defining loader is the first interface's own
///    classloader (so the proxy can always see its interfaces); a
///    bootstrap-loaded interface (`java.lang.Runnable`, …) has no
///    classloader, and the shell loader — whose parent is the system
///    classloader — is used instead.
///
/// The returned object implements all the given interfaces; hand it to Java
/// code like any other object.
///
/// # Errors
///
/// * [`JavaError::JvmStart`] — the one-time bootstrap failed (temp-dir
///   write, class load, or native registration).
/// * [`JavaError::InvalidArgument`] — no interfaces given, or a name that
///   does not resolve to a class.
/// * [`JavaError::JavaException`] — `Proxy.newProxyInstance` refused the
///   setup (e.g. an interface the chosen loader cannot see, or a handler
///   that is not an `InvocationHandler`).
///
/// The handler's own errors/panics are thrown into the *calling* Java code
/// at call time, not here.
pub fn proxy(handler: Arc<Handler>, interfaces: &[impl AsRef<str>]) -> JavaResult<JObject> {
    let bootstrap = bootstrap_arc()?;
    if interfaces.is_empty() {
        return Err(JavaError::InvalidArgument(
            "rjava::interface::proxy requires at least one interface to implement",
        ));
    }

    // 1. Construct the shell. The constructor calls the native `init()`,
    //    which consumes the armed pending handler and binds it to the shell
    //    (ctor-bind pattern). The slot is cleared on every path out of the
    //    block: on success `init()` already took it; on failure the handler
    //    must not leak into the next construction on this thread.
    let shell = {
        PENDING_HANDLER.with(|slot| *slot.borrow_mut() = Some(handler));
        let result = bootstrap.shell_class.new_instance(());
        PENDING_HANDLER.with(|slot| {
            slot.borrow_mut().take();
        });
        result?
    };

    // 2. Load the interfaces by name.
    let java = with_env(Java::from_env)?;
    let ifaces: Vec<JClass> = interfaces
        .iter()
        .map(|name| java.class(name.as_ref()))
        .collect::<JavaResult<_>>()?;

    // 3. The proxy class must be defined in a loader that can see every
    //    interface; the first interface's own classloader trivially can.
    //    Bootstrap-loaded interfaces have no classloader — fall back to the
    //    shell loader (parent: the system classloader).
    let loader = loader_for(&ifaces, &bootstrap)?;

    // 4. Proxy.newProxyInstance(loader, interfaces, shell).
    let proxy_class = java.class("java.lang.reflect.Proxy")?;
    proxy_class.call_static("newProxyInstance", (&loader, ifaces, shell))
}

/// The classloader to define the proxy in: the first interface's own
/// loader (`Class.getClassLoader()`), or — when that is `null` (a
/// bootstrap-loaded interface) — the shell loader.
fn loader_for(ifaces: &[JClass], bootstrap: &Bootstrap) -> JavaResult<JClassLoader> {
    let first = ifaces.first().expect("proxy checked that interfaces is non-empty");
    with_env(|env| {
        let local = env.new_local_ref(&*first.global)?;
        let v = call::with_check(env, |env| {
            env.call_method(
                &local,
                jni::jni_str!("getClassLoader"),
                jni::jni_sig!("()Ljava/lang/ClassLoader;"),
                &[],
            )
        })?;
        match v {
            JValueOwned::Object(o) if !o.is_null() => Ok(JClassLoader::from_handle(
                JObject::from_global(env.new_global_ref(o)?),
            )),
            _ => Ok(bootstrap.loader.clone()),
        }
    })
}

/// A null [`JObject`] handle — the return value for `void` interface
/// methods (the proxy's generated code discards it) and for methods that
/// return a Java `null`.
///
/// Wraps jni-rs's [`Global::null`](jni::objects::Global::null) — a null
/// global reference — so constructing it needs no JNI call and it is
/// `'static` like every other handle.
pub fn null() -> JObject {
    JObject::from_global(jni::objects::Global::null())
}

/// Convert a [`JValueOwned`] — as produced by a handler or built from
/// scratch — into the [`JObject`] handle the handler must return.
///
/// * [`JValueOwned::Object`] — wrapped into a global-reference handle (a
///   `null` object stays null);
/// * [`JValueOwned::Void`] — becomes [`null`];
/// * a primitive — boxed into its wrapper class via `Wrapper.valueOf`
///   (`JValueOwned::Int(v)` → `Integer.valueOf(v)`), which the proxy's
///   generated code unboxes again for primitive-returning interface methods.
///
/// The returned handle is `'static` and owned; the JVM-side conversion into
/// the native return value happens inside the shell's `invoke`.
pub fn box_value<'env>(env: &mut Env<'env>, value: JValueOwned<'env>) -> JavaResult<JObject> {
    let local = match value {
        JValueOwned::Object(o) => o,
        JValueOwned::Void => return Ok(null()),
        JValueOwned::Bool(v) => call::box_primitive(env, &JavaArg::Bool(v))?,
        JValueOwned::Byte(v) => call::box_primitive(env, &JavaArg::Byte(v))?,
        JValueOwned::Char(v) => call::box_primitive(env, &JavaArg::Char(v))?,
        JValueOwned::Short(v) => call::box_primitive(env, &JavaArg::Short(v))?,
        JValueOwned::Int(v) => call::box_primitive(env, &JavaArg::Int(v))?,
        JValueOwned::Long(v) => call::box_primitive(env, &JavaArg::Long(v))?,
        JValueOwned::Float(v) => call::box_primitive(env, &JavaArg::Float(v))?,
        JValueOwned::Double(v) => call::box_primitive(env, &JavaArg::Double(v))?,
    };
    Ok(JObject::from_global(env.new_global_ref(local)?))
}

// ---------------------------------------------------------------------------
// The typed `interface!` layer
// ---------------------------------------------------------------------------

/// The contract every [`interface!`](macro@crate::interface) expansion
/// implements for its generated trait.
///
/// `#[doc(hidden)]` — the macro generates `impl InterfaceTrait for
/// dyn Callback + Send + Sync` (the *trait object* is the self type, so the
/// impl is local and the orphan rule is satisfied), providing the interface's
/// binary name and the full dispatch: a generated `match` over
/// `(name, param_types)` that converts the arguments with the declared
/// types' [`FromJava`](crate::FromJava) impls and calls the corresponding
/// trait method on `self` (no type erasure, no name-match dispatch in user
/// code). User code never implements this directly.
#[doc(hidden)]
pub trait InterfaceTrait {
    /// The interface's binary name (`Class.getName()` form), e.g.
    /// `"com.example.Greeter"`.
    const INTERFACE_NAME: &'static str;

    /// Dispatch one proxied method call to the interface's trait
    /// implementation: match `(name, param_types)` against the declared
    /// methods (the no-widening guarantee — see [`proxy_typed`]), convert
    /// the arguments per the declared types, call the trait method on
    /// `self`, and box the return. A call matching no declared method is a
    /// clear error naming the method and its parameter types.
    ///
    /// The `'env` lifetime ties the `Env` to the argument values it borrows
    /// (they are local references of the same JNI call).
    fn dispatch<'env>(
        &self,
        env: &mut Env<'env>,
        name: &str,
        param_types: &[String],
        args: Vec<JValueOwned<'env>>,
    ) -> JavaResult<JObject>;
}

/// Does the arriving call's `(name, param_types)` match one declared method?
/// Name, arity and the declared parameter types must match — **exact** for
/// every declared type except a declared `JObject` (stored as
/// `"java.lang.Object"`), which matches any *reference* parameter type
/// (never a primitive). This is the no-widening guarantee: a call whose Java
/// parameter types differ from the declared ones (e.g. an `add(long, long)`
/// arriving at a declared `add(int, int)`) matches nothing and errors,
/// exactly like the handler path's `param_types` dispatch.
#[doc(hidden)]
pub fn param_matches(declared: &[&str], actual: &[String]) -> bool {
    declared.len() == actual.len()
        && declared
            .iter()
            .zip(actual)
            .all(|(declared, actual)| {
                *declared == "java.lang.Object" && !call::is_primitive_name(actual)
                    || *declared == actual
            })
}

/// Convert a Rust value into an owned JNI value — the reverse of
/// [`FromJava`](crate::FromJava), built on [`ToJava`](crate::ToJava): a
/// scalar's single `JavaArg` becomes the matching [`JValueOwned`] variant
/// (primitives stay unboxed, objects become local references), and `()` — the
/// void return — becomes [`JValueOwned::Void`].
///
/// `#[doc(hidden)]` — used by the generated `interface!` dispatch to convert
/// a trait method's return value before [`box_value`] wraps it.
#[doc(hidden)]
pub fn to_value<'env, R: ToJava>(env: &mut Env<'env>, value: R) -> JavaResult<JValueOwned<'env>> {
    let mut args = value.to_java(env)?;
    Ok(match args.pop() {
        Some(JavaArg::Object(o)) => JValueOwned::Object(o),
        Some(JavaArg::Bool(v)) => JValueOwned::Bool(v),
        Some(JavaArg::Byte(v)) => JValueOwned::Byte(v),
        Some(JavaArg::Char(v)) => JValueOwned::Char(v),
        Some(JavaArg::Short(v)) => JValueOwned::Short(v),
        Some(JavaArg::Int(v)) => JValueOwned::Int(v),
        Some(JavaArg::Long(v)) => JValueOwned::Long(v),
        Some(JavaArg::Float(v)) => JValueOwned::Float(v),
        Some(JavaArg::Double(v)) => JValueOwned::Double(v),
        // `()` (the void return) converts to an empty argument list.
        None => JValueOwned::Void,
    })
}

/// Implement a Java interface from Rust with a **typed** trait — the
/// [`interface!`](macro@crate::interface) analog of [`proxy`].
///
/// The macro generated the trait `I` (and the [`InterfaceTrait`] impl on its
/// *trait object*). `I` is the trait object type — write
/// `dyn Callback + Send + Sync` for an `interface!` named `Callback` — and
/// the state is passed as `Arc<I>` (coerce `Arc::new(state)`; the state must
/// implement the generated trait, which the coercion enforces). This function
/// builds the same shell + handler path as [`proxy`] behind the scenes; the
/// handler delegates every call to the generated [`InterfaceTrait::dispatch`],
/// which:
///
/// 1. matches the arriving `(name, param_types)` against the declared
///    methods — no match is a clear error (see below);
/// 2. converts each argument with the **declared** parameter type's
///    [`FromJava`](crate::FromJava) impl — no widening: the match already
///    required the exact Java parameter types, so a declared `i32`
///    parameter receives the unboxed `int`, never a widened `Long`;
/// 3. calls the trait method on the shared state (`Arc<I>`, `Send + Sync`);
/// 4. converts the return with [`ToJava`] +
///    [`box_value`] (`()` → `null`).
///
/// Because the handler runs through the same `shell_invoke` as [`proxy`],
/// the library-level behaviors keep working unchanged: the reserved
/// `Object` methods (`toString()`, `hashCode()`, `equals(Object)`) are
/// intercepted with identity semantics **before** the handler (so they must
/// not be declared in `interface!` — the macro rejects them), and `default`
/// methods are auto-forwarded to their Java default implementation before
/// the handler (declare only the methods you mean to implement; a declared
/// default method is still forwarded).
///
/// # Parameters
///
/// * `state` — `Arc<I>` where `I` is the generated trait's object type
///   (`Arc::new(your_state)` coerces to `Arc<dyn Trait + Send + Sync>`);
///   the state is shared across calls and must be `Send + Sync + 'static`.
/// * `interfaces` — the binary names of the interfaces the proxy implements;
///   the declared interface (the `interface!` class name) must be among them
///   or proxy creation fails with a clear error.
///
/// # Errors
///
/// * [`JavaError::InvalidArgument`] — the declared interface is not among
///   `interfaces` (a proxy that does not implement the declared interface
///   could never dispatch a declared method).
/// * Everything [`proxy`] can fail with — including a method call that
///   matches no declared method, which surfaces at call time as a thrown
///   `java.lang.RuntimeException` (a [`JavaError::JavaException`] to Rust
///   callers) naming the method and the arriving parameter types.
pub fn proxy_typed<I>(state: Arc<I>, interfaces: &[impl AsRef<str>]) -> JavaResult<JObject>
where
    I: ?Sized + InterfaceTrait + Send + Sync + 'static,
{
    let declared = I::INTERFACE_NAME.replace('/', ".");
    let present = interfaces.iter().any(|i| i.as_ref().replace('/', ".") == declared);
    if !present {
        return Err(JavaError::InvalidArgument(
            "rjava::interface::proxy_typed: the declared interface \
             (`interface!` class name) must be among the interfaces the proxy \
             implements — the generated trait's methods can only be dispatched \
             when the proxy implements the declared interface",
        ));
    }

    let handler: Arc<Handler> = Arc::new(move |env, call| {
        I::dispatch(&*state, env, &call.name, &call.param_types, call.args)
    });
    proxy(handler, interfaces)
}
