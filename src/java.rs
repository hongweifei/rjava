//! The [`Java`] facade — the mlua `Lua` analog: a single handle through which
//! you look up classes, create objects and arrays, and attach threads.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use jni::objects::JObject as JniObject;
use jni::strings::JNIString;
use jni::{Env, InitArgsBuilder, JNIVersion, JavaVM};

use crate::array::JavaArrayElement;
use crate::bind::JavaBound;
use crate::call;
use crate::classloader::JClassLoader;
use crate::convert::{FromJava, ToJava};
use crate::error::{JavaError, JavaResult};
use crate::handles::{JArray, JClass as JClassHandle, JObject as JObjectHandle, JavaThread};

/// Serializes JVM creation. jni-rs's `JavaVM::new` checks its singleton and
/// then calls `JNI_CreateJavaVM`; two threads can both pass the check and
/// race the call, and the JVM spec allows only one JVM per process — the
/// loser fails. Holding this lock for the whole `build()` makes concurrent
/// `Java::builder().build()` calls all end up with the one JVM.
static JVM_CREATE_LOCK: Mutex<()> = Mutex::new(());

/// A handle to the (single) Java virtual machine of this process.
///
/// `Java` is cheap to clone, `Send` and `Sync`; every method automatically
/// attaches the calling thread to the JVM for the duration of the call, so
/// user code never manages attachment. See the [crate docs](crate) for the
/// thread model.
#[derive(Clone, Debug)]
pub struct Java {
    pub(crate) vm: JavaVM,
}

/// Configuration for creating the JVM (the mlua `Lua::new` analog).
///
/// Obtained from [`Java::builder`]; see [`JvmConfig::build`].
#[derive(Debug, Default, Clone)]
pub struct JvmConfig {
    class_path: Option<String>,
    options: Vec<String>,
}

impl JvmConfig {
    /// Start with the default configuration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the Java class path (passed as `-Djava.class.path=...`).
    ///
    /// On Windows, multiple entries are separated by `;`; on other platforms
    /// by `:`. This is optional — `rjava` itself only needs JDK classes.
    pub fn class_path(mut self, class_path: impl Into<String>) -> Self {
        self.class_path = Some(class_path.into());
        self
    }

    /// Add a raw JVM option, e.g. `-Xmx256m` or `-Dmy.prop=value`.
    /// Repeatable.
    pub fn option(mut self, option: impl Into<String>) -> Self {
        self.options.push(option.into());
        self
    }

    /// Create the JVM (or reuse an already-created one) and return the
    /// [`Java`] facade.
    ///
    /// The JVM is located via the `jni` crate's [java-locator]: the
    /// `JAVA_HOME` environment variable is consulted first, then `java` on
    /// `PATH` (and on Windows, the registry). You need a JDK (not just a JRE)
    /// for the `jvm` library (`jvm.dll` on Windows).
    ///
    /// Safe to call concurrently from multiple threads: creation is
    /// serialized, so every caller ends up with the one process-wide JVM.
    ///
    /// "Reuse" applies to a JVM already created **through this crate** (an
    /// earlier `build()` or [`Java::from_env`]); when Rust is loaded into a
    /// Java process that created the JVM itself, `build()` cannot attach to
    /// it — wrap the `&mut jni::Env` your native method receives with
    /// [`Java::from_env`] instead.
    ///
    /// [java-locator]: https://docs.rs/java-locator
    pub fn build(self) -> JavaResult<Java> {
        // See JVM_CREATE_LOCK: jni-rs's `JavaVM::new` is not safe to call
        // concurrently (its singleton check + `JNI_CreateJavaVM` race), and
        // once the VM exists later calls just reuse it.
        let _guard = JVM_CREATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut args = InitArgsBuilder::new().version(JNIVersion::V1_8);
        for opt in &self.options {
            args = args.option(opt.clone());
        }
        if let Some(cp) = &self.class_path {
            args = args.option(format!("-Djava.class.path={cp}"));
        }
        let args = args.build().map_err(|e| JavaError::JvmStart(e.to_string()))?;
        let vm = JavaVM::new(args).map_err(|e| JavaError::JvmStart(e.to_string()))?;
        Ok(Java { vm })
    }
}

