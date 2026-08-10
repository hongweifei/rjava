//! # Async demo — three ways to await Java from Rust
//!
//! One coherent demo of the async round, all under `#[tokio::main]`:
//!
//! 1. **`rjava::future::java_future`** — a `CompletableFuture` completed by
//!    Java's own executor (`supplyAsync` over a Rust-implemented
//!    `java.util.function.Supplier` proxy), awaited from Rust;
//! 2. **`Java::call_async`** — a StringBuilder round trip; inside the tokio
//!    runtime the JVM calls run on the runtime's blocking pool
//!    (`spawn_blocking`, feature `tokio`);
//! 3. **`rjava::rx::from_callback`** — a Java listener (`Listener`) fired
//!    by a background Java thread, bridged to a Rust future and awaited
//!    (`feature interface`);
//! 4. **`rjava::async_native!`** — a Rust **async** function registered as
//!    a Java native returning a `CompletableFuture`, awaited back through
//!    `java_future`: the full circle, Rust → Java → Rust.
//!
//! The small Java fixture (`Listener` + `Host`) is embedded below, compiled
//! with `javac` (via `JAVA_HOME/bin/javac`, like the integration tests)
//! into `target/async-demo-classes`, and the JVM is started with that
//! directory on the class path.
//!
//! Run with: `cargo run --example async_demo --features "tokio interface"`

use std::sync::Arc;

use rjava::async_native;
use rjava::future::java_future;
use rjava::interface::{self, Call};
use rjava::jni::JValueOwned;
use rjava::prelude::*;
use rjava::rx;

/// The Rust **async** function registered as the Java native
/// `AsyncNativeDemo.compute` (an `async_native!`): Java callers see a
/// `CompletableFuture<Integer>`, and awaiting that future back through
/// `java_future` is the full circle — Rust → Java → Rust.
async fn compute(_java: Java, (a, b): (i32, i32)) -> JavaResult<i32> {
    Ok(a + b)
}

// ---------------------------------------------------------------------------
// The Java fixture, embedded so the example is one self-contained file
// ---------------------------------------------------------------------------

/// The Java fixture, embedded so the example is one self-contained file.
///
/// `Host` must be `public` with a `public` constructor: rjava's constructor
/// resolution fallback enumerates `Class.getConstructors()`, which returns
/// **public** constructors only — a package-private constructor cannot be
/// resolved via reflection. Each public type lives in its own file (the JLS
/// requires it), so the fixture is three embedded sources compiled by one
/// `javac` invocation.
///
/// `Listener` — the interface `rjava::rx` implements from Rust.
const LISTENER_JAVA: &str = r#"
public interface Listener {
    void onEvent(String value);
    void onDone(int n);
}
"#;

/// `Host` — fires the listener's events from a background Java thread after
/// a delay, the "event arrives from Java, Rust awaits" shape.
const HOST_JAVA: &str = r#"
public class Host {
    private final Listener listener;

    public Host(Listener listener) {
        this.listener = listener;
    }

