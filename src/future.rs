//! Bridge a Java `java.util.concurrent.CompletableFuture` to a Rust
//! [`Future`] and back — std-only: no executor dependency, no
//! Java helper classes, no `unsafe`.
//!
//! The module has two directions:
//!
//! * **Java → Rust**: [`crate::future::java_future()`] wraps a
//!   `CompletableFuture` handle and returns a Rust future.
//! * **Rust → Java**: [`crate::future::make_async_native`] (via the
//!   [`crate::async_native!`] macro) registers a Rust **async** function as a
//!   Java static `native` method whose Java return type is
//!   `java.util.concurrent.CompletableFuture` — the Rust async work runs on
//!   a detached std thread and completes the future later, so Java code can
//!   chain and `await` it.
//!
//! # Java → Rust: `java_future`
//!
//! The **first poll** spawns one detached std thread that attaches to
//! the JVM, blocks on `CompletableFuture.get()` (returning the result object,
//! or surfacing the failure as [`JavaError::JavaException`]), converts the
//! value to `T` via [`FromJava`], stores the outcome, and wakes the future's
//! waker. The thread is attached **permanently** (the JVM detaches it
//! automatically when it exits) and ends as soon as `get()` returns.
//!
//! ```no_run
//! use rjava::future::java_future;
//! use rjava::prelude::*;
//!
//! # async fn example(java: Java) -> JavaResult<()> {
//! // 1) a CompletableFuture, typically completed by Java code...
//! let cf: JObject = java.new_object("java.util.concurrent.CompletableFuture", ())?;
//!
//! //    ...or completed from Rust — a plain call from *any* thread (rjava
//! //    auto-attaches that thread to the JVM for the duration of the call):
//! //    cf.call_void("complete", ("done",))?;
//!
//! // 2) bridge it to a Rust future: the first poll spawns one std thread
//! //    that blocks on CompletableFuture.get()
//! let result: String = java_future::<String>(java, cf).await?;
//! # let _ = result;
//! # Ok(())
//! # }
//! ```
//!
//! ## `java_future` semantics
//!
//! * **Lazy start.** Nothing is spawned and no JVM call is made until the
//!   first poll; constructing the future is cheap. The bridge is fully
//!   driven by the executor: it never spawns work off its own.
//! * **Send.** The returned future is `Send` (the state is a `Mutex`; `Java`
//!   and [`JObject`] are `Send + Sync`), so it can be awaited from any
//!   executor thread.
//! * **Fused.** Once `Ready`, the future stays completed; polling it again
//!   after completion is a contract violation and panics.
//! * **Cancellation.** Dropping the future does **not** cancel the Java
//!   future: the detached thread keeps waiting on `get()` and the eventual
//!   result is discarded. Cancel from the Java side if needed
//!   (`cf.call_void("cancel", (true,))`).
//! * **Timeouts.** Not provided here. Race the Java side instead:
//!   `cf.call_void("completeOnTimeout", (value, timeout_ms, unit))` completes
//!   the future with a fallback after the timeout, or call the
//!   `get(timeout, unit)` variant yourself via `cf.call("get", (timeout, unit))`
//!   (a `TimeoutException` surfaces as a `JavaError::JavaException`).
//! * **Exception unwrapping.** `CompletableFuture.get()` wraps any failure
//!   thrown by the task in `java.util.concurrent.ExecutionException`. The
//!   bridge unwraps **one level**: when the thrown exception has a non-null
//!   cause (`Throwable.getCause()`), the *cause's* class and message are
//!   reported — so `completeExceptionally(new IllegalArgumentException("nope"))`
//!   surfaces as `IllegalArgumentException` / `"nope"`, not as
//!   `ExecutionException`. Exceptions without a cause (`CancellationException`,
//!   `InterruptedException`, an `ExecutionException` with a `null` cause) are
//!   reported as themselves. The unwrap is deliberately not recursive.
//! * **Primitives.** `get()` returns `Object`, so for a primitive `T`
//!   (`i32`, `i64`, `bool`, …) the bridge unboxes the wrapper object
//!   (`Integer.intValue()` and friends) before converting; a `null` result
//!   with a primitive annotation is an error (annotate `Option<T>` to accept
//!   `null`).
//!
//! # Rust → Java: `async_native!`
//!
//! [`crate::async_native!`] registers a Rust **async** function as a Java
//! static `native` method returning `java.util.concurrent.CompletableFuture`:
//!
//! ```no_run
//! use rjava::async_native;
//! use rjava::future::java_future;
//! use rjava::prelude::*;
//!
//! // Java: `public static native CompletableFuture<Integer> compute(int a, int b);`
//! async fn compute(java: Java, (a, b): (i32, i32)) -> JavaResult<i32> {
//!     let _ = java; // call back into Java through `java` before any await
//!     Ok(a + b)
//! }
//!
//! # async fn example(java: Java) -> JavaResult<()> {
//! let clazz = java.class("com.example.NativeLib")?;
//! clazz.register_natives(&[async_native!("compute", compute)?])?;
//! // Java (and Rust) callers get a CompletableFuture<Integer>; awaiting it
//! // through java_future is the full circle back to Rust:
//! let cf: JObject = clazz.call_static("compute", (1_i32, 2_i32))?;
//! let sum: i32 = java_future::<i32>(java, cf).await?;
//! # assert_eq!(sum, 3);
//! # Ok(())
//! # }
//! ```
//!
//! The Java declaration must be
//! `public static native CompletableFuture<R> name(…)`, where the parameter
//! descriptor is derived from the Rust argument tuple and the return
//! descriptor is **always** exactly `Ljava/util/concurrent/CompletableFuture;`.
//!
//! ## The `Java` parameter
//!
//! The async function receives the [`Java`] facade **by value** — clonable,
//! `Send + Sync`, exactly like the `java_future` bridge — **not** an
//! `&mut jni::Env`: `Env` is not `Send`, so it cannot be held across an
//! `await`, and there is no JVM `Env` on the worker thread anyway (the
//! thread attaches on demand through `Java`). Every `Java`/handle call inside
//! the async function auto-attaches the calling thread for the duration of
//! the call, so call-back-into-Java code is written the same way as anywhere
//! else in `rjava`. If you need a raw `&mut Env` (e.g. for a low-level JNI
//! call) at the very start of the function, wrap one with
//! [`Java::from_env`](crate::Java::from_env) — it must be used *before* the
//! first `await`.
//!
//! ## `async_native!` semantics
//!
//! * **One worker per call, no executor dependency.** The native method
//!   returns the `CompletableFuture` immediately; by default a detached std
//!   thread runs the Rust future to completion under
//!   [`futures_lite::future::block_on`] — a real thread-parking waker (the
//!   `unsafe` lives inside futures-lite, never in `rjava`) — and then
//!   completes the Java future. With the `tokio` feature, a call made from
//!   inside a tokio runtime runs the future on that runtime instead (see
//!   the executor bullet below). No executor dependency by default,
//!   consistent with [`java_future`](crate::future::java_future)'s
//!   thread-per-call design.
//! * **Result conversion.** `Ok(R)` is converted with [`ToJava`] and boxed
//!   into an object where needed (primitives become their wrapper classes via
//!   `Wrapper.valueOf`, so `i32` completes the future with an `Integer`),
//!   then `complete(Object)` is called. `()` (the `R = ()` void annotation)
//!   completes the future with `null`.
//! * **Errors.** `Err(JavaError::JavaException { class, message })` is
//!   materialized as an instance of `class` (constructed with the message)
//!   and delivered via `completeExceptionally`; any other `Err` — and any
//!   **panic** inside the async function — completes the future
//!   exceptionally with a `java.lang.RuntimeException` carrying the error's
//!   `Debug` rendering / the panic payload, mirroring the synchronous
//!   native-method exception rules ([`crate::native::throw_error`]). A
//!   `java_future` awaiting such a future reports the exception through its
//!   usual `ExecutionException`-cause unwrap.
//! * **Registration collisions.** Two *different* `async_native!`
//!   registrations with the **same Rust argument-tuple type** share one
//!   trampoline (the C return is `jobject` for every async native — Java
//!   always sees a `CompletableFuture` — so the trampoline is keyed by `A`
//!   only), and [`JClass::register_natives`](crate::JClass::register_natives)
//!   rejects the second exactly like the synchronous path does. **The
//!   explicit-signature escape hatch is not available for async natives in
//!   this version**; give the two methods distinct Rust argument-tuple types
//!   instead.
//! * **Cancellation.** Java-side `cancel()` does **not** propagate to the
//!   Rust future in v1: the worker thread keeps running and its eventual
//!   result is discarded by the (already cancelled) future.
//! * **Executor.** The std path runs the future under
//!   [`futures_lite::future::block_on`], whose waker parks and wakes the
//!   worker thread for real — executor-agnostic futures complete: plain
//!   computation, other `rjava` bridges, futures that park in `poll` waiting
//!   for a wakeup delivered through their waker, cross-thread channels.
//!   Futures that need a tokio runtime (timers, IO, tokio channels) work
//!   when the call happens inside one: with the `tokio` feature the future
//!   is spawned on the current tokio runtime, where its wakers are real
//!   tokio wakers. Called outside any tokio runtime (or without the
//!   `tokio` feature) such futures cannot make progress — a tokio timer's
//!   first poll panics without a runtime, and the worker surfaces the panic
//!   as an exceptional completion — so make tokio-dependent calls from
//!   inside a tokio runtime.
//!
//! Both bridges need a JVM the same way everything else in `rjava` does: the
//! [`Java`] facade passed in is `Send + Sync`, and the worker thread attaches
//! through it.