impl Java {
    /// Create a [`JvmConfig`] to configure and start the JVM.
    ///
    /// ```no_run
    /// # use rjava::prelude::*;
    /// # fn example() -> JavaResult<()> {
    /// let java = Java::builder()
    ///     .class_path("target/classes")
    ///     .option("-Xmx256m")
    ///     .build()?;
    /// # Ok(()) }
    /// ```
    pub fn builder() -> JvmConfig {
        JvmConfig::new()
    }

    /// Wrap the JVM backing an attached `Env`, so a native-method
    /// implementation can call back into Java.
    ///
    /// Typical use: inside a function registered with [`crate::native!`] /
    /// [`crate::native_inst!`], obtain a [`Java`] facade from the
    /// `&mut jni::Env` the function receives and use it like any other `Java`
    /// value:
    ///
    /// ```no_run
    /// # use rjava::prelude::*;
    /// # fn example(env: &mut jni::Env) -> JavaResult<()> {
    /// let java = Java::from_env(env)?;
    /// let abs: i32 = java.class("java.lang.Math")?.call_static("abs", (-42_i32,))?;
    /// # Ok(()) }
    /// ```
    ///
    /// Works both when the JVM was created by this crate and when Rust was
    /// loaded into a Java process that created the JVM (the VM is read from
    /// the `Env`'s pointer in that case).
    pub fn from_env(env: &mut Env<'_>) -> JavaResult<Java> {
        let vm = env.get_java_vm().map_err(JavaError::from)?;
        Ok(Java { vm })
    }

    /// Look up a class by name. Both dotted (`java.lang.StringBuilder`) and
    /// slash-separated (`java/lang/StringBuilder`) forms are accepted.
    pub fn class(&self, name: &str) -> JavaResult<JClassHandle> {
        let name = normalize_class_name(name)?;
        self.with_env(|env| {
            let cls = call::find_class(env, JNIString::from(name.as_str()))?;
            Ok(JClassHandle::from_global(env.new_global_ref(cls)?))
        })
    }

    /// Construct an object of `class`, passing `args` to the constructor.
    pub fn new_object<A: ToJava>(&self, class: &str, args: A) -> JavaResult<JObjectHandle> {
        let name = normalize_class_name(class)?;
        self.with_env(|env| {
            let cls = call::find_class(env, JNIString::from(name.as_str()))?;
            let cls_global = env.new_global_ref(cls)?;
            call::new_object(env, &cls_global, &args)
        })
    }

    /// Create a new primitive array of `len` elements.
    ///
    /// `T` selects the array type: `JArray<i32>` → `int[]`, `JArray<i8>` →
    /// `byte[]`, and so on. For object arrays use [`Java::new_object_array`].
    pub fn new_array<T: JavaArrayElement>(&self, len: usize) -> JavaResult<JArray<T>> {
        self.with_env(|env| {
            let arr = crate::array::new_primitive_array(env, len, T::__kind())?;
            Ok(JArray::from_global_obj(env.new_global_ref(&arr)?))
        })
    }

    /// Create a new primitive array filled from `values` (a `Vec`-backed
    /// [`Java::new_array`]).
    pub fn new_array_from<T: JavaArrayElement>(&self, values: Vec<T>) -> JavaResult<JArray<T>> {
        JArray::<T>::from_vec(values)
    }

    /// Create a new object array `class[]` with `len` (null) elements.
    pub fn new_object_array(
        &self,
        class: &str,
        len: usize,
    ) -> JavaResult<JArray<JObjectHandle>> {
        let name = normalize_class_name(class)?;
        self.with_env(|env| {
            let cls = call::find_class(env, JNIString::from(name.as_str()))?;
            let arr: JniObject = env.new_object_array(len as i32, &cls, JniObject::null())?.into();
            Ok(JArray::from_global_obj(env.new_global_ref(arr)?))
        })
    }

