//! Rust-backed Java objects (the mlua `UserData` analog).
//!
//! # The pattern
//!
//! Java code calls a **static factory** — native constructors are illegal in
//! Java source, the JLS forbids `native` on constructors — and receives an
//! ordinary Java object, the *shell*. The shell's *state* lives in Rust: the
//! host binds a Rust value to the Java object with [`bind`](crate::userdata::bind), and the object's
//! instance methods — registered with [`crate::native_inst!`] — fetch the
//! state back with [`get`](crate::userdata::get) and call its methods. This is the mlua
//! [`UserData`](https://docs.rs/mlua/latest/mlua/trait.UserData.html) analog:
//! a Java object whose methods are backed by Rust data.
//!
//! # How the shell is identified
//!
//! There is **no `long` handle field**: the registry is keyed by the Java
//! object's own identity — `System.identityHashCode(obj)` (an `i32`), the
//! value behind the `xxxxxx` in `Object.toString()`'s `ClassName@xxxxxx`.
//! Consequences:
//!
//! * the shell class declares no extra fields — it is a plain Java class,
//!   and Java code can construct it with `new` itself, with the host binding
//!   state to it at any later point ([`bind`](crate::userdata::bind));
//! * the factory is just sugar: [`create_shell`](crate::userdata::create_shell) does `new` + [`bind`](crate::userdata::bind).
//!
//! # Collisions
//!
//! `identityHashCode` is 32-bit and *not* guaranteed unique. [`bind`](crate::userdata::bind)
//! detects a collision: if the key is already taken by a *different* live
//! object (checked with JNI `IsSameObject` against the stored weak
//! reference, resolved to the live object first), it fails with
//! [`JavaError::InvalidArgument`] instead of clobbering the existing
//! binding. [`get`](crate::userdata::get) and [`unbind`](crate::userdata::unbind) perform the same
//! identity check, so a colliding object never sees another object's state.
//! A binding whose shell has been garbage-collected is **dead** and no
//! longer guards its key — a new [`bind`](crate::userdata::bind) simply replaces it.
//! In practice collisions are vanishingly rare (they require two live objects
//! with the same identity hash).
//!
//! # GC semantics
//!
//! A bound object is **not pinned**: [`bind`](crate::userdata::bind) stores a JNI *weak* global
//! reference to the shell, so the Java object stays garbage-collectable
//! while bound. When the shell becomes unreachable and is collected, the
//! weak reference no longer resolves, the binding is *dead*, and its Rust
//! state is released automatically:
//!
//! * a background **cleaner thread** — spawned lazily on the first
//!   [`bind`](crate::userdata::bind), a plain `std::thread` that runs for the
//!   process lifetime — wakes roughly every 500 ms, attaches to the JVM, and
//!   removes dead bindings. It attaches with the `jni` crate's permanent
//!   attachment (`jni-rs` 0.22 removed the daemon-attach API, and `rjava`
//!   forbids `unsafe`, so raw `AttachCurrentThreadAsDaemon` is off the
//!   table); `rjava` never destroys the JVM, so the thread never blocks JVM
//!   shutdown. Errors are logged (`eprintln!`) and the loop continues;
//!   the loop never panics.
//! * [`bind`](crate::userdata::bind), [`get`](crate::userdata::get) and [`unbind`](crate::userdata::unbind) additionally drain dead
//!   bindings lazily on entry, so cleanup is prompt without waiting for the
//!   next tick.
//!
//! [`unbind`](crate::userdata::unbind) remains for **explicit** release (e.g. from the shell
//! class's `close()`/`dispose()` method); unreachable shells are released
//! automatically.
//!
//! # Thread safety
//!
//! The registry itself is a `Mutex` (parking_lot), so
//! [`bind`](crate::userdata::bind)/[`get`](crate::userdata::get)/[`unbind`](crate::userdata::unbind) are safe to call from any thread (the
//! cleaner thread takes the same lock briefly on every tick). The bound
//! state is shared as an [`Arc`](std::sync::Arc) — the type is `Send + Sync`, and *you*
//! choose the synchronization inside it (`Mutex`, `AtomicI64`, …) for
//! concurrent access.
//!
//! # Example
//!
//! Java:
//!
//! ```java
//! // Counter.java — a plain Java class, no handle field.
//! public class Counter {
//!     public static native Counter create();
//!     public native long increment(long by);
//!     public native long value();
//! }
//! ```
//!
//! Rust:
//!
//! ```no_run
//! # use parking_lot::Mutex;
//! # use rjava::prelude::*;
//! # use rjava::{native, native_inst};
//! struct Counter(Mutex<i64>);
//!
//! fn counter_create(env: &mut jni::Env, (): ()) -> JavaResult<JObject> {
//!     rjava::userdata::create_shell(env, "com.example.Counter", Counter(Mutex::new(0)))
//! }
//!
//! fn counter_increment(env: &mut jni::Env, (this, by): (JObject, i64)) -> JavaResult<i64> {
//!     // `with` is the closure sugar over `get`: fetch the state, borrow it
//!     // for the closure, return its result.
//!     rjava::userdata::with::<Counter, _>(env, &this, |counter| {
//!         let mut count = counter.0.lock();
//!         *count += by;
//!         *count
//!     })
//! }
//!
//! fn counter_value(env: &mut jni::Env, (this,): (JObject,)) -> JavaResult<i64> {
//!     // `with(env, &this, |c| *c.0.lock())` instead of
//!     // `Ok(get::<Counter>(env, &this)?.0.lock())`.
//!     rjava::userdata::with::<Counter, _>(env, &this, |counter| *counter.0.lock())
//! }
//!
//! # fn main() -> JavaResult<()> {
//! # let java = Java::builder().build()?;
//! # let clazz = java.class("com.example.Counter")?;
//! # clazz.register_natives(&[
//! #     // `create` returns a concrete class (`Counter`): the type-derived
//! #     // form derives `()Ljava/lang/Object;` (R = JObject), and
//! #     // register_natives resolves the exact return type via reflection
//! #     // at registration time — no explicit signature needed.
//! #     native!("create", counter_create)?,
//! #     native_inst!("increment", counter_increment)?,
//! #     native_inst!("value", counter_value)?,
//! # ])?;
//! # let counter: JObject = clazz.call_static("create", ())?;
//! # let v: i64 = counter.call("increment", (5_i64,))?;
//! # assert_eq!(v, 5);
//! # Ok(()) }
//! ```
//!
//! The state demonstrably lives in Rust: `increment(5)` then `increment(3)`
//! then `value()` yields `8` — the Java shell carries no data of its own.
//!
//! # Binding from a constructor (the "direct `new`" pattern)
//!
//! The factory is not the only way in: a Java constructor can bind the state
//! itself. The constructor is plain Java — a `native` constructor is illegal
//! in Java source, the JLS forbids `native` on constructors, `javac` rejects
//! it, and class-file rewriting is out of scope — so the constructor's
//! **body** calls a native `init()` method that binds the state to `this`
//! with [`bind`](crate::userdata::bind). Java-side `new X()` then directly
//! yields a fully-backed object: no factory, no post-construction `bind`.
//!
//! ```java
//! // DirectCounter.java
//! public class DirectCounter {
//!     public DirectCounter() { init(); }   // plain Java ctor calls the native binder
//!     private native void init();          // binds Rust state to `this`
//!     public native long add(int by);      // returns the new value
//! }
//! ```
//!
//! ```no_run
//! # use parking_lot::Mutex;
//! # use rjava::prelude::*;
//! # use rjava::native_inst;
//! struct DirectCounter(Mutex<i64>);
//!
//! fn counter_init(env: &mut jni::Env, (this,): (JObject,)) -> JavaResult<()> {
//!     rjava::userdata::bind(env, &this, DirectCounter(Mutex::new(0)))
//! }
//!
//! fn counter_add(env: &mut jni::Env, (this, by): (JObject, i32)) -> JavaResult<i64> {
//!     let counter = rjava::userdata::get::<DirectCounter>(env, &this)?;
//!     let mut count = counter.0.lock();
//!     *count += by as i64;
//!     Ok(*count)
//! }
//!
//! # fn main() -> JavaResult<()> {
//! # let java = Java::builder().build()?;
//! # let clazz = java.class("DirectCounter")?;
//! # clazz.register_natives(&[
//! #     native_inst!("init", counter_init)?,
//! #     native_inst!("add", counter_add)?,
//! # ])?;
//! # let counter: JObject = java.new_object("DirectCounter", ())?;
//! # let v: i64 = counter.call("add", (5_i32,))?;
//! # assert_eq!(v, 5);
//! # Ok(()) }
//! ```
//!
//! The constructor runs `init()` during `new`, so the object is bound before
//! `new X()` returns and `add` finds the state with [`get`](crate::userdata::get). Register
//! the natives **before** constructing: a `native` method with no registered
//! implementation makes the JVM throw `java.lang.UnsatisfiedLinkError` when
//! the constructor calls it. The factory ([`create_shell`](crate::userdata::create_shell))
//! remains the alternative for classes that should not bind in the
//! constructor.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::Duration;

