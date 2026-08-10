#![forbid(unsafe_code)]
//! Integration tests for the `register_natives!` batch macro: the one-`?`
//! batch form over the existing `JClass::register_natives(&[…])` array
//! form. The batch accepts any mix of `native!` / `native_inst!` /
//! `async_native!` items (they all yield a `JavaResult<NativeMethod>`), an
//! optional trailing comma, and unwraps every item inside the generated
//! array — so the caller's single `?` handles the whole batch.
//!
//! The `#![forbid(unsafe_code)]` mirrors tests/integration.rs: the macro
//! expansion is user-side `unsafe`-free (the only `unsafe` in the feature
//! lives in the `rjava-helper` crate's `register_natives` helper).
//!
//! If no JVM (or no `javac`) can be located at test time, every test skips
//! gracefully with an `eprintln` reason (the `jni` crate's java-locator uses
//! `JAVA_HOME`, then `java` on `PATH`, then the Windows registry).

use std::sync::LazyLock;

use rjava::prelude::*;
use rjava::{async_native, native, native_inst, register_natives};

/// Compile the batch fixtures (`com.example.BatchLib`, `com.example.Counter`)
/// exactly once per test process. `Err` carries the reason, reported once;
/// when it fails every test skips.
static FIXTURE_COMPILED: LazyLock<Result<(), String>> = LazyLock::new(compile_fixture);

