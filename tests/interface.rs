#![forbid(unsafe_code)]
//! Integration tests for the `interface` feature (`rjava::interface`):
//! implementing Java interfaces from Rust with zero user-written Java
//! implementation classes, via `java.lang.reflect.Proxy` plus the one fixed
//! precompiled shell class.
//!
//! The `#![forbid(unsafe_code)]` on the first line is intentional: it proves
//! the whole feature — handler dispatch, proxy creation, shell bootstrap —
//! is user-side `unsafe`-free.
//!
//! Each test binary is its own process with its own JVM, so nothing here can
//! collide with another binary's state.
//!
//! If no JVM (or no `javac`) can be located at test time, every test skips
//! gracefully with an `eprintln` reason (the `jni` crate's java-locator uses
//! `JAVA_HOME`, then `java` on `PATH`, then the Windows registry).

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, LazyLock};

use rjava::interface;
use rjava::interface::Call;
use rjava::jni::JValueOwned;
use rjava::prelude::*;

// ---------------------------------------------------------------------------
// JVM bootstrap (mirrors tests/bind.rs and tests/constructor_bind.rs)
// ---------------------------------------------------------------------------

/// Compile the `interface` fixtures (`Callback`, `Host`) exactly once per
/// test process. `Err` carries the reason, reported once; when it fails
/// every test skips.
static FIXTURE_COMPILED: LazyLock<Result<(), String>> = LazyLock::new(compile_fixture);