    /// Create a new `Object[]` array filled from `values`.
    pub fn new_object_array_from(
        &self,
        values: Vec<JObjectHandle>,
    ) -> JavaResult<JArray<JObjectHandle>> {
        JArray::<JObjectHandle>::from_vec(values)
    }

    /// Run a Java `public static void main(String[])` entry point.
    ///
    /// Thin sugar over the normal call machinery: looks up `class` (dotted or
    /// slash form), builds a `String[]` from `args` (via the `Vec<String>`
    /// conversion) and calls `main` with it. A `main` that throws surfaces as
    /// [`JavaError::JavaException`] (the exception is captured and cleared as
    /// usual). Note: `System.exit(...)` inside `main` terminates the JVM —
    /// that is Java semantics, not an rjava behavior. `main` returns `()`.
    pub fn run_main(&self, class: &str, args: &[impl AsRef<str>]) -> JavaResult<()> {
        let class = self.class(class)?;
        let args: Vec<String> = args.iter().map(|s| s.as_ref().to_string()).collect();
        class.call_static("main", (args,))
    }

    /// Create a `java.net.URLClassLoader` over the given class-path entries
    /// and return a [`JClassLoader`] handle.
    ///
    /// Each entry is a jar file or a class directory, resolved to a URL
    /// through the `File` / `toURI` / `toURL` chain (which handles Windows
    /// paths correctly). The loader's **parent is the system class loader**,
    /// so it can also see JDK and system-class-path classes. An empty
    /// `class_path` or a path that does not exist is rejected with
    /// [`JavaError::InvalidArgument`].
    ///
    /// This is how the host loads the **API jar** in the plugin workflow
    /// (see [`JClassLoader`] and [`Java::class_loader_with_parent`]):
    ///
    /// ```no_run
    /// # use rjava::prelude::*;
    /// # fn example(java: &Java) -> JavaResult<()> {
    /// // The API jar is the compile-time contract for plugin developers:
    /// // interfaces + a Bridge class declaring `native` methods.
    /// let api = java.load_jar("target/plugin-api.jar")?;
    /// let bridge = api.load_class("com.example.api.Bridge")?;
    /// bridge.register_natives(&[])?; // ...register the natives here...
    /// // Plugin jars are then loaded with this loader as their parent:
    /// let plugin = java.class_loader_with_parent(&["target/plugin.jar"], &api)?;
    /// let hello = plugin.load_class("com.example.plugin.Hello")?;
    /// # Ok(()) }
    /// ```
    pub fn class_loader(&self, class_path: &[impl AsRef<str>]) -> JavaResult<JClassLoader> {
        let urls = Self::urls_for_paths(self, class_path)?;
        let loader = self.new_object("java.net.URLClassLoader", (urls,))?;
        Ok(JClassLoader::from_handle(loader))
    }

    /// Create a `java.net.URLClassLoader` over `class_path` whose **parent**
    /// is `parent` — the building block of the plugin workflow.
    ///
    /// Plugin jars are compiled against the API jar but do not contain it;
    /// when the plugin loader's parent is the API loader, plugin classes
    /// resolve the API interfaces and the `Bridge` through the *same* classes
    /// the host registered natives on. Without this, sibling loaders cannot
    /// see each other's classes (the JVM gives each loader its own
    /// namespace), and the plugin would fail with `ClassNotFoundException`.
    ///
    /// `class_path` follows the same rules as [`Java::class_loader`] (an
    /// empty or non-existent entry is rejected with
    /// [`JavaError::InvalidArgument`]).
    ///
    /// ```no_run
    /// # use rjava::prelude::*;
    /// # fn example(java: &Java, api: &JClassLoader) -> JavaResult<()> {
    /// let plugin = java.class_loader_with_parent(&["target/plugin.jar"], api)?;
    /// let hello = plugin.load_class("com.example.plugin.Hello")?;
    /// let name: String = hello.new_instance(())?.call("name", ())?;
    /// # Ok(()) }
    /// ```
    pub fn class_loader_with_parent(
        &self,
        class_path: &[impl AsRef<str>],
        parent: &JClassLoader,
    ) -> JavaResult<JClassLoader> {
        let urls = Self::urls_for_paths(self, class_path)?;
        let loader = self.new_object("java.net.URLClassLoader", (urls, parent))?;
        Ok(JClassLoader::from_handle(loader))
    }

