#![forbid(unsafe_code)]
//! Integration tests for the `bind!` macro: compile-time-typed class
//! bindings over the dynamic call machinery.
//!
//! The `#![forbid(unsafe_code)]` on the first line is intentional: it proves
//! that the `bind!` expansion is user-side `unsafe`-free (like the
//! `native!`/`native_inst!` expansions).
//!
//! Each test binary is its own process with its own JVM, so nothing here can
//! collide with another binary's state.
//!
//! If no JVM (or no `javac`) can be located at test time, every test skips
//! gracefully with an `eprintln` reason (the `jni` crate's java-locator uses
//! `JAVA_HOME`, then `java` on `PATH`, then the Windows registry).

use std::sync::LazyLock;

use rjava::prelude::*;

// ---------------------------------------------------------------------------
// Bindings
// ---------------------------------------------------------------------------

// The goal example: a JDK class with an instance method that returns the
// same class (`-> Self`), a primitive return and a `String` return.
rjava::bind! {
    "java.lang.StringBuilder" => StringBuilder {
        fn append(s: &str) -> Self;
        fn length() -> i32;
        fn toString() -> String;
        fn toUpperCase() -> String;   // does NOT exist on StringBuilder (error test)
    }
}

// A JDK class with static methods (typed static calls).
rjava::bind! {
    "java.lang.Math" => Math {
        static fn max(a: i32, b: i32) -> i32;
    }
}

// The fixture class: Option/Vec round trips, i64, chainable setters,
// a static factory (`static fn ... -> Self`) and JObject-annotated methods
// that exercise the exact-signature reflection fallback.
rjava::bind! {
    "bind.BindKit" => BindKit {
        fn label() -> String;
        fn setLabel(s: String) -> Self;
        fn value() -> i64;
        fn setValue(v: i64) -> Self;
        fn add(a: i64, b: i64) -> i64;
        fn splitWords(s: String) -> Vec<String>;
        fn joinWords(words: Vec<String>) -> String;
        fn nullableString(mode: i32) -> Option<String>;
        fn nullableArray(mode: i32) -> Option<Vec<String>>;
        fn echo(o: JObject) -> String;       // real signature: echo(String)
        fn raw() -> JObject;                  // real return type: String
        static fn staticAdd(a: i64, b: i64) -> i64;
        static fn create(label: String) -> Self;
        field tag: String;
        field count: i32;
        field active: bool;
        field ready: bool;
        field usable: bool;
        static field MAGIC: i32;
        fn doubled(x: i32) -> i32 [java_name = "compute"];
    }
}

// A second wrapper for the same class whose declared types are *wrong*:
// `echo` really takes a `String`, not an `int`, so the call must fail with
// a clear error (the reflection fallback finds no matching candidate).
rjava::bind! {
    "bind.BindKit" => BindKitWrong {
        fn echo(n: i32) -> String;
    }
}

// A class that does not exist: the error must surface at first use.
rjava::bind! {
    "com.example.NoSuchClass" => Ghost {
        fn ping() -> ();
    }
}

// ---------------------------------------------------------------------------
// JVM bootstrap (mirrors tests/constructor_bind.rs)
// ---------------------------------------------------------------------------

/// Compile the `bind` fixture exactly once per test process. `Err` carries
/// the reason, reported once; when it fails every test skips.
static FIXTURE_COMPILED: LazyLock<Result<(), String>> = LazyLock::new(compile_fixture);

