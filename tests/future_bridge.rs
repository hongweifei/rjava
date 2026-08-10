#![forbid(unsafe_code)]
//! Integration tests for the `CompletableFuture` bridges in `rjava::future`:
//!
//! * **Java → Rust**: `rjava::future::java_future` — the existing tests below
//!   use only JDK classes (no fixtures).
//! * **Rust → Java**: `rjava::async_native!` — the new tests register Rust
//!   async functions on the `tests/java/asyncnative/NativeLib.java` fixture
//!   (compiled with `javac` like `tests/integration.rs`'s fixture) and
//!   exercise the full circle: call the native → get the `CompletableFuture`
//!   → await it through `java_future`.
//!
//! The `#![forbid(unsafe_code)]` mirrors tests/integration.rs: the bridges
//! themselves and this harness are entirely safe code (the async_native!
//! expansion is a plain safe call, like the native! expansion).
//!
//! Futures are driven with `futures_lite::future::block_on` — the same
//! executor the async-native worker threads use — so a future that only
//! makes progress through its waker (the park-based and tokio-timer tests
//! below) is driven exactly as the worker would drive it.
//!
//! If no JVM (or no `javac`) can be located, every test skips gracefully
//! with a reason.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use futures_lite::future::block_on;
use rjava::future::java_future;
use rjava::prelude::*;
use rjava::{async_native, JavaError};

/// Create the singleton JVM, skipping with a reason when none is available.
/// The fixture is compiled first and put on the class path so the
/// async-native tests can look `asyncnative.NativeLib` up (harmless for the
/// JDK-only tests — the JVM is created once per process, whichever test runs
/// first).
fn jvm() -> Option<Java> {
    if let Err(reason) = FIXTURE_COMPILED.get_or_init(compile_fixture) {
        eprintln!("SKIPPING test: could not compile Java fixture: {reason}");
        return None;
    }
    match Java::builder()
        .class_path("target/rjava-asyncnative-classes")
        .build()
    {
        Ok(java) => Some(java),
        Err(e) => {
            eprintln!("SKIPPING test: no JVM available: {e}");
            None
        }
    }
}

/// Compile the `asyncnative` fixture exactly once per test process. `Err`
/// carries the reason, reported once; when it fails every test skips.
static FIXTURE_COMPILED: OnceLock<Result<(), String>> = OnceLock::new();

fn compile_fixture() -> Result<(), String> {
    // `javac -d` creates the output directory itself; create it first anyway
    // so a failure here is reported as a compile problem, not a mystery.
    let out_dir = "target/rjava-asyncnative-classes";
    std::fs::create_dir_all(out_dir).map_err(|e| format!("cannot create {out_dir}: {e}"))?;
    let javac = match std::env::var("JAVA_HOME") {
        Ok(home) => format!(
            "{home}/bin/javac{}",
            if cfg!(windows) { ".exe" } else { "" }
        ),
        Err(_) => "javac".to_string(),
    };
    let output = std::process::Command::new(&javac)
        .arg("-d")
        .arg(out_dir)
        .arg("tests/java/asyncnative/NativeLib.java")
        .output()
        .map_err(|e| format!("could not run `{javac}`: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "`{javac}` failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

// ---------------------------------------------------------------------------
// Java → Rust: java_future (JDK classes only)
// ---------------------------------------------------------------------------

#[test]
fn future_completed_from_rust_thread() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };
    let cf: JObject = java.new_object("java.util.concurrent.CompletableFuture", ())?;

    // Complete it from a plain std::thread ~150ms later. The thread is not
    // attached yet; the `call_void` auto-attaches it for the call.
    let completer = cf.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        completer
            .call_void("complete", ("done",))
            .expect("complete(\"done\") must succeed on the JVM thread");
    });

    // The whole bridge: a std thread → JVM call → future completion →
    // Rust await, blocking until the Rust side completes it.
    let result = block_on(java_future::<String>(java, cf));
    assert_eq!(result?, "done");
    Ok(())
}

#[test]
fn future_already_completed() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };
    let cf: JObject = java.new_object("java.util.concurrent.CompletableFuture", ())?;
    cf.call_void("complete", ("done",))?;

    // get() returns immediately, so the bridge resolves without waiting for
    // any other thread.
    let result = block_on(java_future::<String>(java, cf));
    assert_eq!(result?, "done");
    Ok(())
}