fn compile_fixture() -> Result<(), String> {
    // `javac -d` creates the output directory itself; create it first anyway
    // so a failure here is reported as a compile problem, not a mystery.
    let out_dir = "target/rjava-test-classes";
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
        .arg("tests/java/com/example/BatchLib.java")
        .arg("tests/java/com/example/Counter.java")
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

/// Build (or reuse) the singleton JVM, skipping with a reason when none is
/// available. The fixture is compiled first and put on the class path, so
/// whichever test creates the JVM first gets the batch fixtures on it.
fn jvm() -> Option<Java> {
    if let Err(reason) = &*FIXTURE_COMPILED {
        eprintln!("SKIPPING test: could not compile Java fixture: {reason}");
        return None;
    }
    match Java::builder()
        .class_path("target/rjava-test-classes")
        .build()
    {
        Ok(java) => Some(java),
        Err(e) => {
            eprintln!("SKIPPING test: no JVM available: {e}");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// BatchLib natives: one static, one instance, one async — all registered in
// a single batch in `batch_registers_mixed_kinds`.
// ---------------------------------------------------------------------------

// Java: `public static native int add(int a, int b);`
fn batch_add(_env: &mut jni::Env, (a, b): (i32, i32)) -> JavaResult<i32> {
    Ok(a + b)
}

// Java: `public native int times(int factor);` (instance: reads `base`)
fn batch_times(_env: &mut jni::Env, (this, factor): (JObject, i32)) -> JavaResult<i32> {
    let base: i32 = this.get_field("base")?;
    Ok(base * factor)
}

// Java: `public static native int negate(int x);`
fn batch_negate(_env: &mut jni::Env, (x,): (i32,)) -> JavaResult<i32> {
    Ok(-x)
}

// Java: `public static native CompletableFuture<Integer> compute(int a, int b);`
async fn batch_compute(_java: Java, (a, b): (i32, i32)) -> JavaResult<i32> {
    Ok(a + b)
}

// Two *different* fn items with the same Rust signature `(i64,) -> i64`
// share one trampoline, so `register_natives` must reject registering both.
// The fixture lacks these method names — the collision check fires before
// any JVM mutation. Used by `batch_collision_error_propagates`.
fn batch_plus_one(_env: &mut jni::Env, (x,): (i64,)) -> JavaResult<i64> {
    Ok(x + 1)
}

fn batch_plus_two(_env: &mut jni::Env, (x,): (i64,)) -> JavaResult<i64> {
    Ok(x + 2)
}

// ---------------------------------------------------------------------------
// Counter userdata natives (mirror tests/integration.rs)
// ---------------------------------------------------------------------------

/// The Rust-side state behind a `com.example.Counter` Java shell; a `Mutex`
/// makes concurrent increments deterministic.
#[derive(Debug)]
struct Counter {
    count: parking_lot::Mutex<i64>,
}

impl Counter {
    fn new() -> Self {
        Counter {
            count: parking_lot::Mutex::new(0),
        }
    }

    fn increment(&self, by: i64) -> i64 {
        let mut count = self.count.lock();
        *count += by;
        *count
    }

    fn value(&self) -> i64 {
        *self.count.lock()
    }
}

fn counter_create(env: &mut jni::Env, (): ()) -> JavaResult<JObject> {
    rjava::userdata::create_shell(env, "com.example.Counter", Counter::new())
}

fn counter_increment(env: &mut jni::Env, (this, by): (JObject, i64)) -> JavaResult<i64> {
    let counter = rjava::userdata::get::<Counter>(env, &this)?;
    Ok(counter.increment(by))
}

fn counter_value(env: &mut jni::Env, (this,): (JObject,)) -> JavaResult<i64> {
    let counter = rjava::userdata::get::<Counter>(env, &this)?;
    Ok(counter.value())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The headline proof: one batch call, one `?`, no array brackets, mixing
/// `native!` (static), `native_inst!` (instance) and `async_native!` — and
/// the registrations behave exactly like the array form's.
#[test]
fn batch_registers_mixed_kinds() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };
    let clazz = java.class("com.example.BatchLib")?;
    // Batch form of:
    //   clazz.register_natives(&[
    //       native!("add", batch_add)?,
    //       native_inst!("times", batch_times)?,
    //       async_native!("compute", batch_compute)?,
    //   ])?;
    // One `?` at the end — the item-level `?`s live inside the macro.
    register_natives!(
        clazz,
        native!("add", batch_add),
        native_inst!("times", batch_times),
        async_native!("compute", batch_compute),
    )?;
    // Static native through the batch.
    let sum: i32 = clazz.call_static("add", (2, 3))?;
    assert_eq!(sum, 5);
    // Instance native: `this` arrives as the first tuple element.
    let obj = clazz.new_instance(())?;
    obj.set_field("base", 10)?;
    let r: i32 = obj.call("times", (2,))?;
    assert_eq!(r, 20);
    // Async native: Java sees a CompletableFuture<Integer> completed by the
    // Rust worker thread; `get()` blocks briefly until it completes (the
    // result is a boxed Integer, read via intValue).
    let cf: JObject = clazz.call_static("compute", (20_i32, 22_i32))?;
    let value: JObject = cf.call("get", ())?;
    let n: i32 = value.call("intValue", ())?;
    assert_eq!(n, 42);
    Ok(())
}

/// The TODO's exact shape (`create` / `increment`) on the userdata fixture,
/// plus a trailing comma: the batch registers a static factory and two
/// instance methods, and the Rust-backed state behaves exactly as with the
/// array form.
#[test]
fn batch_trailing_comma_userdata() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };
    let clazz = java.class("com.example.Counter")?;
    register_natives!(
        clazz,
        native!("create", counter_create),
        native_inst!("increment", counter_increment),
        native_inst!("value", counter_value),
    )?;
    let counter: JObject = clazz.call_static("create", ())?;
    let n: i64 = counter.call("increment", (5_i64,))?;
    assert_eq!(n, 5);
    let n: i64 = counter.call("increment", (3_i64,))?;
    assert_eq!(n, 8);
    let n: i64 = counter.call("value", ())?;
    assert_eq!(n, 8);
    Ok(())
}

/// A one-item batch (the `+` repetition requires at least one method).
#[test]
fn batch_single_item() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };
    let clazz = java.class("com.example.BatchLib")?;
    register_natives!(clazz, native!("negate", batch_negate))?;
    let n: i32 = clazz.call_static("negate", (7_i32,))?;
    assert_eq!(n, -7);
    Ok(())
}

/// A duplicate-signature registration must surface the SAME collision error
/// as the array form — through the batch's single tail `?` (the item-level
/// `?`s live inside the macro).
#[test]
fn batch_collision_error_propagates() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };
    let clazz = java.class("com.example.BatchLib")?;
    // Two different fn items with the same Rust signature `(i64,) -> i64`
    // map to the *same* shared trampoline; registering both must be rejected
    // before any JVM mutation (the fixture lacks these method names).
    let err = register_natives!(
        clazz,
        native!("collisionA", batch_plus_one),
        native!("collisionB", batch_plus_two),
    )
    .expect_err("a shared-trampoline collision must be rejected");
    let text = err.to_string();
    assert!(
        text.contains("signature") || text.contains("trampoline"),
        "unexpected error: {text}"
    );
    Ok(())
}