use jni::objects::{Global, JClass as JniClass, JObject as JniObject};
use jni::objects::Weak;
use jni::strings::JNIString;
use jni::{Env, JavaVM};
use parking_lot::Mutex;

use crate::call;
use crate::error::{JavaError, JavaResult};
use crate::handles::JObject;
use crate::java::Java;

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// One bound shell: the Java object (referenced *weakly*, so it stays
/// GC-able) and its Rust state.
struct Binding {
    /// A weak global reference to the Java shell. A weak ref does **not**
    /// pin the object: once the shell is garbage-collected it no longer
    /// resolves, and the binding is dead. While the shell is alive it lets
    /// [`bind`]/[`get`]/[`unbind`] verify — with JNI `IsSameObject` — that a
    /// registry key really belongs to the object being addressed
    /// (identity-hash collisions).
    obj: Weak<JniObject<'static>>,
    /// The Rust state behind the shell, shared as an `Arc` so every native
    /// call gets a cheap clone.
    state: Arc<dyn Any + Send + Sync>,
}

// Process-global registry: `System.identityHashCode(obj)` → binding. Entries
// live until [`unbind`], until their shell is garbage-collected (released by
// the cleaner thread / the lazy drain), or the process end.
static REGISTRY: LazyLock<Mutex<HashMap<i32, Binding>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `System.identityHashCode(obj)` — the object's stable identity value (the
/// `xxxxxx` of `Object.toString()`'s `ClassName@xxxxxx`).
fn identity_hash(env: &mut Env<'_>, obj: &JObject) -> JavaResult<i32> {
    let system = system_class(env)?;
    call::call_static_method(env, system, "identityHashCode", &(obj.clone(),))
}