use std::future::Future;
use std::marker::PhantomData;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use jni::objects::{JObject as JniObject, JThrowable};
use jni::signature::{MethodSignature, RuntimeMethodSignature};
use jni::strings::JNIString;
use jni::{Env, JValue, JValueOwned};

use crate::call;
use crate::convert::{FromJava, JavaArg, ToJava};
use crate::error::{JavaError, JavaResult};
use crate::handles::JObject;
use crate::java::Java;
use crate::native::{
    registry_get, registry_insert, throw_error, NativeArg, NativeArgs, NativeCall, NativeMethod,
};

/// The shared state of a [`crate::future::java_future()`] bridge, guarded by a `Mutex`.
///
/// The worker thread and the polling side communicate exclusively through
/// this state: the poller spawns the worker out of [`State::Unstarted`] and
/// observes the outcome in [`State::Done`]; the worker stores the outcome and
/// wakes the last registered waker. Both transitions happen under the same
/// lock, so a poll that observes [`State::Waiting`] can never miss the
/// completion (the worker needs the lock to reach `Done`).
enum State<T> {
    /// Not yet polled: the `CompletableFuture` handle to block on.
    Unstarted {
        /// The JVM facade the worker thread attaches through.
        java: Java,
        /// The `CompletableFuture` to call `get()` on.
        cf: JObject,
    },
    /// The worker is running (or about to be spawned); `waker` is the latest
    /// waker registered by a poll.
    Waiting {
        /// The waker to wake when the outcome is ready.
        waker: Option<Waker>,
    },
    /// The outcome is ready. `None` means it has already been returned by a
    /// poll (the future is then spent — polling again is a bug).
    Done(Option<JavaResult<T>>),
}

/// The future returned by [`crate::future::java_future()`]. Private: the public constructor is
/// the function, and the concrete type is exposed as `impl Future`.
struct JavaFuture<T> {
    /// Shared state; cloned into the worker thread at spawn.
    state: Arc<Mutex<State<T>>>,
}