#[test]
fn future_exception_surfaces() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };
    let cf: JObject = java.new_object("java.util.concurrent.CompletableFuture", ())?;
    let ex: JObject = java.new_object("java.lang.IllegalArgumentException", ("nope",))?;
    cf.call_void("completeExceptionally", (ex,))?;

    let result = block_on(java_future::<String>(java, cf));
    match result {
        // get() wraps the failure in ExecutionException; the bridge unwraps
        // the cause, so the *original* exception surfaces.
        Err(JavaError::JavaException { class, message }) => {
            assert_eq!(class, "java.lang.IllegalArgumentException");
            assert_eq!(message, "nope");
        }
        other => panic!("expected the cause to surface, got {other:?}"),
    }
    Ok(())
}

#[test]
fn future_primitive_result() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };
    let cf: JObject = java.new_object("java.util.concurrent.CompletableFuture", ())?;

    // complete(Object): the 42_i32 auto-boxes into an Integer via the
    // reflection fallback's boxing pass, exercising that path too.
    cf.call_void("complete", (42_i32,))?;

    // get() returns the boxed Integer; the bridge unboxes it for the i32
    // annotation.
    let result = block_on(java_future::<i32>(java, cf));
    assert_eq!(result?, 42);
    Ok(())
}

// ---------------------------------------------------------------------------
// Rust → Java: async_native!
// ---------------------------------------------------------------------------

// Java: `public static native CompletableFuture<Integer> compute(int a, int b);`
// A=(i32, i32), R=i32 — the fn-item form.
async fn async_compute(java: Java, (a, b): (i32, i32)) -> JavaResult<i32> {
    // Call back into Java through the facade before any await — proves the
    // `Java` parameter is usable inside the async function.
    let abs: i32 = java.class("java.lang.Math")?.call_static("abs", (-7_i32,))?;
    assert_eq!(abs, 7);
    Ok(a + b)
}

// Java: `public static native CompletableFuture<String> fail(String msg, int code);`
// A=(String, i32), R=String — the Err path.
async fn async_fail(java: Java, (msg, _code): (String, i32)) -> JavaResult<String> {
    let _ = java;
    Err(JavaError::JavaException {
        class: "java.lang.IllegalArgumentException".to_string(),
        message: msg,
    })
}

// Java: `public static native CompletableFuture<String> boom();`
// A=(), R=String — the panic path.
async fn async_boom(java: Java, (): ()) -> JavaResult<String> {
    let _ = java;
    panic!("async boom");
}

// A=(bool, i32) — deliberately a signature no other test registers, so the
// collision check (which compares against the process-global registry) only
// sees the two registrations of *this* test.
async fn async_collision_a(java: Java, (_flag, n): (bool, i32)) -> JavaResult<i32> {
    let _ = java;
    Ok(n)
}
async fn async_collision_b(java: Java, (_flag, n): (bool, i32)) -> JavaResult<i32> {
    let _ = java;
    Ok(n + 1)
}

/// A future that parks the polling thread in `poll`, the way async-std
/// internals do — the classic noop-waker breaker. Completion is delivered by
/// the completing thread through two channels, exactly like a real reactor:
/// `wake()` (reaches the executor's own park, so the executor re-polls) and
/// `thread::unpark()` on the captured polling thread (unblocks the park
/// *inside* this poll — the two parking mechanisms are distinct, so both
/// channels are needed).
///
/// The poll parks with `park_timeout`, so a worker whose executor never
/// drives a re-poll fails the test with a bounded timeout error instead of
/// hanging CI forever: the 5s wall-clock budget of the test wrapper below
/// bounds any regression.
struct ParkFuture {
    state: Arc<ParkState>,
}

struct ParkState {
    /// Set by the completing thread; `poll` returns `Ready` only when it
    /// observes this set.
    done: AtomicBool,
    /// The waker the completing thread wakes; `take()`d after the wake.
    waker: Mutex<Option<Waker>>,
    /// The thread parked inside `poll`; set on the first poll so the
    /// completing thread can `unpark()` it (there is no other way to reach
    /// a thread parked with `std::thread::park_timeout`).
    thread: OnceLock<std::thread::Thread>,
}

impl Future for ParkFuture {
    type Output = &'static str;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let state = &self.state;
        state.thread.get_or_init(std::thread::current);
        if state.done.load(Ordering::Acquire) {
            return Poll::Ready("parked-done");
        }
        // Register the waker, then re-check: the flag may have flipped
        // between the first check and the registration.
        *state.waker.lock().unwrap_or_else(|e| e.into_inner()) = Some(cx.waker().clone());
        if state.done.load(Ordering::Acquire) {
            return Poll::Ready("parked-done");
        }
        // Block the polling thread. The completing thread's `unpark()` ends
        // the park the moment the flag flips; without it, only the timeout
        // guard does.
        std::thread::park_timeout(Duration::from_secs(2));
        if state.done.load(Ordering::Acquire) {
            Poll::Ready("parked-done")
        } else {
            Poll::Pending
        }
    }
}