/// The `java/lang/System` class, cached as a process-global global
/// reference: [`bind`]/[`get`]/[`unbind`] resolve `identityHashCode` on
/// every entry, and a per-call `FindClass` + fresh global ref dominated the
/// identity lookup. `Global` is `Send + Sync`, so the cache needs no
/// `unsafe`. The first caller does the lookup; the loser of the
/// `OnceLock::get_or_init` race simply drops its redundant ref.
static SYSTEM_CLASS: OnceLock<Global<JniClass<'static>>> = OnceLock::new();

fn system_class(env: &mut Env<'_>) -> JavaResult<&'static Global<JniClass<'static>>> {
    if let Some(class) = SYSTEM_CLASS.get() {
        return Ok(class);
    }
    let class = call::find_class(env, JNIString::from("java/lang/System"))?;
    let class = env.new_global_ref(class)?;
    Ok(SYSTEM_CLASS.get_or_init(|| class))
}

/// Build the `InvalidArgument` error for a failed [`get`].
///
/// The message is a `'static` literal: `JavaError::InvalidArgument` carries a
/// `&'static str`, so the (dynamic) object class and expected type cannot be
/// embedded without leaking; when the error escapes through a native call,
/// the [`Debug`] rendering thrown into Java still identifies the operation.
fn state_error(bound: bool) -> JavaError {
    JavaError::InvalidArgument(if bound {
        "userdata: this Java object's Rust state is not of the requested type — \
         call rjava::userdata::get with the exact type the state was bound with"
    } else {
        "userdata: this Java object has no Rust state bound — bind it first with \
         rjava::userdata::bind"
    })
}

/// Remove every binding whose shell has been garbage-collected (its weak
/// reference no longer resolves).
///
/// Runs under the registry lock; it needs an `Env` because resolving a weak
/// reference is a JNI call. Shared by the background cleaner thread and the
/// lazy drains at the top of [`bind`]/[`get`]/[`unbind`]. An error resolving
/// a weak reference is treated as "not provably dead" — the binding stays.
fn drain_dead(
    registry: &mut parking_lot::MutexGuard<'_, HashMap<i32, Binding>>,
    env: &mut Env<'_>,
) {
    let dead: Vec<i32> = registry
        .iter()
        .filter(|(_, binding)| matches!(binding.obj.is_garbage_collected(env), Ok(true)))
        .map(|(key, _)| *key)
        .collect();
    for key in dead {
        registry.remove(&key);
    }
}

// ---------------------------------------------------------------------------
// Cleaner thread (automatic release)
// ---------------------------------------------------------------------------