/// Bridge a Java `java.util.concurrent.CompletableFuture` to a Rust future.
///
/// On the first poll, one detached std thread is spawned; it attaches to the
/// JVM through `java`, blocks on `CompletableFuture.get()`, converts the
/// result (or surfaces the failure as [`JavaError::JavaException`], with
/// [`ExecutionException`](https://docs.oracle.com/en/java/javase/21/docs/api/java.base/java/util/concurrent/ExecutionException.html)
/// causes unwrapped), and wakes the future. See the [module docs](crate::future)
/// for the full semantics (lazy start, cancellation, timeouts, primitive
/// unboxing).
///
/// `T` is the Rust type of the Java future's value, selected by the caller's
/// annotation exactly like a method-call return type: `String` for a future
/// of `String`, `i32` for a future of boxed `Integer`, [`JObject`] for an
/// opaque object, and so on.
///
/// ```no_run
/// # use rjava::prelude::*;
/// # use rjava::future::java_future;
/// # async fn example(java: Java) -> JavaResult<()> {
/// let cf: JObject = java.new_object("java.util.concurrent.CompletableFuture", ())?;
/// let value: i32 = java_future::<i32>(java, cf).await?;
/// # let _ = value;
/// # Ok(())
/// # }
/// ```
pub fn java_future<T: FromJava + Send + 'static>(
    java: Java,
    cf: JObject,
) -> impl Future<Output = JavaResult<T>> + Send {
    JavaFuture {
        state: Arc::new(Mutex::new(State::Unstarted { java, cf })),
    }
}

impl<T: FromJava + Send + 'static> Future for JavaFuture<T> {
    type Output = JavaResult<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Clone the Arc first so the spawn closure can own a handle to the
        // same state even while the guard below borrows it.
        let state_arc = Arc::clone(&self.state);
        let mut state = state_arc.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            // First poll: take the startup data out, spawn the worker, and
            // re-enter the loop to register the waker (now `Waiting`).
            if let State::Unstarted { java, cf } = &mut *state {
                let java = java.clone();
                let cf = cf.clone();
                *state = State::Waiting { waker: None };
                let worker_state = Arc::clone(&state_arc);
                let spawned = std::thread::Builder::new()
                    .name("rjava-java-future".into())
                    .spawn(move || worker_thread::<T>(java, cf, worker_state));
                if spawned.is_err() {
                    // The worker can never run; the future can never complete
                    // normally, so surface the failure as the outcome.
                    let err = JavaError::InvalidArgument(
                        "failed to spawn the CompletableFuture bridge thread",
                    );
                    *state = State::Done(Some(Err(err)));
                    return Poll::Ready(Err(JavaError::InvalidArgument(
                        "failed to spawn the CompletableFuture bridge thread",
                    )));
                }
                continue;
            }
            match &mut *state {
                State::Waiting { waker } => {
                    let should_update = match waker {
                        Some(current) => !current.will_wake(cx.waker()),
                        None => true,
                    };
                    if should_update {
                        *waker = Some(cx.waker().clone());
                    }
                    return Poll::Pending;
                }
                State::Done(outcome) => match outcome.take() {
                    Some(outcome) => return Poll::Ready(outcome),
                    None => panic!("JavaFuture polled after completion"),
                },
                State::Unstarted { .. } => unreachable!("handled by the if-let above"),
            }
        }
    }
}

/// The worker thread body: block on `get()`, convert, store the outcome, and
/// wake the registered waker (outside the lock — a waker may re-enter poll
/// synchronously).
fn worker_thread<T: FromJava + Send + 'static>(
    java: Java,
    cf: JObject,
    state: Arc<Mutex<State<T>>>,
) {
    let outcome = fetch_completion::<T>(&java, &cf);
    let waker = {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        let waker = match &mut *state {
            State::Waiting { waker } => waker.take(),
            _ => None,
        };
        *state = State::Done(Some(outcome));
        waker
    };
    if let Some(waker) = waker {
        waker.wake();
    }
}

/// Block on `CompletableFuture.get()`, convert the result to `T`, and
/// translate failures into [`JavaError`] — unwrapping one level of
/// `ExecutionException` cause.
fn fetch_completion<T: FromJava + Send + 'static>(java: &Java, cf: &JObject) -> JavaResult<T> {
    java.with_env(|env| {
        // CompletableFuture.get()'s erased signature — the exact one.
        let rms =
            RuntimeMethodSignature::from_str("()Ljava/lang/Object;").map_err(JavaError::from)?;
        let sig: MethodSignature = (&rms).into();
        let value = match env.call_method(&*cf.global, JNIString::from("get"), sig, &[]) {
            Ok(value) => value,
            Err(e) => {
                // A failed get() leaves a pending Java exception
                // (ExecutionException, CancellationException,
                // InterruptedException). Prefer it over the raw JNI error,
                // unwrapping the cause (the crate's convention: the exception
                // is the ground truth).
                return Err(if env.exception_check() {
                    let throwable = env.exception_occurred();
                    env.exception_clear();
                    match throwable {
                        Some(throwable) => exception_error(env, &throwable),
                        None => JavaError::from(e),
                    }
                } else {
                    JavaError::from(e)
                });
            }
        };
        convert_value::<T>(env, value)
    })
}

/// Convert the `Object` returned by `get()` into `T`, unboxing wrapper
/// objects for primitive annotations (a `CompletableFuture<Integer>` yields
/// an `Integer` object; `java_future::<i32>` needs `intValue()`).
fn convert_value<'env, T: FromJava>(
    env: &mut Env<'env>,
    value: JValueOwned<'env>,
) -> JavaResult<T> {
    let value = unbox_for_annotation::<T>(env, value)?;
    T::from_java(env, value)
}

