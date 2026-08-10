#![forbid(unsafe_code)]
//! Integration tests for the async round: `rjava::rx` (callback → Rust
//! future, feature `interface`) and `Java::call_async` /
//! `Java::call_static_async` (method call → Rust future, tokio-aware when
//! the `tokio` feature is on).
//!
//! The `#![forbid(unsafe_code)]` mirrors tests/integration.rs: the bridges
//! and this harness are entirely safe code.
//!
//! The `rx` tests need a small Java fixture (`Listener` + `Host`, compiled
//! into `target/rjava-asyncrx-classes` by this binary, exactly once per
//! process); the `call_async` tests use only JDK classes.
//!
//! The std-only tests drive the futures with the tiny `block_on` below (a
//! no-op waker). When the `tokio` feature is enabled in the build (it is,
//! via the dev-dependency), `block_on` polls *outside* any tokio runtime,
//! which exercises the std-thread fallback; the `#[tokio::test]` cases
//! exercise the `spawn_blocking` path.
//!
//! If no JVM (or no `javac`) can be located, every test skips gracefully
//! with a reason.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use rjava::interface;
use rjava::prelude::*;
use rjava::rx;

// ---------------------------------------------------------------------------
// JVM + fixture bootstrap (mirrors tests/interface.rs)
// ---------------------------------------------------------------------------

/// Compile the `asyncrx` fixtures (`Listener`, `Host`) exactly once per
/// test process. `Err` carries the reason, reported once; when it fails
/// every test skips.
static FIXTURE_COMPILED: LazyLock<Result<(), String>> = LazyLock::new(compile_fixture);

fn compile_fixture() -> Result<(), String> {
    let out_dir = "target/rjava-asyncrx-classes";
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
        .arg("tests/java/asyncrx/Listener.java")
        .arg("tests/java/asyncrx/Host.java")
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

/// Attempt to build the JVM; returns `None` (after an `eprintln`) if no JVM
/// is available so tests can skip instead of failing on JVM-less machines.
fn jvm() -> Option<Java> {
    if let Err(reason) = &*FIXTURE_COMPILED {
        eprintln!("SKIPPING test: could not compile Java fixture: {reason}");
        return None;
    }
    match Java::builder()
        .class_path("target/rjava-asyncrx-classes")
        .build()
    {
        Ok(java) => Some(java),
        Err(e) => {
            eprintln!("SKIPPING test: no JVM available: {e}");
            None
        }
    }
}

/// Minimal `block_on` (no tokio dependency): poll `fut` in a loop with a
/// no-op waker, sleeping a little between polls, until it is `Ready`.
fn block_on<F: Future>(fut: F) -> F::Output {
    let mut fut = Box::pin(fut);
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(out) => return out,
            Poll::Pending => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

// ---------------------------------------------------------------------------
// rjava::rx — callback → future
// ---------------------------------------------------------------------------

/// The core shape: a listener built with `rx::from_callback`, registered on
/// a Java `Host`, and awaited — the value fired by a *Java* thread on
/// `onEvent` arrives through the Rust future.
#[test]
fn callback_becomes_future() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };

    let (listener, event) = rx::from_callback::<String, _>(&["Listener"], |tx| {
        Arc::new(move |env, call| {
            if call.name == "onEvent" {
                let value = String::from_java(
                    env,
                    call.args.into_iter().next().expect("onEvent takes 1 arg"),
                )?;
                let _ = tx.send(Ok(value));
            }
            Ok(interface::null())
        })
    })?;

    let host = java.class("Host")?;
    let host_obj = host.new_instance((&listener,))?;
    host_obj.call_void("fireEventAfter", (50_i64, "hello from Java"))?;

    let value = block_on(event)?;
    assert_eq!(value, "hello from Java");
    Ok(())
}

/// One-shot semantics: the first event completes the future; the
/// `onDone` event that follows is dropped (its `send` fails, which the
/// handler observes).
#[test]
fn first_event_wins() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };

    let later_dropped = Arc::new(AtomicBool::new(false));
    let later_dropped2 = later_dropped.clone();
    let (listener, event) = rx::from_callback::<String, _>(&["Listener"], move |tx| {
        let later_dropped = later_dropped2;
        Arc::new(move |env, call| {
            let mut args = call.args.into_iter();
            match call.name.as_str() {
                "onEvent" => {
                    let value =
                        String::from_java(env, args.next().expect("onEvent takes 1 arg"))?;
                    let _ = tx.send(Ok(value));
                }
                "onDone" => {
                    let n = i32::from_java(env, args.next().expect("onDone takes 1 arg"))?;
                    // The future already completed with onEvent; the channel
                    // is closed, so this send must fail.
                    if tx.send(Ok(n.to_string())).is_err() {
                        later_dropped.store(true, Ordering::SeqCst);
                    }
                }
                _ => {}
            }
            Ok(interface::null())
        })
    })?;

    let host = java.class("Host")?;
    let host_obj = host.new_instance((&listener,))?;
    host_obj.call_void("fireBothAfter", (50_i64, "first", 42_i32))?;

    let value = block_on(event)?;
    assert_eq!(value, "first");

    // The onDone event fires ~50ms after onEvent; give it time to arrive
    // and observe the failed send.
    for _ in 0..100 {
        if later_dropped.load(Ordering::SeqCst) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the onDone send after completion should have failed")
}