fn compile_fixture() -> Result<(), String> {
    let out_dir = "target/rjava-bind-classes";
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
        .arg("tests/java/bind/BindKit.java")
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
        .class_path("target/rjava-bind-classes")
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

/// Full round trip on a JDK class: construction via the typed `Java::new`,
/// `-> Self` chaining, and typed primitive/String returns.
#[test]
fn string_builder_round_trip() {
    let Some(java) = jvm() else { return };
    let sb = java.new::<StringBuilder>(("Hello",)).unwrap();
    let sb = sb.append(" world").unwrap(); // Self chaining: same object re-wrapped
    let s: String = sb.toString().unwrap();
    assert_eq!(s, "Hello world");
    let len: i32 = sb.length().unwrap();
    assert_eq!(len, 11);
}

/// `Java::wrap` wraps an existing handle (the class is validated lazily on
/// the first actual call).
#[test]
fn wrap_existing_object() {
    let Some(java) = jvm() else { return };
    let raw: JObject = java.new_object("java.lang.StringBuilder", ("wrapped",)).unwrap();
    let sb: StringBuilder = java.wrap(raw);
    sb.append("!").unwrap();
    let s: String = sb.toString().unwrap();
    assert_eq!(s, "wrapped!");
}

/// Typed static calls on a JDK class.
#[test]
fn math_static_call() {
    let Some(java) = jvm() else { return };
    let m: i32 = Math::max(&java, 3, 7).unwrap();
    assert_eq!(m, 7);
    let m2: i32 = Math::max(&java, -1, -5).unwrap();
    assert_eq!(m2, -1);
}

/// The fixture class: constructor args, chainable setters, i64 arithmetic.
#[test]
fn fixture_round_trip() {
    let Some(java) = jvm() else { return };
    let kit = java.new::<BindKit>(("kit", 5_i64)).unwrap();
    assert_eq!(kit.label().unwrap(), "kit");
    assert_eq!(kit.value().unwrap(), 5);
    let kit = kit.setLabel("renamed".to_string()).unwrap(); // Self chaining
    assert_eq!(kit.label().unwrap(), "renamed");
    let kit = kit.setValue(9).unwrap();
    assert_eq!(kit.value().unwrap(), 9);
    assert_eq!(kit.add(1_i64, 2_i64).unwrap(), 3);
    assert_eq!(kit.add(-5, 5).unwrap(), 0);
}

/// Static factory (`static fn ... -> Self`) and a static method with i64
/// arguments.
#[test]
fn static_factory_and_static_method() {
    let Some(java) = jvm() else { return };
    let kit = BindKit::create(&java, "made".to_string()).unwrap();
    assert_eq!(kit.label().unwrap(), "made");
    let n: i64 = BindKit::staticAdd(&java, 40_i64, 2_i64).unwrap();
    assert_eq!(n, 42);
}

/// `Vec<String>` round trips: `String[]` → `Vec<String>` and back.
#[test]
fn string_arrays_round_trip() {
    let Some(java) = jvm() else { return };
    let kit = java.new::<BindKit>(("kit", 0_i64)).unwrap();
    let words: Vec<String> = kit.splitWords("a b c".to_string()).unwrap();
    assert_eq!(words, vec!["a", "b", "c"]);
    let joined: String = kit
        .joinWords(vec!["x".to_string(), "y".to_string()])
        .unwrap();
    assert_eq!(joined, "x y");
}

/// `Option<T>` returns: null → `None`, values → `Some` (mirroring the
/// dynamic path's null-tolerance), for both plain and array types.
#[test]
fn option_null_tolerance() {
    let Some(java) = jvm() else { return };
    let kit = java.new::<BindKit>(("kit", 0_i64)).unwrap();
    let none: Option<String> = kit.nullableString(0).unwrap();
    assert!(none.is_none(), "null String must map to None");
    let some: Option<String> = kit.nullableString(1).unwrap();
    assert_eq!(some.as_deref(), Some("present"));
    let none_arr: Option<Vec<String>> = kit.nullableArray(0).unwrap();
    assert!(none_arr.is_none(), "null String[] must map to None");
    let some_arr: Option<Vec<String>> = kit.nullableArray(1).unwrap();
    assert_eq!(some_arr, Some(vec!["a".to_string(), "b".to_string()]));
}

/// The exact-signature reflection fallback resolves a declared `JObject`
/// against the real method signature, in both parameter and return position.
#[test]
fn reflection_fallback_resolves_generic_annotations() {
    let Some(java) = jvm() else { return };
    let kit = java.new::<BindKit>(("kit", 0_i64)).unwrap();
    // Declared (Ljava/lang/Object;) but the real method takes (Ljava/lang/String;).
    let arg: JObject = java.new_object("java.lang.String", ("hello",)).unwrap();
    let echoed: String = kit.echo(arg).unwrap();
    assert_eq!(echoed, "hello");
    // Declared ()Ljava/lang/Object; but the real return type is String.
    let raw: JObject = kit.raw().unwrap();
    let s: String = raw.call("toString", ()).unwrap();
    assert_eq!(s, "raw");
}

/// Error cases: a nonexistent class fails at first use, a nonexistent
/// method fails with a clear error, and a *wrong* declared argument type
/// fails with the fallback's "could not resolve" error.
#[test]
fn error_cases() {
    let Some(java) = jvm() else { return };

    // Nonexistent class → clear error at first use (the class cache init).
    let err = match java.new::<Ghost>(()) {
        Ok(_) => panic!("nonexistent class must fail at first use"),
        Err(e) => e,
    };
    let is_class_not_found = match &err {
        JavaError::WithContext { operation, source } => {
            assert!(
                operation.contains("constructing"),
                "operation must name the failed construction: {operation}"
            );
            match &**source {
                JavaError::JavaException { class, .. } => class.contains("ClassNotFound"),
                JavaError::Jni(jni::errors::Error::NoClassDefFound { .. }) => true,
                _ => false,
            }
        }
        JavaError::JavaException { class, .. } => class.contains("ClassNotFound"),
        JavaError::Jni(jni::errors::Error::NoClassDefFound { .. }) => true,
        _ => false,
    };
    assert!(is_class_not_found, "expected a class-not-found error, got {err:?}");

    // Nonexistent method → the first GetMethodID fails and the reflection
    // fallback reports that no matching method exists.
    let sb = java.new::<StringBuilder>(("x",)).unwrap();
    let err = sb.toUpperCase().expect_err("nonexistent method must fail");
    assert!(
        matches!(err, JavaError::InvalidArgument(_)),
        "expected the fallback's could-not-resolve error, got {err:?}"
    );

    // Wrong declared argument type for a method that exists → the fallback
    // finds no matching candidate (int vs the real String parameter).
    let kit = java.new::<BindKitWrong>(("k", 0_i64)).unwrap();
    let err = kit.echo(5_i32).expect_err("wrong declared arg type must fail");
    assert!(
        matches!(err, JavaError::InvalidArgument(_)),
        "expected the fallback's could-not-resolve error, got {err:?}"
    );
}

/// The dynamic escape hatch: typed `bind!` calls and dynamic `call`/
/// `call_void` calls mix freely on the same object.
#[test]
fn mixed_typed_and_dynamic_calls() {
    let Some(java) = jvm() else { return };
    let sb = java.new::<StringBuilder>(("Typed ",)).unwrap();
    sb.append("plus ").unwrap(); // typed
    sb.obj().call_void("append", ("dynamic",)).unwrap(); // dynamic, same object
    let s: String = sb.obj().call("toString", ()).unwrap();
    assert_eq!(s, "Typed plus dynamic");
    let len: i32 = sb.length().unwrap(); // still typed afterwards
    assert_eq!(len, 18);
}

/// The `field` declarations: typed getters/setters over the Java fields —
/// `get_<name>` / `set_<name>` — including a raw bool field, the bool
/// accessor fallback (a missing field falls back to `get<Name>()` then
/// `is<Name>()`, in the bean read order), and static fields.
#[test]
fn fields_round_trip() {
    let Some(java) = jvm() else { return };
    let kit = java.new::<BindKit>(("kit", 0_i64)).unwrap();

    // Raw instance fields.
    kit.set_tag("hello".to_string()).unwrap();
    assert_eq!(kit.get_tag().unwrap(), "hello");
    kit.set_count(5).unwrap();
    assert_eq!(kit.get_count().unwrap(), 5);

    // A raw bool field reads and writes directly.
    kit.set_active(true).unwrap();
    assert!(kit.get_active().unwrap());
    kit.set_active(false).unwrap();
    assert!(!kit.get_active().unwrap());

    // No field `ready` — the getter falls back to the accessor methods:
    // `getReady()` (missing) then `isReady()`.
    assert!(kit.get_ready().unwrap(), "the isReady() accessor must be found");

    // No field `usable` either, but both `getUsable()` (true) and
    // `isUsable()` (false) exist — the get-then-is order must win.
    assert!(
        kit.get_usable().unwrap(),
        "getUsable() must be tried before isUsable()"
    );

    // Static fields: get_static_<name> / set_static_<name>.
    assert_eq!(BindKit::get_static_magic(&java).unwrap(), 7);
    BindKit::set_static_magic(&java, 8).unwrap();
    assert_eq!(BindKit::get_static_magic(&java).unwrap(), 8);
}

/// The `[java_name = "…"]` alias: the trait-free wrapper method keeps the
/// idiomatic Rust name while the Java call targets the aliased method.
#[test]
fn java_name_alias_reroutes_the_call() {
    let Some(java) = jvm() else { return };
    let kit = java.new::<BindKit>(("kit", 0_i64)).unwrap();
    assert_eq!(kit.doubled(21).unwrap(), 42);
}