/// If `T` is a primitive, `get()`'s `Object` result is a boxed wrapper;
/// unbox it so `T::from_java` sees the primitive it expects. Non-primitive
/// annotations pass the value through unchanged.
fn unbox_for_annotation<'env, T: FromJava>(
    env: &mut Env<'env>,
    value: JValueOwned<'env>,
) -> JavaResult<JValueOwned<'env>> {
    let (method, ret) = match T::java_return_type().as_str() {
        "Z" => ("booleanValue", "Z"),
        "B" => ("byteValue", "B"),
        "C" => ("charValue", "C"),
        "S" => ("shortValue", "S"),
        "I" => ("intValue", "I"),
        "J" => ("longValue", "J"),
        "F" => ("floatValue", "F"),
        "D" => ("doubleValue", "D"),
        _ => return Ok(value),
    };
    let value = match value {
        JValueOwned::Object(obj) if !obj.is_null() => {
            let rms =
                RuntimeMethodSignature::from_str(format!("(){ret}")).map_err(JavaError::from)?;
            let sig: MethodSignature = (&rms).into();
            let unboxed = env
                .call_method(&obj, JNIString::from(method), sig, &[])
                .map_err(JavaError::from)?;
            call::check_exception(env)?;
            unboxed
        }
        JValueOwned::Object(_) => {
            return Err(JavaError::InvalidArgument(
                "the future completed with null but a Java primitive value was \
                 expected (annotate with Option<T> to accept null)",
            ));
        }
        other => other,
    };
    Ok(value)
}

/// Build the [`JavaError::JavaException`] for a throwable, preferring its
/// cause when one exists (the `ExecutionException` unwrap).
fn exception_error(env: &mut Env<'_>, throwable: &JThrowable<'_>) -> JavaError {
    let (class, message) = match throwable.get_cause(env) {
        Ok(cause) if !cause.is_null() => throwable_info(env, &cause),
        _ => throwable_info(env, throwable),
    };
    JavaError::JavaException { class, message }
}

/// The class name (`Class.getName()`) and message (`Throwable.getMessage()`)
/// of a throwable, falling back to a generic class when the queries fail
/// (they essentially cannot — the JNI calls are infallible for a valid
/// throwable).
fn throwable_info(env: &mut Env<'_>, throwable: &JThrowable<'_>) -> (String, String) {
    let class = env
        .get_object_class(throwable)
        .and_then(|class| class.get_name(env))
        .map(|name| name.to_string())
        .unwrap_or_else(|_| String::from("java.lang.Throwable"));
    let message = throwable
        .get_message(env)
        .map(|msg| msg.to_string())
        .unwrap_or_default();
    (class, message)
}

// ---------------------------------------------------------------------------
// Rust → Java: async natives (`async_native!` → make_async_native)
//
// The async twin of the type-derived `native!` form: the Java method returns
// a `java.util.concurrent.CompletableFuture`, the trampoline hands the
// converted arguments to a detached std thread that runs the Rust async
// function to completion and then completes the Java future (or completes it
// exceptionally). Like the sync type-derived form, the C ABI is fixed at
// compile time by shared generic trampolines — here keyed by the argument
// tuple `A` only, because the C return type is `jobject` for *every* async
// native (Java always sees a `CompletableFuture`; the Rust value type `R` is
// an implementation detail of the worker) — so two different registrations
// with the same `A` are detected and rejected by the same
// process-global registry the sync path uses.
// ---------------------------------------------------------------------------

/// The registered callable behind a shared async trampoline: holds the user's
/// async function and runs the whole pipeline — arity check, `FromJava`
/// conversion, `CompletableFuture` creation, worker spawn — synchronously in
/// the trampoline, returning the future object to Java.
///
/// `F` is behind an `Arc` so each call's worker thread can own a `'static`
/// handle to it without requiring `F: Clone`.
struct AsyncAdapter<F, A, R> {
    f: Arc<F>,
    marker: PhantomData<(A, R)>,
}

impl<F, Fut, A, R> NativeCall for AsyncAdapter<F, A, R>
where
    F: Fn(Java, A) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = JavaResult<R>> + Send + 'static,
    A: for<'env> NativeArgs<'env> + Send + Sync + 'static,
    R: ToJava + Send + Sync + 'static,
{
    fn call<'env>(
        &self,
        env: &mut Env<'env>,
        args: Vec<JavaArg<'env>>,
    ) -> JavaResult<JValueOwned<'env>> {
        // Same arity check and argument conversion as the sync `dispatch` —
        // a mismatch is the most common mistake and must fail loudly, thrown
        // into Java exactly like a sync native's would be.
        if A::ARITY != args.len() {
            let err = JavaError::InvalidArgument(
                "async native method: the Rust async function takes a different number \
                 of arguments than its JNI signature declares (arity mismatch)",
            );
            let _ = throw_error(env, err);
            return Ok(JValueOwned::Object(JniObject::null()));
        }
        let a = match A::from_args(env, args) {
            Ok(a) => a,
            Err(e) => {
                let _ = throw_error(env, e);
                return Ok(JValueOwned::Object(JniObject::null()));
            }
        };
        let java = match Java::from_env(env) {
            Ok(java) => java,
            Err(e) => {
                let _ = throw_error(env, e);
                return Ok(JValueOwned::Object(JniObject::null()));
            }
        };
        // The CompletableFuture the worker completes; returned to Java now.
        let cf: JObject = match create_completable_future(env) {
            Ok(cf) => cf,
            Err(e) => {
                let _ = throw_error(env, e);
                return Ok(JValueOwned::Object(JniObject::null()));
            }
        };
        // Spawn the worker. With the `tokio` feature and a current tokio
        // runtime (the Java caller is inside one), the user's future runs as
        // a task ON that runtime — real tokio wakers, so tokio timers and IO
        // work (`async_worker_tokio`). Otherwise a detached std thread runs
        // it under `futures_lite::future::block_on`, whose thread-parking
        // waker drives executor-agnostic futures to completion
        // (`async_worker`). A spawn failure is surfaced exceptionally so the
        // Java side sees a completed-with-error future instead of one that
        // never completes.
        let worker_f = Arc::clone(&self.f);
        let worker_java = java.clone();
        let worker_cf = cf.clone();
        #[cfg(feature = "tokio")]
        if tokio::runtime::Handle::try_current().is_ok() {
            async_worker_tokio::<F, Fut, A, R>(worker_java, a, worker_cf, worker_f);
            // The JVM's local reference lives until the native method
            // returns, which is exactly the lifetime of this `JValueOwned`.
            return Ok(JValueOwned::Object(env.new_local_ref(&*cf.global)?));
        }
        let spawned = std::thread::Builder::new()
            .name("rjava-async-native".into())
            .spawn(move || async_worker::<F, Fut, A, R>(worker_java, a, worker_cf, worker_f));
        if spawned.is_err() {
            complete_exceptionally(
                &java,
                &cf,
                JavaError::InvalidArgument("failed to spawn the async native worker thread"),
            );
        }
        // The JVM's local reference lives until the native method returns,
        // which is exactly the lifetime of this `JValueOwned`.
        Ok(JValueOwned::Object(env.new_local_ref(&*cf.global)?))
    }
}