// Java: `public static native CompletableFuture<String> parked(int n);`
// A=(i32,), R=String — the future parks in poll until a waker-driven
// completion (the classic noop-waker breaker).
async fn async_parked(java: Java, (_n,): (i32,)) -> JavaResult<String> {
    let _ = java;
    let state = Arc::new(ParkState {
        done: AtomicBool::new(false),
        waker: Mutex::new(None),
        thread: OnceLock::new(),
    });
    // The completing thread: flip the flag, then signal through both
    // channels — `wake()` for the executor's park, `unpark()` for the poll's
    // in-poll park.
    let completer = Arc::clone(&state);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        completer.done.store(true, Ordering::Release);
        if let Some(waker) = completer
            .waker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            waker.wake();
        }
        if let Some(thread) = completer.thread.get() {
            thread.unpark();
        }
    });
    ParkFuture { state }.await;
    Ok("parked-done".to_string())
}

// Java: `public static native CompletableFuture<String> tokioSleep(long ms);`
// A=(i64,), R=String — awaits a tokio timer, which needs a tokio runtime
// (only provided when the worker is spawned on one, `tokio` feature on).
async fn async_tokio_sleep(java: Java, (ms,): (i64,)) -> JavaResult<String> {
    let _ = java;
    tokio::time::sleep(Duration::from_millis(ms as u64)).await;
    Ok("tokio-done".to_string())
}

/// Register the shared async natives exactly once per test process (tests run
/// in parallel on one JVM; the registry and the class are process-global).
static NATIVES_REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();

fn register_natives() -> Result<(), String> {
    NATIVES_REGISTERED
        .get_or_init(|| {
            let java = match jvm() {
                Some(java) => java,
                None => return Err("no JVM available".to_string()),
            };
            let result: JavaResult<()> = (|| {
                let clazz = java.class("asyncnative.NativeLib")?;
                clazz.register_natives(&[
                    // fn-item form.
                    async_native!("compute", async_compute)?,
                    // closure form (same A would collide with compute, so it
                    // uses a distinct argument-tuple type).
                    async_native!("shout", |java: Java, (s,): (String,)| async move {
                        let _ = java;
                        Ok(s.to_uppercase())
                    })?,
                    async_native!("fail", async_fail)?,
                    async_native!("boom", async_boom)?,
                    async_native!("parked", async_parked)?,
                    async_native!("tokioSleep", async_tokio_sleep)?,
                ])
            })();
            result.map_err(|e| format!("{e:?}"))
        })
        .clone()
}

#[test]
fn async_native_full_circle() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING async native test: {reason}");
        return Ok(());
    }
    let clazz = java.class("asyncnative.NativeLib")?;

    // Call the native: Java sees a CompletableFuture<Integer> immediately.
    let cf: JObject = clazz.call_static("compute", (1_i32, 2_i32))?;
    // Await it through java_future: Rust → Java → Rust, the full circle. The
    // Rust worker thread runs the async fn and completes the future.
    let sum = block_on(java_future::<i32>(java, cf));
    assert_eq!(sum?, 3);
    Ok(())
}

#[test]
fn async_native_closure_form() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING async native test: {reason}");
        return Ok(());
    }
    let clazz = java.class("asyncnative.NativeLib")?;

    // `shout` is registered with a closure (`|java: Java, (s,): (String,)|
    // async move { … }`) — distinct A from compute, so no collision.
    let cf: JObject = clazz.call_static("shout", ("hi",))?;
    let s = block_on(java_future::<String>(java, cf));
    assert_eq!(s?, "HI");
    Ok(())
}

#[test]
fn async_native_java_side_chaining() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING async native test: {reason}");
        return Ok(());
    }
    let clazz = java.class("asyncnative.NativeLib")?;

    // The Java host method `chain` calls the native `compute` and attaches
    // thenApply — Java-side future semantics over the Rust-completed future.
    let cf: JObject = clazz.call_static("chain", (1_i32, 2_i32))?;
    let s = block_on(java_future::<String>(java, cf));
    assert_eq!(s?, "sum=3");
    Ok(())
}

#[test]
fn async_native_error_completes_exceptionally() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING async native test: {reason}");
        return Ok(());
    }
    let clazz = java.class("asyncnative.NativeLib")?;

    // Err(JavaException) → the worker materializes the exception and calls
    // completeExceptionally; java_future unwraps the ExecutionException cause
    // and reports the *original* exception, exactly like the java_future
    // exception test above.
    let cf: JObject = clazz.call_static("fail", ("nope", 1_i32))?;
    let result = block_on(java_future::<String>(java, cf));
    match result {
        Err(JavaError::JavaException { class, message }) => {
            assert_eq!(class, "java.lang.IllegalArgumentException");
            assert_eq!(message, "nope");
        }
        other => panic!("expected IllegalArgumentException, got {other:?}"),
    }
    Ok(())
}