/// Cancel-by-drop: dropping the future before its first poll spawns
/// nothing (lazy start) and closes the channel, so the handler's later
/// `send` fails with `SendError` — observable by the handler.
#[test]
fn cancel_by_drop() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };

    let saw_send_error = Arc::new(AtomicBool::new(false));
    let saw_send_error2 = saw_send_error.clone();
    let (listener, event) = rx::from_callback::<String, _>(&["Listener"], move |tx| {
        let saw = saw_send_error2;
        Arc::new(move |env, call| {
            if call.name == "onEvent" {
                let value = String::from_java(
                    env,
                    call.args.into_iter().next().expect("onEvent takes 1 arg"),
                )?;
                if tx.send(Ok(value)).is_err() {
                    saw.store(true, Ordering::SeqCst);
                }
            }
            Ok(interface::null())
        })
    })?;

    // Drop the future before any poll: no thread is spawned, the receiver
    // is dropped, the channel closes.
    drop(event);

    let host = java.class("Host")?;
    let host_obj = host.new_instance((&listener,))?;
    host_obj.call_void("fireEventAfter", (50_i64, "late"))?;

    for _ in 0..100 {
        if saw_send_error.load(Ordering::SeqCst) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the send into a dropped future should have failed")
}

/// A handler that never captures the sender closes the channel
/// immediately; the future resolves with a clear error instead of hanging
/// forever.
#[test]
fn channel_closed_resolves_with_error() -> JavaResult<()> {
    let Some(_java) = jvm() else { return Ok(()) };

    // `_tx` is not captured by the handler, so the channel has no sender
    // the moment `f` returns: the worker's `recv` fails at once.
    let (_listener, event) = rx::from_callback::<String, _>(&["Listener"], |_tx| {
        Arc::new(|_env, call| {
            let _ = call;
            Ok(interface::null())
        })
    })?;

    let err = block_on(event).expect_err("a sender-less channel must resolve with an error");
    match err {
        JavaError::InvalidArgument(msg) => {
            assert!(
                msg.contains("dropped without ever firing"),
                "unexpected message: {msg}"
            );
        }
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Java::call_async / Java::call_static_async — std thread (no runtime)
// ---------------------------------------------------------------------------

/// The `call_async` round trip on a StringBuilder, driven by the no-op
/// `block_on` — which runs *outside* any tokio runtime, so the worker
/// thread is a plain std thread even in tokio-enabled builds (the
/// documented fallback).
#[test]
fn call_async_round_trip() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };

    let sb: JObject = java.new_object("java.lang.StringBuilder", ("Hello",))?;

    // append returns the receiver (a StringBuilder object).
    let returned: JObject = block_on(java.call_async::<_, JObject>(&sb, "append", (" world",)))?;
    let len: i32 = block_on(java.call_async(&sb, "length", ()))?;
    let s: String = block_on(java.call_async(&sb, "toString", ()))?;
    assert_eq!(len, 11);
    assert_eq!(s, "Hello world");

    // The returned handle is the same object.
    let same_len: i32 = block_on(java.call_async(&returned, "length", ()))?;
    assert_eq!(same_len, 11);
    Ok(())
}

/// Error propagation: a Java exception from the async call surfaces as
/// `JavaError::JavaException` through the future, exactly like the sync
/// path.
#[test]
fn call_async_error_propagates() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };

    let integer = java.class("java.lang.Integer")?;
    let err = block_on(java.call_static_async::<_, i32>(&integer, "parseInt", ("nope",)))
        .expect_err("parseInt(\"nope\") must throw");
    match err {
        JavaError::WithContext { operation, source } => {
            assert_eq!(
                operation, "calling parseInt(String) on java.lang.Integer",
                "the async path must name the failed call too"
            );
            match *source {
                JavaError::JavaException { class, .. } => {
                    assert_eq!(class, "java.lang.NumberFormatException");
                }
                other => panic!("expected a JavaException source, got {other:?}"),
            }
        }
        other => panic!("expected WithContext, got {other:?}"),
    }
    Ok(())
}

/// `call_static_async` round trip on `Math.max`.
#[test]
fn call_static_async_round_trip() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };

    let math = java.class("java.lang.Math")?;
    let max: i32 = block_on(java.call_static_async(&math, "max", (3_i32, 7_i32)))?;
    assert_eq!(max, 7);
    Ok(())
}

// ---------------------------------------------------------------------------
// tokio runtime — the spawn_blocking path
// ---------------------------------------------------------------------------

/// Under a real tokio runtime the `call_async` worker is dispatched through
/// `spawn_blocking`; the future still completes with the call's result.
#[tokio::test]
async fn call_async_under_tokio_runtime() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };

    let sb: JObject = java.new_object("java.lang.StringBuilder", ())?;
    let _appended: JObject = java.call_async(&sb, "append", ("tokio",)).await?;
    let s: String = java.call_async(&sb, "toString", ()).await?;
    assert_eq!(s, "tokio");
    Ok(())
}

/// `rjava::rx` works under a tokio runtime too (the callback bridge spawns
/// a std thread regardless — it never touches the JVM).
#[tokio::test]
async fn rx_under_tokio_runtime() -> JavaResult<()> {
    let Some(java) = jvm() else { return Ok(()) };

    let (listener, event) = rx::from_callback::<i32, _>(&["Listener"], |tx| {
        Arc::new(move |env, call| {
            if call.name == "onDone" {
                let n = i32::from_java(
                    env,
                    call.args.into_iter().next().expect("onDone takes 1 arg"),
                )?;
                let _ = tx.send(Ok(n));
            }
            Ok(interface::null())
        })
    })?;

    let host = java.class("Host")?;
    let host_obj = host.new_instance((&listener,))?;
    host_obj.call_void("fireDoneAfter", (50_i64, 42_i32))?;

    let n = event.await?;
    assert_eq!(n, 42);
    Ok(())
}