fn compile_fixture() -> Result<(), String> {
    let out_dir = "target/rjava-interface-classes";
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
        .arg("tests/java/interface/Callback.java")
        .arg("tests/java/interface/Host.java")
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
        .class_path("target/rjava-interface-classes")
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
// Tests
// ---------------------------------------------------------------------------

/// The fixture interface (`Callback`) implemented by a Rust closure that
/// dispatches on the method name (and parameter types): `greet`
/// (String → String), `add` (int, int → int, whose result the proxy
/// unboxes from the returned Integer) and `ping` (void). Java-side `Host`
/// calls all three — proving the proxy is an ordinary Java object.
#[test]
fn fixture_interface_round_trip() {
    let Some(java) = jvm() else { return };

    let proxy = interface::proxy(
        Arc::new(|env, call| {
            let mut args = call.args.into_iter();
            match call.name.as_str() {
                "greet" => {
                    let who = String::from_java(env, args.next().expect("greet takes 1 arg"))?;
                    let out = JValueOwned::Object(env.new_string(format!("Hello, {who}"))?.into());
                    Ok(interface::box_value(env, out)?)
                }
                "add" => {
                    let a = i32::from_java(env, args.next().expect("add takes 2 args"))?;
                    let b = i32::from_java(env, args.next().expect("add takes 2 args"))?;
                    Ok(interface::box_value(env, JValueOwned::Int(a + b))?)
                }
                "ping" => Ok(interface::null()),
                _ => Err(JavaError::InvalidArgument("Callback has no such method")),
            }
        }),
        &["Callback"],
    )
    .expect("proxy creation must succeed");
    let host = java.class("Host").expect("Host fixture class");

    let s: String = host
        .call_static("greet", (&proxy, "world"))
        .expect("Host.greet(Callback, String)");
    assert_eq!(s, "Hello, world");

    let n: i32 = host
        .call_static("add", (&proxy, 20, 22))
        .expect("Host.add(Callback, int, int)");
    assert_eq!(n, 42);

    host.call_static::<_, ()>("ping", (&proxy,))
        .expect("Host.ping(Callback)");
}

/// The closure's captured state lives in the userdata registry and persists
/// across calls: a shared counter that each `add` call bumps and returns.
#[test]
fn closure_state_captured() {
    let Some(java) = jvm() else { return };

    let calls = Arc::new(AtomicI64::new(0));
    let counter = Arc::clone(&calls);
    let proxy = interface::proxy(
        Arc::new(move |env, call| {
            if call.name == "add" {
                // Consume (and ignore) the two int arguments.
                let _ = call.args.len();
                counter.fetch_add(1, Ordering::SeqCst);
            }
            // The counter is i64; `add` returns `int` — the cast truncates,
            // which is exact for the test's small values.
            interface::box_value(
                env,
                JValueOwned::Int(counter.load(Ordering::SeqCst) as i32),
            )
        }),
        &["Callback"],
    )
    .expect("proxy creation must succeed");
    let host = java.class("Host").expect("Host fixture class");

    for expected in [1_i32, 2, 3] {
        let n: i32 = host
            .call_static("add", (&proxy, 1, 2))
            .expect("Host.add(Callback, int, int)");
        assert_eq!(n, expected, "the closure state must persist across calls");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

/// A JDK interface with no fixture: `java.lang.Runnable` (`void run()`).
/// Bootstrap-loaded interfaces have no classloader, which exercises the
/// shell-loader fallback for the proxy's defining classloader.
#[test]
fn runnable_via_jdk_proxy() {
    let Some(_java) = jvm() else { return };

    let proxy = interface::proxy(
        Arc::new(|_env, call| match call.name.as_str() {
            "run" => Ok(interface::null()),
            _ => Err(JavaError::InvalidArgument("Runnable has only run()")),
        }),
        &["java.lang.Runnable"],
    )
    .expect("proxy creation must succeed");
    proxy
        .call_void("run", ())
        .expect("run() must dispatch to the Rust handler");
}

/// A JDK generic interface: `java.util.function.Function<String, String>`
/// (`apply`). The proxy's erased `apply(Object)` resolves through the
/// existing reflection fallback.
#[test]
fn function_via_jdk_proxy() {
    let Some(_java) = jvm() else { return };

    let proxy = interface::proxy(
        Arc::new(|env, call| {
            let mut args = call.args.into_iter();
            match call.name.as_str() {
                "apply" => {
                    let s = String::from_java(env, args.next().expect("apply takes 1 arg"))?;
                    let out = JValueOwned::Object(env.new_string(format!("{s}!"))?.into());
                    Ok(interface::box_value(env, out)?)
                }
                _ => Err(JavaError::InvalidArgument("Function has only apply()")),
            }
        }),
        &["java.util.function.Function"],
    )
    .expect("proxy creation must succeed");
    let out: String = proxy
        .call("apply", ("hello",))
        .expect("apply(String) must dispatch to the Rust handler");
    assert_eq!(out, "hello!");
}

/// Exception propagation: a handler `Err` becomes a Java exception that both
/// Rust callers and Java code see (Host.tryGreet catches it and reports the
/// message).
#[test]
fn exception_propagation() {
    let Some(java) = jvm() else { return };

    let proxy = interface::proxy(
        Arc::new(|_env, call| match call.name.as_str() {
            "greet" => Err(JavaError::JavaException {
                class: "java.lang.IllegalStateException".to_string(),
                message: "rust says no".to_string(),
            }),
            _ => Ok(interface::null()),
        }),
        &["Callback"],
    )
    .expect("proxy creation must succeed");

    // Rust side: the exception crosses back through JNI as JavaException;
    // the failure names the proxy call.
    let err = proxy
        .call::<_, String>("greet", ("x",))
        .expect_err("greet must throw the handler's exception");
    match &err {
        JavaError::WithContext { operation, source } => {
            assert!(
                operation.starts_with("calling greet(String) on "),
                "operation must name the failed call: {operation}"
            );
            match &**source {
                JavaError::JavaException { class, message } => {
                    assert_eq!(class, "java.lang.IllegalStateException");
                    assert_eq!(message, "rust says no");
                }
                other => panic!("expected a JavaException source, got {other:?}"),
            }
        }
        other => panic!("expected WithContext, got {other:?}"),
    }

    // Java side: Host catches it, proving the JVM really saw the throw.
    let host = java.class("Host").expect("Host fixture class");
    let marker: String = host
        .call_static("tryGreet", (&proxy, "x"))
        .expect("tryGreet must catch the handler exception");
    assert_eq!(marker, "caught:rust says no");
}

/// One handler serving two interfaces on one proxy: `Runnable.run()` and
/// `Function.apply()` dispatch from the same closure.
#[test]
fn one_handler_two_interfaces() {
    let Some(_java) = jvm() else { return };

    let proxy = interface::proxy(
        Arc::new(|env, call| {
            let mut args = call.args.into_iter();
            match call.name.as_str() {
                "run" => Ok(interface::null()),
                "apply" => {
                    let s = String::from_java(env, args.next().expect("apply takes 1 arg"))?;
                    let out = JValueOwned::Object(env.new_string(s.to_uppercase())?.into());
                    Ok(interface::box_value(env, out)?)
                }
                _ => Err(JavaError::InvalidArgument("unexpected method")),
            }
        }),
        &["java.lang.Runnable", "java.util.function.Function"],
    )
    .expect("proxy creation must succeed");
    proxy
        .call_void("run", ())
        .expect("run() must dispatch to the handler");
    let out: String = proxy
        .call("apply", ("hi",))
        .expect("apply(String) must dispatch to the handler");
    assert_eq!(out, "HI");
}

/// The temp-dir bootstrap (writing the embedded shell `.class`, loading it
/// through a `URLClassLoader`, registering its natives) runs exactly once
/// per process — a second proxy creation reuses the cached class.
#[test]
fn shell_loaded_once() {
    let Some(_java) = jvm() else { return };

    let p1 = interface::proxy(
        Arc::new(|_env, _call| Ok(interface::null())),
        &["Callback"],
    )
    .expect("first proxy creation must succeed");
    let p2 = interface::proxy(
        Arc::new(|_env, _call| Ok(interface::null())),
        &["java.lang.Runnable"],
    )
    .expect("second proxy creation must succeed");

    let _ = (p1, p2);
    assert_eq!(
        interface::bootstrap_count(),
        1,
        "the temp-dir class load + native registration must run exactly once per process"
    );
}

/// A `default` method is auto-forwarded to its Java default implementation:
/// the handler is not consulted for `shout`, and the default body's result
/// (`"RUST"`) arrives. (The handler errors on any method it does not
/// recognize, so reaching it would fail the test.)
#[test]
fn default_method_auto_forwarded() {
    let Some(java) = jvm() else { return };

    let proxy = interface::proxy(
        Arc::new(|_env, _call| {
            Err::<rjava::JObject, JavaError>(JavaError::InvalidArgument(
                "the handler must never see the default method",
            ))
        }),
        &["Callback"],
    )
    .expect("proxy creation must succeed");
    let host = java.class("Host").expect("Host fixture class");

    let s: String = host
        .call_static("shout", (&proxy,))
        .expect("Host.shout(Callback)");
    assert_eq!(
        s, "RUST",
        "the interface's default implementation must run, not the handler"
    );
}

/// Overloads are distinguished via `call.param_types`: one closure handles
/// both `add(int, int)` and `add(long, long)` by matching the declared
/// parameter types, and the `long` result (far beyond `i32` range) proves
/// the right overload ran.
#[test]
fn overloads_distinguished_by_param_types() {
    let Some(java) = jvm() else { return };

    let proxy = interface::proxy(
        Arc::new(|env, call| {
            let mut args = call.args.into_iter();
            match call.name.as_str() {
                "add" if call.param_types == ["int", "int"] => {
                    let a = i32::from_java(env, args.next().expect("add(int,int) takes 2 args"))?;
                    let b = i32::from_java(env, args.next().expect("add(int,int) takes 2 args"))?;
                    Ok(interface::box_value(env, JValueOwned::Int(a + b))?)
                }
                "add" if call.param_types == ["long", "long"] => {
                    let a = i64::from_java(env, args.next().expect("add(long,long) takes 2 args"))?;
                    let b = i64::from_java(env, args.next().expect("add(long,long) takes 2 args"))?;
                    Ok(interface::box_value(env, JValueOwned::Long(a + b))?)
                }
                _ => Err(JavaError::InvalidArgument("Callback has no such method")),
            }
        }),
        &["Callback"],
    )
    .expect("proxy creation must succeed");
    let host = java.class("Host").expect("Host fixture class");

    let n: i32 = host
        .call_static("add", (&proxy, 20, 22))
        .expect("Host.add(Callback, int, int)");
    assert_eq!(n, 42);

    let big: i64 = 1_i64 << 40;
    let l: i64 = host
        .call_static("addLong", (&proxy, big, 7_i64))
        .expect("Host.addLong(Callback, long, long)");
    assert_eq!(l, big + 7, "the long overload must produce the long sum");
}

/// `proxy.toString()` (from Java) is intercepted by the library with the
/// standard `Object` format — `ClassName@hexhash` — built in Rust: the
/// string is stable, the class part is the proxy's real class name, and the
/// hex part is `System.identityHashCode(proxy)`.
#[test]
fn object_to_string_intercepted() {
    let Some(java) = jvm() else { return };

    let proxy = interface::proxy(
        Arc::new(|_env, _call| Err(JavaError::InvalidArgument("no methods expected"))),
        &["Callback"],
    )
    .expect("proxy creation must succeed");
    let host = java.class("Host").expect("Host fixture class");

    let s: String = host
        .call_static("toStringOf", (&proxy,))
        .expect("Host.toStringOf(Callback)");
    let s2: String = host
        .call_static("toStringOf", (&proxy,))
        .expect("Host.toStringOf(Callback)");
    assert_eq!(s, s2, "toString must be stable for one object");

    let (class_part, hex_part) = s
        .split_once('@')
        .unwrap_or_else(|| panic!("Object toString must be `ClassName@hex`, got: {s}"));
    assert_eq!(
        class_part,
        proxy.class().expect("proxy runtime class").name().expect("class name"),
        "the class part must be the proxy's actual class name"
    );
    let hex = u32::from_str_radix(hex_part, 16)
        .unwrap_or_else(|_| panic!("the hex part must parse as a u32, got: {hex_part}"));
    let system = java.class("java.lang.System").expect("System class");
    let id: i32 = system
        .call_static("identityHashCode", (&proxy,))
        .expect("System.identityHashCode(proxy)");
    assert_eq!(hex, id as u32, "the hex part must be the identity hash");
}

/// The reserved `Object` methods never reach the handler, so a proxy whose
/// handler rejects every call is enough to exercise them.
fn reject_all(_env: &mut jni::Env<'_>, _call: Call<'_>) -> JavaResult<rjava::JObject> {
    Err(JavaError::InvalidArgument("no methods expected"))
}

/// `hashCode()` and `equals(Object)` are intercepted with identity
/// semantics: `hashCode` is stable and equals `System.identityHashCode`,
/// `equals(proxy)` is `true` and `equals(other proxy)` is `false`.
#[test]
fn hash_code_and_equals_intercepted() {
    let Some(java) = jvm() else { return };

    let proxy = interface::proxy(Arc::new(reject_all), &["Callback"])
        .expect("proxy creation must succeed");
    let other = interface::proxy(Arc::new(reject_all), &["Callback"])
        .expect("second proxy creation must succeed");
    let host = java.class("Host").expect("Host fixture class");

    let h: i32 = host
        .call_static("hashCodeOf", (&proxy,))
        .expect("Host.hashCodeOf(Callback)");
    let h2: i32 = host
        .call_static("hashCodeOf", (&proxy,))
        .expect("Host.hashCodeOf(Callback)");
    assert_eq!(h, h2, "the identity hash must be stable for one object");
    let system = java.class("java.lang.System").expect("System class");
    let id: i32 = system
        .call_static("identityHashCode", (&proxy,))
        .expect("System.identityHashCode(proxy)");
    assert_eq!(h, id, "hashCode must be the identity hash");

    let self_eq: bool = host
        .call_static("equalsSelf", (&proxy,))
        .expect("Host.equalsSelf(Callback)");
    assert!(self_eq, "proxy.equals(proxy) must be true");
    let other_eq: bool = host
        .call_static("equalsOther", (&proxy, &other))
        .expect("Host.equalsOther(Callback, Object)");
    assert!(!other_eq, "proxy.equals(other) must be false");
}

/// A domain method named like an `Object` method but with a different
/// signature — `toString(int)` — still reaches the handler: the library's
/// interception matches the exact signature (name + parameter types), never
/// the name alone.
#[test]
fn domain_method_named_like_object_method_reaches_handler() {
    let Some(java) = jvm() else { return };

    let proxy = interface::proxy(
        Arc::new(|env, call| {
            let mut args = call.args.into_iter();
            match call.name.as_str() {
                "toString" if call.param_types == ["int"] => {
                    let n = i32::from_java(env, args.next().expect("toString(int) takes 1 arg"))?;
                    let out = JValueOwned::Object(env.new_string(format!("n={n}"))?.into());
                    Ok(interface::box_value(env, out)?)
                }
                _ => Err(JavaError::InvalidArgument("Callback has no such method")),
            }
        }),
        &["Callback"],
    )
    .expect("proxy creation must succeed");
    let host = java.class("Host").expect("Host fixture class");

    let s: String = host
        .call_static("toStringInt", (&proxy, 7))
        .expect("Host.toStringInt(Callback, int)");
    assert_eq!(s, "n=7", "the domain toString(int) must reach the handler");
}

// ---------------------------------------------------------------------------
// Typed `interface!` tests
// ---------------------------------------------------------------------------

// The typed mirror of the closure tests: the same fixture interface
// (`Callback`), declared once with Rust types. The generated trait requires
// the user to implement every declared method; `interface::proxy_typed`
// builds the generic handler behind the scenes. `add_long` and `to_string`
// exercise the `[java_name = "…"]` alias — the Java method is `add` (the
// `long` overload) / `toString` (the domain `toString(int)`); the trait
// method names are idiomatic Rust.
rjava::interface! {
    "Callback" => Callback {
        fn greet(name: String) -> String;
        fn add(a: i32, b: i32) -> i32;
        fn add_long(a: i64, b: i64) -> i64 [java_name = "add"];
        fn ping();
        fn apply(words: Vec<String>) -> Option<String>;
        fn to_string(n: i32) -> String [java_name = "toString"];
    }
}

// A second interface for the two-interfaces-one-state test; `run()` is void.
rjava::interface! {
    "java.lang.Runnable" => Runnable {
        fn run();
    }
}

/// The typed state: a captured `prefix` (observable through `greet`) and a
/// shared call counter (observable in Rust — the test holds the `Arc`).
struct GreeterState {
    prefix: String,
    calls: std::sync::Arc<AtomicI64>,
}

impl Callback for GreeterState {
    fn greet(&self, _env: &mut jni::Env, name: String) -> JavaResult<String> {
        Ok(format!("{}{name}", self.prefix))
    }
    fn add(&self, _env: &mut jni::Env, a: i32, b: i32) -> JavaResult<i32> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(a + b)
    }
    fn add_long(&self, _env: &mut jni::Env, a: i64, b: i64) -> JavaResult<i64> {
        Ok(a + b)
    }
    fn ping(&self, _env: &mut jni::Env) -> JavaResult<()> {
        Ok(())
    }
    fn apply(&self, _env: &mut jni::Env, words: Vec<String>) -> JavaResult<Option<String>> {
        Ok(if words.is_empty() {
            None
        } else {
            Some(words.join(","))
        })
    }
    fn to_string(&self, _env: &mut jni::Env, n: i32) -> JavaResult<String> {
        Ok(format!("n={n}"))
    }
}

impl Runnable for GreeterState {
    fn run(&self, _env: &mut jni::Env) -> JavaResult<()> {
        Ok(())
    }
}

fn greeter_state(calls: std::sync::Arc<AtomicI64>) -> GreeterState {
    GreeterState {
        prefix: "Hi, ".to_string(),
        calls,
    }
}

/// The typed round trip: `greet` (String → String), `add` (int, int → int),
/// `ping` (void) and `apply` (`Vec<String>` → `Option<String>`, including the
/// `null` → `None` leg) — through the same Java-side `Host` fixture as the
/// closure tests, proving the typed proxy is an ordinary Java object.
#[test]
fn typed_interface_round_trip() {
    let Some(java) = jvm() else { return };

    let proxy = interface::proxy_typed::<dyn Callback + Send + Sync>(Arc::new(greeter_state(Arc::new(AtomicI64::new(0)))), &["Callback"])
        .expect("typed proxy creation must succeed");
    let host = java.class("Host").expect("Host fixture class");

    let s: String = host
        .call_static("greet", (&proxy, "world"))
        .expect("Host.greet(Callback, String)");
    assert_eq!(s, "Hi, world", "the captured state must be reachable through the trait");

    let n: i32 = host
        .call_static("add", (&proxy, 20, 22))
        .expect("Host.add(Callback, int, int)");
    assert_eq!(n, 42);

    host.call_static::<_, ()>("ping", (&proxy,))
        .expect("Host.ping(Callback)");

    let joined: String = host
        .call_static("applyWords", (&proxy, vec!["a".to_string(), "b".to_string()]))
        .expect("Host.applyWords(Callback, String[])");
    assert_eq!(joined, "a,b");

    let none: Option<String> = host
        .call_static("applyWords", (&proxy, Vec::<String>::new()))
        .expect("Host.applyWords(Callback, String[])");
    assert!(none.is_none(), "a Java null return must map to None");
}

/// Overloads via the `[java_name]` alias: `add_long` declares Java `add` with
/// `(long, long)` parameters. The table has two `add` entries and dispatch
/// picks the one whose declared parameter types match the call — the `long`
/// sum (far beyond `i32` range) proves the right overload ran.
#[test]
fn typed_overloads_distinguished_by_param_types() {
    let Some(java) = jvm() else { return };

    let proxy = interface::proxy_typed::<dyn Callback + Send + Sync>(Arc::new(greeter_state(Arc::new(AtomicI64::new(0)))), &["Callback"])
        .expect("typed proxy creation must succeed");
    let host = java.class("Host").expect("Host fixture class");

    let n: i32 = host
        .call_static("add", (&proxy, 20, 22))
        .expect("Host.add(Callback, int, int)");
    assert_eq!(n, 42);

    let big: i64 = 1_i64 << 40;
    let l: i64 = host
        .call_static("addLong", (&proxy, big, 7_i64))
        .expect("Host.addLong(Callback, long, long)");
    assert_eq!(l, big + 7, "the long overload must produce the long sum");
}

/// The state is captured per proxy and shared across calls (the trait method
/// runs with `&state`); a second proxy gets an independent state.
#[test]
fn typed_state_captured_and_independent() {
    let Some(java) = jvm() else { return };
    let host = java.class("Host").expect("Host fixture class");

    let calls1 = Arc::new(AtomicI64::new(0));
    let proxy1 = interface::proxy_typed::<dyn Callback + Send + Sync>(Arc::new(greeter_state(Arc::clone(&calls1))), &["Callback"])
        .expect("first proxy must succeed");
    let calls2 = Arc::new(AtomicI64::new(0));
    let proxy2 = interface::proxy_typed::<dyn Callback + Send + Sync>(Arc::new(greeter_state(Arc::clone(&calls2))), &["Callback"])
        .expect("second proxy must succeed");

    for _ in 0..3 {
        let _: i32 = host
            .call_static("add", (&proxy1, 1, 2))
            .expect("Host.add(Callback, int, int)");
    }
    let _: i32 = host
        .call_static("add", (&proxy2, 1, 2))
        .expect("Host.add(Callback, int, int)");

    assert_eq!(calls1.load(Ordering::SeqCst), 3, "state must persist across calls on proxy1");
    assert_eq!(calls2.load(Ordering::SeqCst), 1, "proxy2 must have its own state");
}

/// A `default` method (`shout`) is NOT declared in the trait — the hardened
/// invoke auto-forwards it to the Java default implementation, so the
/// handler (and therefore the trait) is never consulted and `"RUST"` arrives.
#[test]
fn typed_default_method_auto_forwarded() {
    let Some(java) = jvm() else { return };

    let proxy = interface::proxy_typed::<dyn Callback + Send + Sync>(Arc::new(greeter_state(Arc::new(AtomicI64::new(0)))), &["Callback"])
        .expect("typed proxy creation must succeed");
    let host = java.class("Host").expect("Host fixture class");

    let s: String = host
        .call_static("shout", (&proxy,))
        .expect("Host.shout(Callback)");
    assert_eq!(s, "RUST", "the Java default implementation must run, not the trait");
}

/// The library-reserved `Object` methods (`toString()`, `hashCode()`,
/// `equals(Object)`) are intercepted with identity semantics before the
/// handler — they work even though the trait does not (and may not) declare
/// them; the macro rejects a declaration of these exact signatures.
#[test]
fn typed_object_methods_intercepted() {
    let Some(java) = jvm() else { return };

    let proxy = interface::proxy_typed::<dyn Callback + Send + Sync>(Arc::new(greeter_state(Arc::new(AtomicI64::new(0)))), &["Callback"])
        .expect("typed proxy creation must succeed");
    let host = java.class("Host").expect("Host fixture class");

    let s: String = host
        .call_static("toStringOf", (&proxy,))
        .expect("Host.toStringOf(Callback)");
    assert!(s.contains('@'), "the standard Object toString format");

    let h: i32 = host
        .call_static("hashCodeOf", (&proxy,))
        .expect("Host.hashCodeOf(Callback)");
    let h2: i32 = host
        .call_static("hashCodeOf", (&proxy,))
        .expect("Host.hashCodeOf(Callback)");
    assert_eq!(h, h2, "the identity hash must be stable");

    let self_eq: bool = host
        .call_static("equalsSelf", (&proxy,))
        .expect("Host.equalsSelf(Callback)");
    assert!(self_eq);
}

/// A call whose `(name, param_types)` matches no declared method — here
/// `mystery(String)` is a fixture method the trait deliberately does not
/// declare — becomes a clear error: a thrown `RuntimeException` (a
/// `JavaException` to Rust callers) naming the method and its parameter
/// types, exactly like the generic handler's unknown-method behavior.
#[test]
fn typed_unknown_method_errors() {
    let Some(java) = jvm() else { return };

    let proxy = interface::proxy_typed::<dyn Callback + Send + Sync>(Arc::new(greeter_state(Arc::new(AtomicI64::new(0)))), &["Callback"])
        .expect("typed proxy creation must succeed");
    let host = java.class("Host").expect("Host fixture class");

    let err = host
        .call_static::<_, String>("mystery", (&proxy, "x"))
        .expect_err("a method absent from the trait must fail");
    match &err {
        JavaError::WithContext { operation, source } => {
            assert!(
                operation.starts_with("calling mystery(") && operation.contains("on Host"),
                "operation must name the failed call: {operation}"
            );
            match &**source {
                JavaError::JavaException { class, message } => {
                    assert_eq!(
                        class, "java.lang.RuntimeException",
                        "the shell throws a RuntimeException"
                    );
                    assert!(
                        message.contains("mystery") && message.contains("java.lang.String"),
                        "the error must name the method and its parameter types, got: {message}"
                    );
                }
                other => panic!("expected a thrown RuntimeException source, got {other:?}"),
            }
        }
        other => panic!("expected WithContext, got {other:?}"),
    }
}

/// The declared interface must be among the proxy's interfaces; passing an
/// unrelated list fails proxy creation with a clear error instead of
/// producing a proxy that could never dispatch a declared method.
#[test]
fn typed_wrong_interface_list_errors() {
    let Some(_java) = jvm() else { return };

    let err = interface::proxy_typed::<dyn Callback + Send + Sync>(
        Arc::new(greeter_state(Arc::new(AtomicI64::new(0)))),
        &["java.lang.Runnable"],
    )
    .expect_err("the declared interface must be in the proxy's interface list");
    assert!(
        matches!(err, JavaError::InvalidArgument(_)),
        "expected InvalidArgument, got {err:?}"
    );
}

/// One state struct implementing two generated traits (`Callback` and
/// `Runnable`): two proxies, each built from the state, sharing the same
/// underlying counter.
#[test]
fn two_interfaces_one_state() {
    let Some(java) = jvm() else { return };
    let host = java.class("Host").expect("Host fixture class");

    let calls = Arc::new(AtomicI64::new(0));
    // Two proxies from the same underlying state (the counter Arc is
    // shared), one implementing Callback and one implementing Runnable.
    let callback_proxy = interface::proxy_typed::<dyn Callback + Send + Sync>(Arc::new(greeter_state(Arc::clone(&calls))), &["Callback"])
        .expect("the Callback proxy must succeed");
    let runnable_proxy = interface::proxy_typed::<dyn Runnable + Send + Sync>(Arc::new(greeter_state(Arc::clone(&calls))), &["java.lang.Runnable"])
        .expect("the Runnable proxy must succeed");

    runnable_proxy
        .call_void("run", ())
        .expect("run() must dispatch to the Runnable trait");
    let s: String = host
        .call_static("greet", (&callback_proxy, "world"))
        .expect("Host.greet(Callback, String)");
    assert_eq!(s, "Hi, world");

    // The two proxies share the underlying state: a call through the
    // Callback proxy bumps the counter both were built from.
    let _: i32 = host
        .call_static("add", (&callback_proxy, 1, 2))
        .expect("Host.add(Callback, int, int)");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "both proxies must share one state");
}

// ---------------------------------------------------------------------------
// Freshness guard: the committed shell .class must match a fresh javac
// --release 8 compile of the .java source
// ---------------------------------------------------------------------------

/// The committed shell class (`interface/java/rjava/shell/InvocationHandlerShell.class`)
/// is the source of truth for the bytes `rjava::interface` embeds, so the
/// `.java` source and the committed `.class` must never drift. This test
/// recompiles the source with `javac --release 8` (the exact command that
/// produced the committed class) and byte-compares the result: if a
/// developer edits the `.java` without recompiling, the test fails with a
/// message telling them to regenerate and commit the `.class`.
///
/// It needs `javac` (found via `$JAVA_HOME/bin/javac` or `PATH`, the same
/// locator the crate's build script used); when no `javac` is available it
/// prints a loud `SKIPPING` reason and passes — CI has a JDK, so the guard
/// actually runs there.
#[test]
fn committed_shell_class_is_fresh() {
    let Some(javac) = find_javac() else {
        eprintln!(
            "SKIPPING shell-class freshness check: no `javac` found (JAVA_HOME unset and \
             not on PATH); trusting the committed .class as-is."
        );
        return;
    };

    // Compile into a fresh temp dir so a stale artifact can never be
    // mistaken for the freshly compiled one.
    let out_dir = std::env::temp_dir().join(format!("rjava-shell-freshness-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("create freshness-check temp dir");

    let output = std::process::Command::new(&javac)
        .arg("--release")
        .arg("8")
        .arg("-d")
        .arg(&out_dir)
        .arg("interface/java/rjava/shell/InvocationHandlerShell.java")
        .output()
        .expect("run javac");
    assert!(
        output.status.success(),
        "`{javac} --release 8` failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let fresh = std::fs::read(out_dir.join("rjava/shell/InvocationHandlerShell.class"))
        .expect("the freshly compiled .class must exist");
    let committed = std::fs::read("interface/java/rjava/shell/InvocationHandlerShell.class")
        .expect("the committed .class must exist");
    let _ = std::fs::remove_dir_all(&out_dir);

    assert_eq!(
        fresh.len(),
        committed.len(),
        "the committed .class has a different length than a fresh `javac --release 8` compile"
    );
    assert!(
        fresh == committed,
        "interface/java/rjava/shell/InvocationHandlerShell.class is STALE: it does not match \
         a fresh `javac --release 8` compile of interface/java/rjava/shell/InvocationHandlerShell.java. \
         Recompile and commit the .class:\n  javac --release 8 -d <tmp> \
         interface/java/rjava/shell/InvocationHandlerShell.java\n  # then move \
         <tmp>/rjava/shell/InvocationHandlerShell.class into interface/java/rjava/shell/"
    );
}

/// Locate `javac`: `$JAVA_HOME/bin/javac(.exe)` if `JAVA_HOME` is set,
/// otherwise `javac(.exe)` on `PATH` — the same locator the crate's build
/// script used, minus the panics (a missing compiler just means the
/// freshness check skips).
fn find_javac() -> Option<String> {
    if let Ok(home) = std::env::var("JAVA_HOME") {
        let exe = format!("{home}/bin/javac{}", if cfg!(windows) { ".exe" } else { "" });
        if std::path::Path::new(&exe).exists() {
            return Some(exe);
        }
    }
    let javac = if cfg!(windows) { "javac.exe" } else { "javac" };
    if std::process::Command::new(javac).arg("-version").output().is_ok() {
        return Some(javac.to_string());
    }
    None
}