    /// Sugar for [`Java::class_loader`] with a single jar path:
    /// `java.load_jar("target/plugin-api.jar")`.
    pub fn load_jar(&self, jar_path: impl AsRef<str>) -> JavaResult<JClassLoader> {
        self.class_loader(&[jar_path])
    }

    /// Convert `class_path` entries into a `URL[]` (each entry through
    /// `File` / `toURI` / `toURL`, which handles Windows paths), checking
    /// that the entries exist first.
    fn urls_for_paths<P: AsRef<str>>(
        java: &Java,
        class_path: &[P],
    ) -> JavaResult<JArray<JObjectHandle>> {
        if class_path.is_empty() {
            return Err(JavaError::InvalidArgument(
                "class path must not be empty — pass at least one jar or class directory",
            ));
        }
        for entry in class_path {
            if !Path::new(entry.as_ref()).exists() {
                return Err(JavaError::InvalidArgument(
                    "class path entry does not exist; pass the path of an existing \
                     jar file or class directory",
                ));
            }
        }
        let urls = java.new_object_array("java.net.URL", class_path.len())?;
        for (i, entry) in class_path.iter().enumerate() {
            let file = java.new_object("java.io.File", (entry.as_ref(),))?;
            let uri: JObjectHandle = file.call("toURI", ())?;
            let url: JObjectHandle = uri.call("toURL", ())?;
            urls.set(i, url)?;
        }
        Ok(urls)
    }

    /// Attach the current thread to the JVM, returning a RAII [`JavaThread`]
    /// guard that detaches the thread when dropped.
    ///
    /// This is only needed when you want explicit control over attachment; in
    /// normal use every `Java`/handle method attaches automatically.
    pub fn attach_thread(&self) -> JavaResult<JavaThread> {
        self.vm
            .attach_current_thread::<_, (), JavaError>(|_| Ok(()))?;
        Ok(JavaThread { vm: self.vm.clone() })
    }

    /// Construct an object of a bound class `T` via its constructor — the
    /// `bind!` analog of [`Java::new_object`].
    ///
    /// `args` is the constructor argument list (a tuple; `()` for no
    /// arguments), exactly as with the dynamic path. `T` is any wrapper
    /// declared with the [`bind!`](macro@crate::bind) macro; the class name comes
    /// from the declaration and is resolved (and cached) on first use, so a
    /// wrong class name surfaces as a clear error from the first call.
    ///
    /// ```no_run
    /// # use rjava::prelude::*;
    /// # use rjava::bind;
    /// # bind! {
    /// #     "java.lang.StringBuilder" => StringBuilder {
    /// #         fn length() -> i32;
    /// #     }
    /// # }
    /// # fn example() -> JavaResult<()> {
    /// # let java = Java::builder().build()?;
    /// let sb = java.new::<StringBuilder>(("Hello",))?;
    /// # let len: i32 = sb.length()?;
    /// # assert_eq!(len, 5);
    /// # Ok(()) }
    /// ```
    // `new` here is the typed-construction sugar for a *bound* class, not a
    // constructor of `Java` itself (which is built via `Java::builder`);
    // `new_ret_no_self` is intentionally allowed.
    #[allow(clippy::new_ret_no_self)]
    pub fn new<T: JavaBound>(&self, args: impl ToJava) -> JavaResult<T> {
        self.with_env(|env| {
            let class = T::class(env)?;
            let obj = call::new_object(env, class, &args)?;
            Ok(T::wrap(self.clone(), obj))
        })
    }