/// The worker thread body (std path): run the user's future to completion
/// under [`futures_lite::future::block_on`] — a real thread-parking waker,
/// so executor-agnostic futures (including ones that park in `poll` waiting
/// for their waker) complete — then complete the Java `CompletableFuture`
/// with the converted result, or exceptionally with the error/panic.
fn async_worker<F, Fut, A, R>(java: Java, args: A, cf: JObject, f: Arc<F>)
where
    F: Fn(Java, A) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = JavaResult<R>> + Send + 'static,
    A: Send + 'static,
    R: ToJava + Send + 'static,
{
    // A panic in this thread would otherwise kill it silently and leave the
    // Java future pending forever; catch it and complete exceptionally,
    // mirroring the sync path's panic → RuntimeException rule.
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let fut = f(java.clone(), args);
        futures_lite::future::block_on(fut)
    }))
    .map_err(|payload| {
        JavaError::JavaException {
            class: "java.lang.RuntimeException".to_string(),
            message: format!("async native method panicked: {}", panic_message(&payload)),
        }
    });
    match outcome {
        Ok(Ok(value)) => complete_value(&java, &cf, value),
        Ok(Err(err)) | Err(err) => complete_exceptionally(&java, &cf, err),
    }
}

/// The worker body for the tokio path: the Java caller is inside a tokio
/// runtime, so the user's future is spawned ON that runtime as a task —
/// tokio polls it with real wakers, so tokio timers and IO work. The future
/// runs in a nested task so a panic surfaces through the `JoinHandle`
/// (instead of aborting this one and leaving the Java future pending); the
/// outer task completes the Java `CompletableFuture` exactly like
/// [`async_worker`] does — with the converted result, or exceptionally with
/// the error/panic.
#[cfg(feature = "tokio")]
fn async_worker_tokio<F, Fut, A, R>(java: Java, args: A, cf: JObject, f: Arc<F>)
where
    F: Fn(Java, A) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = JavaResult<R>> + Send + 'static,
    A: Send + 'static,
    R: ToJava + Send + 'static,
{
    tokio::spawn(async move {
        // `f` is called inside the nested task too, so a synchronous panic
        // in the user function (before the first await) is caught the same
        // way as one inside the future.
        let completion_java = java.clone();
        let outcome = match tokio::spawn(async move { f(java, args).await }).await {
            Ok(result) => result,
            Err(join_err) => {
                let message = join_err
                    .try_into_panic()
                    .map_or_else(
                        |_| "async native method task was cancelled".to_string(),
                        |payload| {
                            format!("async native method panicked: {}", panic_message(&payload))
                        },
                    );
                Err(JavaError::JavaException {
                    class: "java.lang.RuntimeException".to_string(),
                    message,
                })
            }
        };
        match outcome {
            Ok(value) => complete_value(&completion_java, &cf, value),
            Err(err) => complete_exceptionally(&completion_java, &cf, err),
        }
    });
}

/// Convert `value` (`R`) into an object and call `CompletableFuture.complete`
/// on the worker's behalf. `()` completes with `null`; primitives are boxed
/// into their wrapper classes (they cannot be passed to `complete(Object)`).
fn complete_value<R: ToJava + Send>(java: &Java, cf: &JObject, value: R) {
    let result: JavaResult<()> = java.with_env(|env| {
        let mut out = value.to_java(env)?;
        match out.len() {
            0 => complete_call(env, cf, None),
            1 => {
                let arg = out.pop().expect("len == 1");
                let obj = match arg {
                    JavaArg::Object(o) => o,
                    primitive => call::box_primitive(env, &primitive)?,
                };
                complete_call(env, cf, Some(obj))
            }
            _ => Err(JavaError::InvalidArgument(
                "async native method: the Rust async function returned more than one Java value",
            )),
        }
    });
    if let Err(err) = result {
        // The value could not be converted/boxed — a programming error. Turn
        // it into an exceptional completion so Java sees a failure instead of
        // a future that never completes.
        complete_exceptionally(java, cf, err);
    }
}

/// Call `CompletableFuture.complete(Object)`; `None` means `null`.
fn complete_call<'env>(
    env: &mut Env<'env>,
    cf: &JObject,
    value: Option<JniObject<'env>>,
) -> JavaResult<()> {
    let rms =
        RuntimeMethodSignature::from_str("(Ljava/lang/Object;)Z").map_err(JavaError::from)?;
    let sig: MethodSignature = (&rms).into();
    let null = JniObject::null();
    let value = value.as_ref().unwrap_or(&null);
    call::with_check(env, |env| {
        env.call_method(
            &*cf.global,
            JNIString::from("complete"),
            sig,
            &[JValue::Object(value)],
        )
    })?;
    Ok(())
}