    public void fireAfter(long delayMs, String value, int n) {
        Thread t = new Thread(() -> {
            try {
                Thread.sleep(delayMs);
                listener.onEvent(value);
                Thread.sleep(delayMs);
                listener.onDone(n);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
        });
        t.setDaemon(true);
        t.start();
    }
}
"#;

/// `AsyncNativeDemo` — declares the native method the `async_native!`
/// section registers and calls.
const ASYNC_NATIVE_JAVA: &str = r#"
import java.util.concurrent.CompletableFuture;

public class AsyncNativeDemo {
    public static native CompletableFuture<Integer> compute(int a, int b);
}
"#;

// ---------------------------------------------------------------------------
// javac helper (JAVA_HOME/bin/javac(.exe), like the integration tests)
// ---------------------------------------------------------------------------

/// `javac` from `JAVA_HOME/bin` (falling back to `PATH`).
fn tool(name: &str) -> String {
    match std::env::var("JAVA_HOME") {
        Ok(home) => format!(
            "{home}/bin/{name}{}",
            if cfg!(windows) { ".exe" } else { "" }
        ),
        Err(_) => name.to_string(),
    }
}

/// Run one tool with `args`, returning a readable error with stderr on
/// failure.
fn run_tool(program: &str, args: &[&str]) -> Result<(), String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("could not run `{program}`: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "`{program} {}` failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    // ------------------------------------------------------------------
    // 0) compile the embedded fixture and start the JVM
    // ------------------------------------------------------------------
    let src_dir = "target/async-demo-src";
    let out_dir = "target/async-demo-classes";
    std::fs::create_dir_all(src_dir)
        .map_err(|e| format!("cannot create {src_dir}: {e}"))?;
    std::fs::create_dir_all(out_dir)
        .map_err(|e| format!("cannot create {out_dir}: {e}"))?;
    let listener_src = format!("{src_dir}/Listener.java");
    let host_src = format!("{src_dir}/Host.java");
    let async_native_src = format!("{src_dir}/AsyncNativeDemo.java");
    std::fs::write(&listener_src, LISTENER_JAVA)
        .map_err(|e| format!("cannot write {listener_src}: {e}"))?;
    std::fs::write(&host_src, HOST_JAVA)
        .map_err(|e| format!("cannot write {host_src}: {e}"))?;
    std::fs::write(&async_native_src, ASYNC_NATIVE_JAVA)
        .map_err(|e| format!("cannot write {async_native_src}: {e}"))?;
    run_tool(
        &tool("javac"),
        &[
            "-d",
            out_dir,
            &listener_src,
            &host_src,
            &async_native_src,
        ],
    )?;

    let java = Java::builder()
        .class_path(out_dir)
        .build()
        .map_err(|e| format!("cannot start the JVM: {e}"))?;

    demo(&java).await.map_err(|e| e.to_string())?;
    Ok(())
}

/// The three async paths, each returning a [`JavaResult`] so the `?`
/// operator reads naturally.
async fn demo(java: &Java) -> JavaResult<()> {
    // ------------------------------------------------------------------
    // 1) java_future: a CompletableFuture completed by Java's executor
    //    from a Rust-implemented `Supplier` (interface::proxy) — Java
    //    runs our supplier on the ForkJoinPool, then java_future awaits
    //    the CompletableFuture it produced.
    // ------------------------------------------------------------------
    let supplier = interface::proxy(
        Arc::new(|env, call: Call| {
            if call.name == "get" {
                let out =
                    JValueOwned::Object(env.new_string("hello from a Rust Supplier")?.into());
                return interface::box_value(env, out);
            }
            Err(JavaError::InvalidArgument("unexpected method"))
        }),
        &["java.util.function.Supplier"],
    )?;
    let cf: JObject = java
        .class("java.util.concurrent.CompletableFuture")?
        .call_static("supplyAsync", (&supplier,))?;
    let from_cf: String = java_future(java.clone(), cf).await?;
    println!("1. java_future  (CompletableFuture.supplyAsync): {from_cf}");

    // ------------------------------------------------------------------
    // 2) call_async: StringBuilder round trip (spawn_blocking under tokio)
    // ------------------------------------------------------------------
    let sb: JObject = java.new_object("java.lang.StringBuilder", ())?;
    java.call_async::<_, JObject>(&sb, "append", ("async ",)).await?;
    java.call_async::<_, JObject>(&sb, "append", ("method calls",)).await?;
    let s: String = java.call_async(&sb, "toString", ()).await?;
    let len: i32 = java.call_async(&sb, "length", ()).await?;
    println!("2. call_async   (StringBuilder): {s:?} (len {len})");

    // ------------------------------------------------------------------
    // 3) rx::from_callback: a Java listener fired from a Java thread
    // ------------------------------------------------------------------
    let (listener, event) = rx::from_callback::<String, _>(&["Listener"], |tx| {
        Arc::new(move |env, call: Call| {
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
    let host: JObject = java.new_object("Host", (&listener,))?;
    host.call_void("fireAfter", (100_i64, "event from a Java thread", 7_i32))?;
    let event_value: String = event.await?;
    println!("3. rx::from_callback (Listener.onEvent): {event_value}");

    // ------------------------------------------------------------------
    // 4) async_native!: a Rust async fn as a Java native returning a
    //    CompletableFuture — the full circle, awaited back via java_future
    // ------------------------------------------------------------------
    let clazz = java.class("AsyncNativeDemo")?;
    clazz.register_natives(&[async_native!("compute", compute)?])?;
    let cf: JObject = clazz.call_static("compute", (20_i32, 22_i32))?;
    let sum: i32 = java_future(java.clone(), cf).await?;
    println!("4. async_native (Rust async fn → CompletableFuture → java_future): {sum}");

    println!("\nall four async paths complete");
    Ok(())
}