    /// Wrap an existing object handle into a bound wrapper `T` — the `bind!`
    /// analog of annotating a dynamic call's result with a concrete type.
    ///
    /// No JNI call happens here: the wrapper is created as-is, and the class
    /// is resolved (and validated) lazily on the first actual call, exactly
    /// like [`Java::new`].
    ///
    /// ```no_run
    /// # use rjava::prelude::*;
    /// # use rjava::bind;
    /// # bind! {
    /// #     "java.lang.StringBuilder" => StringBuilder {
    /// #         fn length() -> i32;
    /// #     }
    /// # }
    /// # fn example() -> JavaResult<()> {
    /// # let java = Java::builder().build()?;
    /// let raw: JObject = java.new_object("java.lang.StringBuilder", ("Hi",))?;
    /// let sb: StringBuilder = java.wrap(raw);
    /// # let len: i32 = sb.length()?;
    /// # Ok(()) }
    /// ```
    pub fn wrap<T: JavaBound>(&self, obj: JObjectHandle) -> T {
        T::wrap(self.clone(), obj)
    }

    /// Run `f` with the current thread attached to the JVM.
    pub(crate) fn with_env<T>(
        &self,
        f: impl for<'env> FnOnce(&mut jni::Env<'env>) -> JavaResult<T>,
    ) -> JavaResult<T> {
        self.vm.attach_current_thread(f)
    }

    /// Call an instance method **asynchronously** — the future analog of
    /// [`JObject::call`](crate::JObject::call) — returning a Rust future
    /// that resolves with the call's result.
    ///
    /// The future is **lazy**: nothing is spawned and no JVM call is made
    /// until the first poll. On that first poll one worker is dispatched —
    /// onto the tokio blocking pool when a tokio runtime is current
    /// (feature `tokio`), else onto a detached std thread — which attaches
    /// through this [`Java`] facade, converts `args` to JNI arguments on
    /// the worker thread, performs the call (the exact same machinery as
    /// [`JObject::call`](crate::JObject::call), including the reflection
    /// fallback and exception capture), and wakes the future. The receiver
    /// object's global reference is cloned, so the object stays alive for
    /// the duration of the call.
    ///
    /// # Semantics
    ///
    /// * **Fused.** Once `Ready`, the future stays completed; polling it
    ///   again after completion is a contract violation and panics.
    /// * **Cancellation.** Dropping the future does **not** cancel the
    ///   Java call: the worker keeps running and its result is discarded
    ///   (there is no safe way to interrupt an in-flight JNI call). Cancel
    ///   from the Java side if needed.
    /// * **Errors.** The call's errors surface through the future exactly
    ///   as the sync call would return them: a thrown Java exception
    ///   becomes [`JavaError::JavaException`] (captured and cleared), a
    ///   type/arity mismatch an [`JavaError::InvalidArgument`], and so on.
    ///   The one difference: argument-conversion failures also arrive via
    ///   the future instead of synchronously, because the arguments are
    ///   converted on the worker thread.
    /// * **Tokio interplay.** With the `tokio` feature enabled, awaiting
    ///   the future inside a tokio runtime runs the JVM call on the
    ///   runtime's blocking pool ([`tokio::task::spawn_blocking`]); awaited
    ///   outside any runtime (or without the feature) it runs on a detached
    ///   std thread — so enabling `tokio` never breaks non-tokio users.
    ///
    /// ```no_run
    /// # use rjava::prelude::*;
    /// # async fn example(java: &Java, sb: &JObject) -> JavaResult<()> {
    /// let len: i32 = java.call_async(sb, "length", ()).await?;
    /// let s: String = java.call_async(sb, "toString", ()).await?;
    /// # let _ = (len, s);
    /// # Ok(()) }
    /// ```
    pub fn call_async<A: ToJava + Send + 'static, R: FromJava + Send + 'static>(
        &self,
        obj: &JObjectHandle,
        name: impl Into<String>,
        args: A,
    ) -> impl Future<Output = JavaResult<R>> + Send {
        AsyncCallFuture {
            state: Arc::new(Mutex::new(AsyncCallState::Unstarted {
                java: self.clone(),
                target: AsyncCallTarget::Instance(obj.clone()),
                name: name.into(),
                args,
            })),
        }
    }

    /// Call a static method **asynchronously** — the future analog of
    /// [`JClass::call_static`](crate::JClass::call_static). Same semantics
    /// as [`Java::call_async`] (lazy start, worker dispatch, fused,
    /// cancellation, error surfacing); see its docs.
    ///
    /// ```no_run
    /// # use rjava::prelude::*;
    /// # async fn example(java: &Java) -> JavaResult<()> {
    /// let max: i32 = java
    ///     .call_static_async(&java.class("java.lang.Math")?, "max", (3_i32, 7_i32))
    ///     .await?;
    /// # assert_eq!(max, 7);
    /// # Ok(()) }
    /// ```
    pub fn call_static_async<A: ToJava + Send + 'static, R: FromJava + Send + 'static>(
        &self,
        class: &JClassHandle,
        name: impl Into<String>,
        args: A,
    ) -> impl Future<Output = JavaResult<R>> + Send {
        AsyncCallFuture {
            state: Arc::new(Mutex::new(AsyncCallState::Unstarted {
                java: self.clone(),
                target: AsyncCallTarget::Static(class.clone()),
                name: name.into(),
                args,
            })),
        }
    }
}