/// Complete the Java `CompletableFuture` exceptionally with a materialized
/// `Throwable` for `err` (see [`build_throwable`] for the mapping). Failures
/// of the completion itself are ignored: the future is already failed or
/// cancelled on the Java side in the worst case.
fn complete_exceptionally(java: &Java, cf: &JObject, err: JavaError) {
    let _: JavaResult<()> = java.with_env(|env| {
        let throwable = build_throwable(env, &err)?;
        let rms = RuntimeMethodSignature::from_str("(Ljava/lang/Throwable;)Z")
            .map_err(JavaError::from)?;
        let sig: MethodSignature = (&rms).into();
        call::with_check(env, |env| {
            env.call_method(
                &*cf.global,
                JNIString::from("completeExceptionally"),
                sig,
                &[JValue::Object(&throwable)],
            )
        })?;
        Ok(())
    });
}

/// Materialize a [`JavaError`] as a Java `Throwable` object, mirroring
/// [`throw_error`]'s rules: [`JavaError::JavaException`]`{ class, message }`
/// becomes an instance of `class` constructed with `message`; any other error
/// becomes a `java.lang.RuntimeException` carrying the error's `Debug`
/// rendering. If the named class cannot be found or has no `(String)`
/// constructor, the `RuntimeException` fallback is used.
fn build_throwable<'env>(env: &mut Env<'env>, err: &JavaError) -> JavaResult<JniObject<'env>> {
    let (class, message) = match err {
        JavaError::JavaException { class, message } => (class.clone(), message.clone()),
        other => (
            "java.lang.RuntimeException".to_string(),
            format!("{other:?}"),
        ),
    };
    let slash = class.replace('.', "/");
    let cls = match call::find_class(env, JNIString::from(slash)) {
        Ok(cls) => cls,
        Err(_) => call::find_class(env, JNIString::from("java/lang/RuntimeException"))?,
    };
    let cls_global = env.new_global_ref(cls)?;
    let obj = match call::new_object(env, &cls_global, &(message.clone(),)) {
        Ok(obj) => obj,
        Err(_) => {
            let fallback = call::find_class(env, JNIString::from("java/lang/RuntimeException"))?;
            let fb_global = env.new_global_ref(fallback)?;
            call::new_object(env, &fb_global, &(message,))?
        }
    };
    Ok(env.new_local_ref(&*obj.global)?)
}

/// Create a new `java.util.concurrent.CompletableFuture` object.
fn create_completable_future<'env>(env: &mut Env<'env>) -> JavaResult<JObject> {
    let cls = call::find_class(env, JNIString::from("java/util/concurrent/CompletableFuture"))?;
    let cls_global = env.new_global_ref(cls)?;
    call::new_object(env, &cls_global, &())
}

/// The human-readable payload of a caught panic.
///
/// Mirrors `crate::native`'s private helper of the same shape: panics may be
/// caught and re-thrown (`resume_unwind`) by nested `catch_unwind` wrappers,
/// which re-boxes the payload as `Box<dyn Any + Send>`; unwrap those
/// recursively. (Kept private here — the native module's copy is private, so
/// this one is duplicated rather than shared across module boundaries.)
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

/// Per-arity generic `extern "system"` trampolines for async natives. Each
/// instantiation's C signature is the exact JNI ABI for its `A` types (the
/// [`NativeArg::CType`]s) with a `jobject` return — Java always sees a
/// `CompletableFuture` regardless of `R`, so `R` is not part of the C
/// signature (it is fixed at runtime by the registered callable). Same
/// registry + dispatch shape as the sync trampolines in `crate::native`.
macro_rules! impl_async_trampolines {
    ($tramp:ident, $($A:ident),*) => {
        #[doc(hidden)]
        #[allow(improper_ctypes, non_snake_case)]
        pub extern "system" fn $tramp<'caller, $($A: NativeArg,)*>(
            mut __env: ::jni::EnvUnowned<'caller>,
            _receiver: ::jni::objects::JClass<'caller>,
            $($A: <$A as NativeArg>::CType<'caller>,)*
        ) -> ::jni::objects::JObject<'caller> {
            let __key = ($tramp::<$($A,)*> as for<'c> extern "system" fn(
                ::jni::EnvUnowned<'c>,
                ::jni::objects::JClass<'c>,
                $(<$A as NativeArg>::CType<'c>,)*
            ) -> ::jni::objects::JObject<'c>) as usize;
            match __env.with_env(
                |__env| -> ::std::result::Result<::jni::objects::JObject<'caller>, ::jni::errors::Error> {
                    let __arc = ::std::sync::Arc::clone(&registry_get(__key).expect(
                        "rjava: async native method not registered (was the NativeMethod built by async_native!?)",
                    ));
                    let __args: ::std::vec::Vec<JavaArg<'caller>> = ::std::vec![
                        $(<$A as NativeArg>::arg_to_java($A),)*
                    ];
                    match __arc.call(__env, __args) {
                        ::std::result::Result::Ok(__v) => match __v {
                            JValueOwned::Object(__o) => ::std::result::Result::Ok(__o),
                            _ => ::std::result::Result::Ok(::jni::objects::JObject::null()),
                        },
                        ::std::result::Result::Err(__e) => {
                            let _ = throw_error(__env, __e);
                            ::std::result::Result::Ok(::jni::objects::JObject::null())
                        }
                    }
                },
            ).into_outcome() {
                ::jni::Outcome::Ok(__v) => __v,
                // The non-`Ok` arms (its own catch_unwind already covers the
                // panics of `call`) are rare; returning null is acceptable —
                // the JVM observes a null `CompletableFuture`.
                _ => ::jni::objects::JObject::null(),
            }
        }
    };
}