#[test]
fn async_native_error_caught_on_the_java_side() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING async native test: {reason}");
        return Ok(());
    }
    let clazz = java.class("asyncnative.NativeLib")?;

    // The Java host method `handledFail` attaches an `exceptionally` handler
    // to the native `fail` — Java catches the exceptional completion.
    let cf: JObject = clazz.call_static("handledFail", ("nope", 1_i32))?;
    let s = block_on(java_future::<String>(java, cf));
    assert_eq!(s?, "caught:nope");
    Ok(())
}

#[test]
fn async_native_panic_completes_exceptionally() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING async native test: {reason}");
        return Ok(());
    }
    let clazz = java.class("asyncnative.NativeLib")?;

    // A panic inside the async fn is caught by the worker and surfaces as a
    // RuntimeException, mirroring the sync native-method panic rule.
    let cf: JObject = clazz.call_static("boom", ())?;
    let result = block_on(java_future::<String>(java, cf));
    match result {
        Err(JavaError::JavaException { class, message }) => {
            assert_eq!(class, "java.lang.RuntimeException");
            assert!(message.contains("panicked"), "message: {message}");
        }
        other => panic!("expected RuntimeException, got {other:?}"),
    }
    Ok(())
}

#[test]
fn async_native_collision_detected() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING async native test: {reason}");
        return Ok(());
    }
    let clazz = java.class("asyncnative.NativeLib")?;

    // Two *different* async fns with the same argument-tuple type (bool, i32)
    // map to the same shared trampoline; registering both must be rejected
    // before any JVM mutation (the fixture does not even declare these
    // method names).
    let a = async_native!("collisionAsyncA", async_collision_a)?;
    let b = async_native!("collisionAsyncB", async_collision_b)?;
    let err = clazz.register_natives(&[a, b]).unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("signature") || text.contains("trampoline"),
        "message: {text}"
    );
    Ok(())
}

/// The std path drives a park-based future: the worker thread runs
/// `async_parked`, whose future parks the polling thread in `poll` and only
/// completes when the completing thread signals it (via the waker for the
/// executor's park and a direct `unpark` for the poll's own park — the
/// classic noop-waker breaker pattern; async-std internals do this). The
/// worker's executor must let the completion through — the whole point of
/// replacing the noop-waker spin with futures-lite's real waker. The 5s
/// wall-clock budget below turns any regression (the worker never being
/// woken, e.g. a reverted noop `Waker::noop()` spin) into a bounded timeout
/// failure instead of a hang.
#[test]
fn async_native_park_based_future_completes() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING async native test: {reason}");
        return Ok(());
    }
    let clazz = java.class("asyncnative.NativeLib")?;

    let cf: JObject = clazz.call_static("parked", (7_i32,))?;
    // Await the CompletableFuture on a helper thread with a wall-clock
    // budget, so a regression (the worker never being woken) fails with a
    // bounded timeout error rather than hanging the test run.
    let (tx, rx) = std::sync::mpsc::channel();
    let java2 = java.clone();
    let cf2 = cf.clone();
    std::thread::spawn(move || {
        let _ = tx.send(block_on(java_future::<String>(java2, cf2)));
    });
    let result = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the park-based async native must complete within 5s \
                 (regression: the worker's waker never drove completion)");
    assert_eq!(result?, "parked-done");
    Ok(())
}

/// The tokio path: a native whose future awaits `tokio::time::sleep` only
/// completes when the worker is spawned on a tokio runtime. The
/// dev-dependency enables the `tokio` feature in the test build, and the
/// `#[tokio::test]` harness runs this test inside a runtime, so the native
/// trampoline sees a current tokio handle and spawns the future on it — the
/// timer fires and the future completes. (Called outside any runtime, the
/// same native completes exceptionally — a tokio timer's first poll panics
/// without a runtime and the worker surfaces the panic; documented in the
/// `rjava::future` module docs.)
#[tokio::test]
async fn async_native_tokio_timer_inside_runtime() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING async native test: {reason}");
        return Ok(());
    }
    let clazz = java.class("asyncnative.NativeLib")?;

    // Called from inside the runtime: the trampoline spawns the worker on
    // it, so the tokio timer in `async_tokio_sleep` completes.
    let cf: JObject = clazz.call_static("tokioSleep", (10_i64,))?;
    let result = java_future::<String>(java, cf).await;
    assert_eq!(result?, "tokio-done");
    Ok(())
}