// ---------------------------------------------------------------------------
// Async method calls (`call_async` / `call_static_async`) — worker machinery
// ---------------------------------------------------------------------------

/// The target of an async call: an instance receiver or a class (for a
/// static method).
enum AsyncCallTarget {
    /// `obj.call_async(...)` — the receiver object.
    Instance(JObjectHandle),
    /// `class.call_static_async(...)` — the class.
    Static(JClassHandle),
}

/// The shared state of a [`Java::call_async`] / [`Java::call_static_async`]
/// bridge, guarded by a `Mutex`.
///
/// The worker thread and the polling side communicate exclusively through
/// this state, mirroring [`crate::future`]'s bridge: the poller spawns the
/// worker out of [`AsyncCallState::Unstarted`] and observes the outcome in
/// [`AsyncCallState::Done`]; the worker attaches through the captured
/// [`Java`], performs the call, stores the outcome, and wakes the last
/// registered waker.
enum AsyncCallState<A, R> {
    /// Not yet polled: everything the worker needs to perform the call.
    Unstarted {
        /// The facade the worker thread attaches through.
        java: Java,
        /// The call target (instance receiver or class).
        target: AsyncCallTarget,
        /// The method name.
        name: String,
        /// The argument list, converted to JNI args on the worker thread.
        args: A,
    },
    /// The worker is running (or about to be spawned); `waker` is the latest
    /// waker registered by a poll.
    Waiting {
        /// The waker to wake when the outcome is ready.
        waker: Option<Waker>,
    },
    /// The outcome is ready. `None` means it has already been returned by a
    /// poll (the future is then spent — polling again is a bug).
    Done(Option<JavaResult<R>>),
}

/// The future returned by [`Java::call_async`] /
/// [`Java::call_static_async`]. Private: the concrete type is exposed as
/// `impl Future`.
struct AsyncCallFuture<A, R> {
    /// Shared state; cloned into the worker at spawn.
    state: Arc<Mutex<AsyncCallState<A, R>>>,
}