impl_async_trampolines!(tramp_async_0,);
impl_async_trampolines!(tramp_async_1, A1);
impl_async_trampolines!(tramp_async_2, A1, A2);
impl_async_trampolines!(tramp_async_3, A1, A2, A3);
impl_async_trampolines!(tramp_async_4, A1, A2, A3, A4);
impl_async_trampolines!(tramp_async_5, A1, A2, A3, A4, A5);
impl_async_trampolines!(tramp_async_6, A1, A2, A3, A4, A5, A6);
impl_async_trampolines!(tramp_async_7, A1, A2, A3, A4, A5, A6, A7);
impl_async_trampolines!(tramp_async_8, A1, A2, A3, A4, A5, A6, A7, A8);
impl_async_trampolines!(tramp_async_9, A1, A2, A3, A4, A5, A6, A7, A8, A9);
impl_async_trampolines!(tramp_async_10, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10);
impl_async_trampolines!(tramp_async_11, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11);
impl_async_trampolines!(tramp_async_12, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12);
impl_async_trampolines!(tramp_async_13, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13);
impl_async_trampolines!(tramp_async_14, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14);
impl_async_trampolines!(tramp_async_15, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15);
impl_async_trampolines!(tramp_async_16, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16);
impl_async_trampolines!(tramp_async_17, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17);
impl_async_trampolines!(tramp_async_18, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18);
impl_async_trampolines!(tramp_async_19, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19);
impl_async_trampolines!(tramp_async_20, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20);
impl_async_trampolines!(tramp_async_21, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21);
impl_async_trampolines!(tramp_async_22, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22);
impl_async_trampolines!(tramp_async_23, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23);
impl_async_trampolines!(tramp_async_24, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24);
impl_async_trampolines!(tramp_async_25, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25);
impl_async_trampolines!(tramp_async_26, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26);
impl_async_trampolines!(tramp_async_27, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27);
impl_async_trampolines!(tramp_async_28, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28);
impl_async_trampolines!(tramp_async_29, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29);
impl_async_trampolines!(tramp_async_30, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30);
impl_async_trampolines!(tramp_async_31, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31);
impl_async_trampolines!(tramp_async_32, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32);
impl_async_trampolines!(tramp_async_33, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33);
impl_async_trampolines!(tramp_async_34, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34);
impl_async_trampolines!(tramp_async_35, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35);
impl_async_trampolines!(tramp_async_36, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36);
impl_async_trampolines!(tramp_async_37, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37);
impl_async_trampolines!(tramp_async_38, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38);
impl_async_trampolines!(tramp_async_39, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39);
impl_async_trampolines!(tramp_async_40, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40);
impl_async_trampolines!(tramp_async_41, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41);
impl_async_trampolines!(tramp_async_42, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42);
impl_async_trampolines!(tramp_async_43, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43);
impl_async_trampolines!(tramp_async_44, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44);
impl_async_trampolines!(tramp_async_45, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45);
impl_async_trampolines!(tramp_async_46, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46);
impl_async_trampolines!(tramp_async_47, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47);
impl_async_trampolines!(tramp_async_48, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48);
impl_async_trampolines!(tramp_async_49, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49);
impl_async_trampolines!(tramp_async_50, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50);
impl_async_trampolines!(tramp_async_51, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51);
impl_async_trampolines!(tramp_async_52, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52);
impl_async_trampolines!(tramp_async_53, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53);
impl_async_trampolines!(tramp_async_54, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54);
impl_async_trampolines!(tramp_async_55, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55);
impl_async_trampolines!(tramp_async_56, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56);
impl_async_trampolines!(tramp_async_57, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57);
impl_async_trampolines!(tramp_async_58, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58);
impl_async_trampolines!(tramp_async_59, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59);
impl_async_trampolines!(tramp_async_60, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60);
impl_async_trampolines!(tramp_async_61, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61);
impl_async_trampolines!(tramp_async_62, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61, A62);
impl_async_trampolines!(tramp_async_63, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61, A62, A63);
impl_async_trampolines!(tramp_async_64, A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61, A62, A63, A64);

/// Maps a user argument tuple to the shared **async** trampoline for its
/// types, fixing its address at compile time — the async analog of
/// [`crate::native::TrampBridgeStatic`], keyed by `A` only (the C return is
/// always `jobject`, since Java sees a `CompletableFuture` regardless of
/// `R`).
///
/// The casts below are plain `as` casts of fn items to fn pointers to raw
/// pointers — no `unsafe` — and the sealed [`NativeArg`] trait guarantees the
/// target C signature matches the JNI descriptor derived at runtime
/// ([`Self::args_sig`]).
#[doc(hidden)]
pub trait AsyncTrampBridge: Sized {
    /// The address of the shared async trampoline for `Self`.
    const PTR: *mut std::ffi::c_void;
    /// The JNI type fragments of the parameters (no parentheses; the receiver
    /// is not part of a static method's descriptor).
    fn args_sig() -> String;
}