/// Spawned once on the first [`bind`]; the cleaner runs for the process
/// lifetime. The `OnceLock` only guards *spawning* — the thread itself is
/// never joined.
static CLEANER: OnceLock<()> = OnceLock::new();

/// Spawn the background cleaner thread on the first [`bind`] (best-effort:
/// a spawn failure is logged and the bindings still work, relying on the
/// lazy drain alone).
fn ensure_cleaner_thread(env: &mut Env<'_>) {
    CLEANER.get_or_init(|| {
        match env.get_java_vm() {
            Ok(vm) => {
                if let Err(e) = std::thread::Builder::new()
                    .name("rjava-userdata-cleaner".to_string())
                    .spawn(move || cleaner_loop(vm))
                    .map(|_| ())
                {
                    eprintln!("rjava userdata: failed to spawn the cleaner thread: {e}");
                }
            }
            Err(e) => eprintln!("rjava userdata: no JVM available for the cleaner thread: {e:?}"),
        }
    });
}

/// The cleaner loop: sleep ~500 ms, attach to the JVM, drain dead bindings,
/// repeat for the process lifetime. Errors are logged and the loop
/// continues; each iteration is wrapped so a panic cannot kill the thread.
fn cleaner_loop(vm: JavaVM) {
    loop {
        std::thread::sleep(Duration::from_millis(500));
        let result = std::panic::catch_unwind(|| -> JavaResult<()> {
            // Permanent attach; re-attaching an already-attached thread is
            // cheap. The thread never detaches and runs until process exit.
            vm.attach_current_thread(|env| {
                let mut registry = REGISTRY.lock();
                drain_dead(&mut registry, env);
                Ok(())
            })
        });
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("rjava userdata cleaner: error draining dead bindings: {e:?}"),
            Err(payload) => {
                eprintln!("rjava userdata cleaner: iteration panicked (continuing): {payload:?}")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Bind Rust `state` to the Java object `obj`, keying it by the object's own
/// identity (`System.identityHashCode`).
///
/// * If `obj` is already bound, the new `state` **replaces** the old one
///   (re-bind; `Ok(())`).
/// * If the identity-hash key is taken by a *different* live object, the
///   binding is refused with [`JavaError::InvalidArgument`] (a 32-bit
///   identity-hash collision, vanishingly rare). A binding whose shell was
///   garbage-collected is dead and never blocks a new bind.
///
/// The shell is **not** kept alive: [`bind`] stores a *weak* global
/// reference, so the Java object stays GC-able; when it is collected the
/// binding is released automatically (see the [module docs](self) for the
/// GC semantics). The first [`bind`] also spawns the background cleaner
/// thread.
pub fn bind(
    env: &mut Env<'_>,
    obj: &JObject,
    state: impl Any + Send + Sync + 'static,
) -> JavaResult<()> {
    ensure_cleaner_thread(env);
    let key = identity_hash(env, obj)?;
    let mut registry = REGISTRY.lock();
    drain_dead(&mut registry, env);
    if let Some(existing) = registry.get(&key) {
        // Resolve the weak ref first: a collected shell (dead binding) no
        // longer guards the key — the new bind replaces it silently.
        let same = match existing.obj.upgrade_local(env) {
            Ok(Some(local)) => env.is_same_object(&local, &*obj.global)?,
            // Dead (or unresolvable — the binding is unusable either way).
            Ok(None) | Err(_) => false,
        };
        if !same {
            return Err(JavaError::InvalidArgument(
                "userdata: identity-hash collision — a different live Java object with \
                 the same System.identityHashCode is already bound; unbind it first",
            ));
        }
    }
    let binding = Binding {
        obj: env.new_weak_ref(&*obj.global)?,
        state: Arc::new(state),
    };
    registry.insert(key, binding);
    Ok(())
}

/// Look up the Rust state bound to `obj` and downcast it to `T`.
///
/// The state is returned as an [`Arc`] clone shared with the registry. Fails
/// with [`JavaError::InvalidArgument`] when `obj` has no bound state or its
/// state is not of type `T` (a binding whose shell was garbage-collected
/// counts as "no bound state").
pub fn get<T: Any + Send + Sync + 'static>(
    env: &mut Env<'_>,
    obj: &JObject,
) -> JavaResult<Arc<T>> {
    let key = identity_hash(env, obj)?;
    let mut registry = REGISTRY.lock();
    drain_dead(&mut registry, env);
    let binding = registry.get(&key).ok_or_else(|| state_error(false))?;
    // Collision safety: never hand out another object's state. Resolve the
    // weak ref — null means the shell was collected, i.e. no live state.
    let Some(local) = binding.obj.upgrade_local(env).ok().flatten() else {
        return Err(state_error(false));
    };
    let same = env.is_same_object(&local, &*obj.global)?;
    if !same {
        return Err(JavaError::InvalidArgument(
            "userdata: identity-hash collision — this object shares the identity hash \
             of a bound object but is not that object",
        ));
    }
    // Clone the Arc out, then release the lock before downcasting.
    let state = Arc::clone(&binding.state);
    drop(registry);
    state
        .downcast::<T>()
        .map_err(|_| state_error(true))
}

/// Run `f` with the Rust state bound to `obj` — the closure sugar over
/// [`get`] that saves one line per native method.
///
/// [`get`]`(env, obj)?` plus a method call becomes one `with` call: the
/// state is borrowed as `&T` for the duration of `f` (the [`Arc`] clone from
/// [`get`] is dropped when `f` returns) and the closure's result is returned
/// wrapped in `Ok`. A missing or wrongly-typed state fails with the **same**
/// [`JavaError::InvalidArgument`] as [`get`] — `with` is a thin convenience
/// over it, with identical semantics.
///
/// ```
/// # use parking_lot::Mutex;
/// # use rjava::prelude::*;
/// struct Counter(Mutex<i64>);
///
/// // before:  Ok(*rjava::userdata::get::<Counter>(env, &this)?.0.lock())
/// // after:
/// fn counter_value(env: &mut jni::Env, (this,): (JObject,)) -> JavaResult<i64> {
///     rjava::userdata::with::<Counter, _>(env, &this, |c| *c.0.lock())
/// }
///
/// // multi-statement closures work too:
/// fn counter_increment(env: &mut jni::Env, (this, by): (JObject, i64)) -> JavaResult<i64> {
///     rjava::userdata::with::<Counter, _>(env, &this, |c| {
///         let mut count = c.0.lock();
///         *count += by;
///         *count
///     })
/// }
/// # fn main() {}
/// ```
///
/// The closure receives `&T`, not `&mut T`: the state is shared behind an
/// [`Arc`], and you choose the synchronization inside it (`Mutex`,
/// `AtomicI64`, …) exactly as with [`get`] — the crate's userdata pattern.
pub fn with<T, R>(
    env: &mut Env<'_>,
    obj: &JObject,
    f: impl FnOnce(&T) -> R,
) -> JavaResult<R>
where
    T: Any + Send + Sync + 'static,
{
    let state = get::<T>(env, obj)?;
    Ok(f(&state))
}

/// Remove the binding for `obj`, dropping the Rust state (the weak
/// reference is released too — the shell stays GC-able regardless).
///
/// Returns `true` if a binding existed and was removed, `false` otherwise
/// (including for a never-bound object or one whose shell was already
/// collected — such dead bindings are removed by the drain instead).
pub fn unbind(env: &mut Env<'_>, obj: &JObject) -> bool {
    let Ok(key) = identity_hash(env, obj) else {
        return false;
    };
    let mut registry = REGISTRY.lock();
    drain_dead(&mut registry, env);
    let same = registry
        .get(&key)
        .map(|b| match b.obj.upgrade_local(env) {
            Ok(Some(local)) => env.is_same_object(&local, &*obj.global).unwrap_or(false),
            // Collected (or unresolvable): nothing to unbind explicitly.
            Ok(None) | Err(_) => false,
        })
        .unwrap_or(false);
    same && registry.remove(&key).is_some()
}

/// Create a Java object of `class` and bind `state` to it — the factory
/// helper for `public static native X create();` implementations.
///
/// Equivalent to constructing the shell with `new` and calling [`bind`]
/// afterwards; Java code may equally construct the shell itself and let the
/// host bind state to it later.
pub fn create_shell(
    env: &mut Env<'_>,
    class: &str,
    state: impl Any + Send + Sync + 'static,
) -> JavaResult<JObject> {
    let java = Java::from_env(env)?;
    let shell = java.new_object(class, ())?;
    bind(env, &shell, state)?;
    Ok(shell)
}

/// Number of currently-live bindings — a diagnostic aid for leak hunting.
///
/// A pure count under the registry lock: it does **not** drain, so shortly
/// after shells become unreachable the count may still include their (dead)
/// bindings until the cleaner thread's next tick or the lazy drain on
/// [`bind`]/[`get`]/[`unbind`] removes them. The integration tests use this
/// to verify that garbage-collected shells are released automatically.
pub fn live_bindings() -> usize {
    REGISTRY.lock().len()
}