impl<A: ToJava + Send + 'static, R: FromJava + Send + 'static> Future for AsyncCallFuture<A, R> {
    type Output = JavaResult<R>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Clone the Arc first so the spawn closure can own a handle to the
        // same state even while the guard below borrows it.
        let state_arc = Arc::clone(&self.state);
        let mut state = state_arc.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            // First poll: take the startup data out, dispatch the worker,
            // and re-enter the loop to register the waker (now `Waiting`).
            if matches!(*state, AsyncCallState::Unstarted { .. }) {
                let AsyncCallState::Unstarted { java, target, name, args } =
                    std::mem::replace(&mut *state, AsyncCallState::Waiting { waker: None })
                else {
                    unreachable!("guarded by the matches! above")
                };
                let worker_state = Arc::clone(&state_arc);
                let spawned = spawn_bridge_worker(move || {
                    async_call_worker::<A, R>(java, target, name, args, worker_state)
                });
                if spawned.is_err() {
                    // The worker can never run; the future can never complete
                    // normally, so surface the failure as the outcome.
                    let err = JavaError::InvalidArgument(
                        "failed to spawn the async-call bridge thread",
                    );
                    *state = AsyncCallState::Done(Some(Err(JavaError::InvalidArgument(
                        "failed to spawn the async-call bridge thread",
                    ))));
                    return Poll::Ready(Err(err));
                }
                continue;
            }
            match &mut *state {
                AsyncCallState::Waiting { waker } => {
                    let should_update = match waker {
                        Some(current) => !current.will_wake(cx.waker()),
                        None => true,
                    };
                    if should_update {
                        *waker = Some(cx.waker().clone());
                    }
                    return Poll::Pending;
                }
                AsyncCallState::Done(outcome) => match outcome.take() {
                    Some(outcome) => return Poll::Ready(outcome),
                    None => panic!("AsyncCallFuture polled after completion"),
                },
                AsyncCallState::Unstarted { .. } => unreachable!("handled by the if-let above"),
            }
        }
    }
}

/// Dispatch the bridge worker: with the `tokio` feature and a current tokio
/// runtime, run `f` on the runtime's blocking pool
/// ([`tokio::task::spawn_blocking`]); otherwise run it on a detached std
/// thread. The tokio path is preferred so JVM calls never stall a tokio
/// worker thread; the std-thread fallback keeps the feature optional and
/// non-breaking for foreign executors. Returns `Err` when the std-thread
/// spawn fails (a `spawn_blocking` submission never fails).
fn spawn_bridge_worker(f: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    #[cfg(feature = "tokio")]
    {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            std::mem::drop(handle.spawn_blocking(f));
            return Ok(());
        }
    }
    std::thread::Builder::new()
        .name("rjava-call-async".into())
        .spawn(f)
        .map(|_| ())
}

/// The worker body: attach through `java`, perform the call (the same
/// machinery as the sync path), store the outcome, and wake the registered
/// waker (outside the lock — a waker may re-enter poll synchronously).
fn async_call_worker<A: ToJava, R: FromJava>(
    java: Java,
    target: AsyncCallTarget,
    name: String,
    args: A,
    state: Arc<Mutex<AsyncCallState<A, R>>>,
) {
    let outcome = java.with_env(|env| match &target {
        AsyncCallTarget::Instance(obj) => call::call_method(env, &obj.global, &name, &args),
        AsyncCallTarget::Static(cls) => call::call_static_method(env, &cls.global, &name, &args),
    });
    let waker = {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        let waker = match &mut *state {
            AsyncCallState::Waiting { waker } => waker.take(),
            _ => None,
        };
        *state = AsyncCallState::Done(Some(outcome));
        waker
    };
    if let Some(waker) = waker {
        waker.wake();
    }
}

/// Convert a user-facing class name to JNI form (`java.lang.String` →
/// `java/lang/String`). Slash-separated names and array descriptors are
/// accepted as-is.
fn normalize_class_name(name: &str) -> JavaResult<String> {
    if name.is_empty() {
        return Err(JavaError::InvalidArgument("class name must not be empty"));
    }
    if name.contains('/') {
        Ok(name.to_string())
    } else {
        Ok(name.replace('.', "/"))
    }
}