macro_rules! impl_async_bridge {
    ($tramp:ident; $($A:ident),*) => {
        impl<$($A: NativeArg,)*> AsyncTrampBridge for ($($A,)*) {
            const PTR: *mut std::ffi::c_void = ($tramp::<$($A,)*> as for<'c> extern "system" fn(
                ::jni::EnvUnowned<'c>,
                ::jni::objects::JClass<'c>,
                $(<$A as NativeArg>::CType<'c>,)*
            ) -> ::jni::objects::JObject<'c>) as *mut std::ffi::c_void;
            fn args_sig() -> String {
                let mut __frag = String::new();
                $(__frag.push_str(&<$A as NativeArg>::arg_sig());)*
                __frag
            }
        }
    };
}

impl_async_bridge!(tramp_async_0;);
impl_async_bridge!(tramp_async_1; A1);
impl_async_bridge!(tramp_async_2; A1, A2);
impl_async_bridge!(tramp_async_3; A1, A2, A3);
impl_async_bridge!(tramp_async_4; A1, A2, A3, A4);
impl_async_bridge!(tramp_async_5; A1, A2, A3, A4, A5);
impl_async_bridge!(tramp_async_6; A1, A2, A3, A4, A5, A6);
impl_async_bridge!(tramp_async_7; A1, A2, A3, A4, A5, A6, A7);
impl_async_bridge!(tramp_async_8; A1, A2, A3, A4, A5, A6, A7, A8);
impl_async_bridge!(tramp_async_9; A1, A2, A3, A4, A5, A6, A7, A8, A9);
impl_async_bridge!(tramp_async_10; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10);
impl_async_bridge!(tramp_async_11; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11);
impl_async_bridge!(tramp_async_12; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12);
impl_async_bridge!(tramp_async_13; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13);
impl_async_bridge!(tramp_async_14; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14);
impl_async_bridge!(tramp_async_15; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15);
impl_async_bridge!(tramp_async_16; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16);
impl_async_bridge!(tramp_async_17; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17);
impl_async_bridge!(tramp_async_18; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18);
impl_async_bridge!(tramp_async_19; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19);
impl_async_bridge!(tramp_async_20; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20);
impl_async_bridge!(tramp_async_21; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21);
impl_async_bridge!(tramp_async_22; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22);
impl_async_bridge!(tramp_async_23; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23);
impl_async_bridge!(tramp_async_24; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24);
impl_async_bridge!(tramp_async_25; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25);
impl_async_bridge!(tramp_async_26; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26);
impl_async_bridge!(tramp_async_27; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27);
impl_async_bridge!(tramp_async_28; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28);
impl_async_bridge!(tramp_async_29; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29);
impl_async_bridge!(tramp_async_30; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30);
impl_async_bridge!(tramp_async_31; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31);
impl_async_bridge!(tramp_async_32; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32);
impl_async_bridge!(tramp_async_33; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33);
impl_async_bridge!(tramp_async_34; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34);
impl_async_bridge!(tramp_async_35; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35);
impl_async_bridge!(tramp_async_36; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36);
impl_async_bridge!(tramp_async_37; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37);
impl_async_bridge!(tramp_async_38; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38);
impl_async_bridge!(tramp_async_39; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39);
impl_async_bridge!(tramp_async_40; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40);
impl_async_bridge!(tramp_async_41; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41);
impl_async_bridge!(tramp_async_42; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42);
impl_async_bridge!(tramp_async_43; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43);
impl_async_bridge!(tramp_async_44; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44);
impl_async_bridge!(tramp_async_45; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45);
impl_async_bridge!(tramp_async_46; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46);
impl_async_bridge!(tramp_async_47; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47);
impl_async_bridge!(tramp_async_48; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48);
impl_async_bridge!(tramp_async_49; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49);
impl_async_bridge!(tramp_async_50; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50);
impl_async_bridge!(tramp_async_51; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51);
impl_async_bridge!(tramp_async_52; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52);
impl_async_bridge!(tramp_async_53; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53);
impl_async_bridge!(tramp_async_54; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54);
impl_async_bridge!(tramp_async_55; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55);
impl_async_bridge!(tramp_async_56; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56);
impl_async_bridge!(tramp_async_57; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57);
impl_async_bridge!(tramp_async_58; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58);
impl_async_bridge!(tramp_async_59; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59);
impl_async_bridge!(tramp_async_60; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60);
impl_async_bridge!(tramp_async_61; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61);
impl_async_bridge!(tramp_async_62; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61, A62);
impl_async_bridge!(tramp_async_63; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61, A62, A63);
impl_async_bridge!(tramp_async_64; A1, A2, A3, A4, A5, A6, A7, A8, A9, A10, A11, A12, A13, A14, A15, A16, A17, A18, A19, A20, A21, A22, A23, A24, A25, A26, A27, A28, A29, A30, A31, A32, A33, A34, A35, A36, A37, A38, A39, A40, A41, A42, A43, A44, A45, A46, A47, A48, A49, A50, A51, A52, A53, A54, A55, A56, A57, A58, A59, A60, A61, A62, A63, A64);

/// Build a type-derived **async** native-method descriptor for `name` backed
/// by `f` (a closure or fn item returning a `Future`). The JNI parameter
/// descriptor is derived from the Rust types of `A`; the return descriptor is
/// always exactly `Ljava/util/concurrent/CompletableFuture;`.
///
/// Called by the `async_native!("name", f)` macro form; the expansion is
/// plain safe code, so `#![forbid(unsafe_code)]` user crates keep compiling.
/// See the [module docs](crate::future) for the full semantics (thread-per-
/// call, exception mapping, collision rule, cancellation).
#[doc(hidden)]
pub fn make_async_native<F, Fut, A, R>(name: &str, f: F) -> JavaResult<NativeMethod>
where
    F: Fn(Java, A) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = JavaResult<R>> + Send + 'static,
    A: for<'env> NativeArgs<'env> + AsyncTrampBridge + Send + Sync + 'static,
    R: ToJava + Send + Sync + 'static,
{
    let sig = format!("({})Ljava/util/concurrent/CompletableFuture;", A::args_sig());
    let ptr = <A as AsyncTrampBridge>::PTR;
    let adapter: Arc<dyn NativeCall> = Arc::new(AsyncAdapter::<F, A, R> {
        f: Arc::new(f),
        marker: PhantomData,
    });
    registry_insert(ptr as usize, Arc::clone(&adapter));
    Ok(NativeMethod {
        name: name.to_string(),
        sig,
        fn_ptr: ptr,
        call: Some(adapter),
    })
}
