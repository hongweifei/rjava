#![forbid(unsafe_code)]
//! Integration tests against a real JVM. Most tests use only JDK classes;
//! the native-method tests additionally compile a small fixture
//! (`tests/java/com/example/NativeLib.java`) with `javac` and register Rust
//! functions as its `native` methods.
//!
//! The `#![forbid(unsafe_code)]` on the first line is intentional: it proves
//! that the `native!`/`native_inst!` macro expansions are user-side
//! `unsafe`-free (the only `unsafe` in the feature lives in the
//! `rjava-helper` crate's `register_natives` helper).
//!
//! If no JVM (or no `javac`) can be located at test time, every test skips
//! gracefully with an `eprintln` reason (the `jni` crate's java-locator uses
//! `JAVA_HOME`, then `java` on `PATH`, then the Windows registry).

use std::sync::OnceLock;

use rjava::prelude::*;
use rjava::{native, native_inst};

/// Compile the Java fixture exactly once per test process. `Err` carries the
/// reason, reported once; when it fails every test skips.
static FIXTURE_COMPILED: OnceLock<Result<(), String>> = OnceLock::new();

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
        .arg("tests/java/com/example/NativeLib.java")
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

/// Compile + jar the *plugin* fixtures (an API jar and a plugin jar) exactly
/// once per test process. `Err` carries the reason, reported once; when it
/// fails the plugin tests skip.
///
/// Unlike the `NativeLib` fixture these classes are deliberately **not** on
/// the JVM's system class path: the tests must load the jars at runtime via
/// [`Java::load_jar`](rjava::Java::load_jar) / `class_loader_with_parent`,
/// which is the whole point.
static PLUGIN_FIXTURE: OnceLock<Result<(), String>> = OnceLock::new();

fn compile_plugin_fixture() -> Result<(), String> {
    let out_dir = "target/rjava-plugin-classes";
    std::fs::create_dir_all(out_dir).map_err(|e| format!("cannot create {out_dir}: {e}"))?;
    let (javac, jar) = match std::env::var("JAVA_HOME") {
        Ok(home) => {
            let exe = if cfg!(windows) { ".exe" } else { "" };
            (
                format!("{home}/bin/javac{exe}"),
                format!("{home}/bin/jar{exe}"),
            )
        }
        Err(_) => ("javac".to_string(), "jar".to_string()),
    };
    // One javac invocation compiles the API classes and the plugin class
    // against them (the API sources are on the same command line).
    let output = std::process::Command::new(&javac)
        .arg("-d")
        .arg(out_dir)
        .arg("tests/java/plugin/com/example/plugin/api/Plugin.java")
        .arg("tests/java/plugin/com/example/plugin/api/Bridge.java")
        .arg("tests/java/plugin/com/example/plugin/HelloPlugin.java")
        .output()
        .map_err(|e| format!("could not run `{javac}`: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "`{javac}` failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    // API jar: only the api package. Plugin jar: only HelloPlugin.class —
    // jarring the whole `com/example/plugin` tree would sweep the api classes
    // in too, giving the plugin loader its own (native-less) Bridge copy.
    let jars = [
        ("target/rjava-test-api.jar", "com/example/plugin/api"),
        (
            "target/rjava-test-plugin.jar",
            "com/example/plugin/HelloPlugin.class",
        ),
    ];
    for (out, entry) in jars {
        let output = std::process::Command::new(&jar)
            .arg("cf")
            .arg(out)
            .arg("-C")
            .arg(out_dir)
            .arg(entry)
            .output()
            .map_err(|e| format!("could not run `{jar}`: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "`{jar}` failed:\n{}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    Ok(())
}

/// Attempt to build the JVM; returns `None` (after an `eprintln`) if no JVM
/// is available so tests can skip instead of failing on JVM-less machines.
///
/// The fixture is compiled before the first JVM creation, and the fixture
/// class path is always set, so whichever test creates the singleton JVM
/// first gets the native-method fixture on the class path.
fn jvm() -> Option<Java> {
    if let Err(reason) = FIXTURE_COMPILED.get_or_init(compile_fixture) {
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

#[test]
fn jvm_builds_and_reuses() {
    let Some(java) = jvm() else { return };
    // A second build() reuses the running JVM instead of failing — this is
    // the documented replacement for the former `with_current()`: whenever
    // the process JVM is known to this crate, `build()` hands it back.
    let again = Java::builder().build().expect("reuse must succeed");
    let n: i32 = again
        .class("java.lang.Math")
        .expect("Math class")
        .call_static("abs", (-42_i32,))
        .expect("abs call");
    assert_eq!(n, 42);
    let _ = java;
}

#[test]
fn constructor_and_instance_methods() {
    let Some(java) = jvm() else { return };
    let clazz = java.class("java.lang.StringBuilder").unwrap();
    let sb = clazz.new_instance(("Hello",)).unwrap();
    let len: i32 = sb.call("length", ()).unwrap();
    assert_eq!(len, 5);
    sb.call_void("append", (" world",)).unwrap();
    sb.call_void("append", (42_i32,)).unwrap();
    let s: String = sb.call("toString", ()).unwrap();
    assert_eq!(s, "Hello world42");
    // Runtime class of an object.
    let rt = sb.class().unwrap();
    assert_eq!(rt.name().unwrap(), "java.lang.StringBuilder");
    // Facade sugar: java.new_object.
    let sb2 = java.new_object("java.lang.StringBuilder", ("Hi",)).unwrap();
    let s2: String = sb2.call("toString", ()).unwrap();
    assert_eq!(s2, "Hi");
}

#[test]
fn static_members() {
    let Some(java) = jvm() else { return };
    let math = java.class("java.lang.Math").unwrap();
    let max: i32 = math.call_static("max", (3_i32, 7_i32)).unwrap();
    assert_eq!(max, 7);
    let pi: f64 = math.get_static_field("PI").unwrap();
    assert!((pi - std::f64::consts::PI).abs() < 1e-12);
    // Static field read with a reference annotation: field lookup matches the
    // exact field type, so Boolean.TYPE (a java.lang.Class) reads via the
    // JClass annotation. (System.out would need PrintStream, which rjava has
    // no handle type for.)
    let type_class: JClass = java.class("java.lang.Boolean").unwrap().get_static_field("TYPE").unwrap();
    let _ = type_class;
    // Call a static with a boxed argument (Object-typed parameter).
    let integer = java.class("java.lang.Integer").unwrap();
    let parsed: i32 = integer.call_static("parseInt", ("123",)).unwrap();
    assert_eq!(parsed, 123);
    // Static methods returning objects with JObject annotation (fallback path).
    let as_str: JObject = integer.call_static("valueOf", (456_i32,)).unwrap();
    let back: i32 = as_str.call("intValue", ()).unwrap();
    assert_eq!(back, 456);
}

#[test]
fn void_calls_on_non_void_methods() {
    let Some(java) = jvm() else { return };
    // `append` returns StringBuilder; call_void discards it via the
    // reflection fallback.
    let sb = java.new_object("java.lang.StringBuilder", ("a",)).unwrap();
    sb.call_void("append", ("b",)).unwrap();
    let s: String = sb.call("toString", ()).unwrap();
    assert_eq!(s, "ab");
    // And `() as R` works on a call site too.
    let _: () = sb.call("append", ("c",)).unwrap();
    let s: String = sb.call("toString", ()).unwrap();
    assert_eq!(s, "abc");
}

#[test]
fn exceptions_become_typed_errors() {
    let Some(java) = jvm() else { return };
    let integer = java.class("java.lang.Integer").unwrap();
    let err = integer
        .call_static::<_, i32>("parseInt", ("not-a-number",))
        .unwrap_err();
    match err {
        JavaError::WithContext { operation, source } => {
            // The failure names the operation: method + arg types + class.
            assert_eq!(
                operation, "calling parseInt(String) on java.lang.Integer",
                "operation must name the failed call"
            );
            match *source {
                JavaError::JavaException { class, message } => {
                    assert_eq!(class, "java.lang.NumberFormatException");
                    assert!(message.contains("not-a-number"), "message was {message:?}");
                }
                other => panic!("expected a JavaException source, got {other:?}"),
            }
        }
        other => panic!("expected WithContext, got {other:?}"),
    }
    // A failed call must not poison the thread: the next call works.
    let ok: i32 = integer.call_static("parseInt", ("7",)).unwrap();
    assert_eq!(ok, 7);
}

#[test]
fn strings_include_non_ascii() {
    let Some(java) = jvm() else { return };
    let s = java.new_object("java.lang.String", ("你好，世界 🌍",)).unwrap();
    let len: i32 = s.call("length", ()).unwrap();
    assert_eq!(len, 8); // 4 Chinese chars + comma + emoji surrogate pair
    let upper: String = s.call("toUpperCase", ()).unwrap();
    assert_eq!(upper, "你好，世界 🌍");
    // Round-trip through a method that returns a String.
    let text = java.new_object("java.lang.String", ("hello",)).unwrap();
    let replaced: String = text.call("replace", ("l", "L")).unwrap();
    assert_eq!(replaced, "heLLo");
    // Empty string round-trip.
    let empty = java.new_object("java.lang.String", ("",)).unwrap();
    let e: String = empty.call("toString", ()).unwrap();
    assert_eq!(e, "");
}

#[test]
fn char_and_option_conversions() {
    let Some(java) = jvm() else { return };
    let sb = java.new_object("java.lang.StringBuilder", ()).unwrap();
    sb.call_void("append", ('A',)).unwrap();
    let s: String = sb.call("toString", ()).unwrap();
    assert_eq!(s, "A");
    // char at position 0
    let c: char = sb.call("charAt", (0,)).unwrap();
    assert_eq!(c, 'A');
    // Option<String>: null -> None (System.getProperty returns null for unset keys)
    let prop: Option<String> = java
        .class("java.lang.System")
        .unwrap()
        .call_static("getProperty", ("rjava.does.not.exist",))
        .unwrap();
    assert_eq!(prop, None);
    let prop: Option<JObject> = java
        .class("java.lang.System")
        .unwrap()
        .call_static("getProperty", ("java.home",))
        .unwrap();
    assert!(prop.is_some());
    // Null object return as JObject -> error.
    let err = java
        .class("java.lang.System")
        .unwrap()
        .call_static::<_, JObject>("getProperty", ("rjava.does.not.exist",))
        .unwrap_err();
    // The failure names the operation and keeps the underlying
    // InvalidArgument (the null-to-JObject conversion rejection).
    match err {
        JavaError::WithContext { operation, source } => {
            assert_eq!(
                operation, "calling getProperty(String) on java.lang.System",
                "operation must name the failed call"
            );
            assert!(
                matches!(*source, JavaError::InvalidArgument(_)),
                "expected an InvalidArgument source, got {source:?}"
            );
        }
        other => panic!("expected WithContext, got {other:?}"),
    }
}

#[test]
fn primitive_arrays() {
    let Some(java) = jvm() else { return };
    let arr: JArray<i32> = java.new_array(10).unwrap();
    assert_eq!(arr.len().unwrap(), 10);
    arr.set(0, 42).unwrap();
    arr.set(9, -1).unwrap();
    let v: i32 = arr.get(0).unwrap();
    assert_eq!(v, 42);
    let last: i32 = arr.get(9).unwrap();
    assert_eq!(last, -1);
    // to_vec / from_vec
    let mut vec: Vec<i32> = arr.to_vec().unwrap();
    assert_eq!(vec.len(), 10);
    assert_eq!(vec[0], 42);
    vec[3] = 7;
    let arr2 = JArray::<i32>::from_vec(vec).unwrap();
    let v3: i32 = arr2.get(3).unwrap();
    assert_eq!(v3, 7);
    // All primitive element types
    let bytes: JArray<i8> = java.new_array(2).unwrap();
    bytes.set(0, -128).unwrap();
    let b: i8 = bytes.get(0).unwrap();
    assert_eq!(b, -128);
    let longs: JArray<i64> = java.new_array(1).unwrap();
    longs.set(0, i64::MAX).unwrap();
    let l: i64 = longs.get(0).unwrap();
    assert_eq!(l, i64::MAX);
    let doubles: JArray<f64> = java.new_array(1).unwrap();
    doubles.set(0, 2.5).unwrap();
    let d: f64 = doubles.get(0).unwrap();
    assert_eq!(d, 2.5);
    let bools: JArray<bool> = java.new_array(2).unwrap();
    bools.set(1, true).unwrap();
    let t: bool = bools.get(1).unwrap();
    assert!(t);
    let chars: JArray<char> = java.new_array(1).unwrap();
    chars.set(0, '中').unwrap();
    let c: char = chars.get(0).unwrap();
    assert_eq!(c, '中');
}

#[test]
fn byte_arrays_via_vec() {
    let Some(java) = jvm() else { return };
    // Vec<u8> maps to byte[] via i8 casts: String.getBytes() returns byte[].
    let s = java.new_object("java.lang.String", ("bytes",)).unwrap();
    let bytes: Vec<u8> = s.call("getBytes", ()).unwrap();
    assert_eq!(bytes, b"bytes".to_vec());
    // And String(byte[]) ctor round-trips Vec<u8>.
    let data = b"hi".to_vec();
    let s2 = java.new_object("java.lang.String", (data,)).unwrap();
    let back: String = s2.call("toString", ()).unwrap();
    assert_eq!(back, "hi");
    // Vec<i32> as an argument: Arrays.sort(int[]) is void and mutates in
    // place, so the sorted order is observable through the same array.
    let ints = vec![3, 1, 2];
    let arr = JArray::<i32>::from_vec(ints).unwrap();
    java.class("java.util.Arrays")
        .unwrap()
        .call_static::<_, ()>("sort", (&arr,))
        .unwrap();
    let sorted: Vec<i32> = arr.to_vec().unwrap();
    assert_eq!(sorted, vec![1, 2, 3]);
}

#[test]
fn object_arrays_and_null_elements() {
    let Some(java) = jvm() else { return };
    let objs: JArray<JObject> = java.new_object_array("java.lang.String", 3).unwrap();
    assert_eq!(objs.len().unwrap(), 3);
    let a = java.new_object("java.lang.String", ("a",)).unwrap();
    objs.set(0, a).unwrap();
    // index 1 stays null.
    let maybe: Option<JObject> = objs.get(1).unwrap();
    assert!(maybe.is_none());
    let present: Option<JObject> = objs.get(0).unwrap();
    let s: String = present.unwrap().call("toString", ()).unwrap();
    assert_eq!(s, "a");
    // Null element read as JObject -> InvalidArgument.
    let err = objs.get::<JObject>(1).unwrap_err();
    assert!(matches!(err, JavaError::InvalidArgument(_)));
    // to_vec_options keeps nulls; to_vec errors on them.
    let opts = objs.to_vec_options().unwrap();
    assert_eq!(opts.len(), 3);
    assert!(opts[1].is_none());
    let err = objs.to_vec().unwrap_err();
    assert!(matches!(err, JavaError::InvalidArgument(_)));
}

#[test]
fn vec_of_objects_as_argument() {
    let Some(java) = jvm() else { return };
    let items: Vec<JObject> = vec![
        java.new_object("java.lang.String", ("x",)).unwrap(),
        java.new_object("java.lang.String", ("y",)).unwrap(),
    ];
    // A Vec<JObject> converts to an Object[]; it resolves against
    // Object[]-typed parameters. Arrays.asList(T...) is the canonical one
    // (ArrayList.addAll(Collection) is not reachable — an Object[] is not a
    // Collection, and rjava deliberately does not invent one).
    let list: JObject = java
        .class("java.util.Arrays")
        .unwrap()
        .call_static("asList", (&items,))
        .unwrap();
    let size: i32 = list.call("size", ()).unwrap();
    assert_eq!(size, 2);
    // And back: list.toArray() -> Object[] -> Vec<JObject>
    let arr: Vec<JObject> = list.call("toArray", ()).unwrap();
    assert_eq!(arr.len(), 2);
}

#[test]
fn autoboxing_for_object_parameters() {
    let Some(java) = jvm() else { return };
    // ArrayList.add(Object): a primitive int is boxed to Integer.
    let list = java.new_object("java.util.ArrayList", ()).unwrap();
    list.call_void("add", (10_i32,)).unwrap();
    let first: JObject = list.call("get", (0_i32,)).unwrap();
    let v: i32 = first.call("intValue", ()).unwrap();
    assert_eq!(v, 10);
    // HashMap.put(Object, Object): the value is boxed too.
    let map = java.new_object("java.util.HashMap", ()).unwrap();
    map.call_void("put", ("k", 1_i32)).unwrap();
    let size: i32 = map.call("size", ()).unwrap();
    assert_eq!(size, 1);
    let got: JObject = map.call("get", ("k",)).unwrap();
    let n: i32 = got.call("intValue", ()).unwrap();
    assert_eq!(n, 1);
    // Exact-match priority: Math.max(int, int) resolves exactly (no boxing
    // pass is even consulted).
    let max: i32 = java
        .class("java.lang.Math")
        .unwrap()
        .call_static("max", (3_i32, 7_i32))
        .unwrap();
    assert_eq!(max, 7);
}

#[test]
fn unboxing_boxed_args_to_primitive_params() {
    let Some(java) = jvm() else { return };
    // Mixed exact + unbox in one call: Math.max(int, int) with a boxed
    // Integer for one argument and a plain i32 for the other.
    let boxed = java.new_object("java.lang.Integer", (5_i32,)).unwrap();
    let max: i32 = java
        .class("java.lang.Math")
        .unwrap()
        .call_static("max", (&boxed, 3_i32))
        .unwrap();
    assert_eq!(max, 5);
    // Pure unbox: Math.multiplyExact(long, long) with two boxed Longs.
    let a = java.new_object("java.lang.Long", (2_i64,)).unwrap();
    let b = java.new_object("java.lang.Long", (21_i64,)).unwrap();
    let product: i64 = java
        .class("java.lang.Math")
        .unwrap()
        .call_static("multiplyExact", (&a, &b))
        .unwrap();
    assert_eq!(product, 42);
    // Boxed Character for a char parameter. isDigit(char) coexists with
    // isDigit(int), but the int overload cannot accept a Character (unboxing
    // requires the exact wrapper type), so this deterministically exercises
    // the unboxing pass — no widening or Object-typed overload can win
    // instead — and the `true` proves the char value really flowed through.
    let ch = java.new_object("java.lang.Character", ('7',)).unwrap();
    let digit: bool = java
        .class("java.lang.Character")
        .unwrap()
        .call_static("isDigit", (&ch,))
        .unwrap();
    assert!(digit);
    // StringBuilder.append(char) with a boxed Character, end to end.
    let sb = java.new_object("java.lang.StringBuilder", ()).unwrap();
    let c = java.new_object("java.lang.Character", ('x',)).unwrap();
    sb.call_void("append", (&c,)).unwrap();
    let s: String = sb.call("toString", ()).unwrap();
    assert_eq!(s, "x");
}

#[test]
fn unboxing_constructor() {
    let Some(java) = jvm() else { return };
    // StringBuilder(int) resolved via a boxed Integer: capacity() == value.
    let boxed = java.new_object("java.lang.Integer", (42_i32,)).unwrap();
    let sb = java.new_object("java.lang.StringBuilder", (&boxed,)).unwrap();
    let capacity: i32 = sb.call("capacity", ()).unwrap();
    assert_eq!(capacity, 42);
}

#[test]
fn unboxing_wrapper_mismatch_fails() {
    let Some(java) = jvm() else { return };
    // An Integer must NOT match Math.sqrt(double): unboxing requires the
    // exact wrapper type and there is no widening. (Math.abs was avoided
    // here because a boxed Long *legitimately* resolves Math.abs to
    // abs(long) — exactly as Java's own unboxing would pick it.)
    let boxed = java.new_object("java.lang.Integer", (5_i32,)).unwrap();
    let err = java
        .class("java.lang.Math")
        .unwrap()
        .call_static::<_, i32>("sqrt", (&boxed,))
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("could not resolve"),
        "expected the reflection could-not-resolve error, got: {msg}"
    );
}

#[test]
fn unboxing_null_object_not_unboxed() {
    let Some(java) = jvm() else { return };
    // A null object never matches a primitive parameter: resolution fails
    // cleanly with the standard could-not-resolve error (no NPE from the
    // resolver itself).
    let null: Option<JObject> = None;
    let err = java
        .class("java.lang.Math")
        .unwrap()
        .call_static::<_, i32>("abs", (null,))
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("could not resolve"),
        "expected the reflection could-not-resolve error, got: {msg}"
    );
}

#[test]
fn constructor_fallback_and_boxing() {
    let Some(java) = jvm() else { return };
    // WeakReference(Object) ctor with a String argument — assignable to
    // Object, resolved via the reflection fallback. The String handle keeps
    // the referent strongly reachable, so get() is deterministic.
    let s = java.new_object("java.lang.String", ("x",)).unwrap();
    let wr = java.new_object("java.lang.ref.WeakReference", (&s,)).unwrap();
    let got: JObject = wr.call("get", ()).unwrap();
    let text: String = got.call("toString", ()).unwrap();
    assert_eq!(text, "x");
    // Long(long) is an exact ctor match (no fallback needed) — confirms
    // exact matches keep priority.
    let l = java.new_object("java.lang.Long", (2_i64,)).unwrap();
    let v: i64 = l.call("longValue", ()).unwrap();
    assert_eq!(v, 2);
}

#[test]
fn arrays_from_vec_facade() {
    let Some(java) = jvm() else { return };
    // Java::new_array_from: int[] from a Vec.
    let arr: JArray<i32> = java.new_array_from(vec![1_i32, 2, 3]).unwrap();
    let v: Vec<i32> = arr.to_vec().unwrap();
    assert_eq!(v, vec![1, 2, 3]);
    // Java::new_object_array_from: Object[] from a Vec<JObject>.
    let objs: JArray<JObject> = java
        .new_object_array_from(vec![
            java.new_object("java.lang.String", ("a",)).unwrap(),
        ])
        .unwrap();
    let s: String = objs.get(0).unwrap();
    assert_eq!(s, "a");
}

#[test]
fn instance_fields() {
    let Some(java) = jvm() else { return };
    // JNI field lookup bypasses Java access control, so private/protected
    // fields of JDK classes are readable and writable directly.
    // Read: java.lang.Integer's private `int value`.
    let n = java.new_object("java.lang.Integer", (42_i32,)).unwrap();
    let v: i32 = n.get_field("value").unwrap();
    assert_eq!(v, 42);
    // Write/read round-trip: java.util.Vector's protected `int elementCount`.
    let vec = java.new_object("java.util.Vector", ()).unwrap();
    vec.set_field("elementCount", 3_i32).unwrap();
    let count: i32 = vec.get_field("elementCount").unwrap();
    assert_eq!(count, 3);
    // The value written is observable by Java itself (size() is derived from
    // elementCount); leave the vector untouched afterwards as documented.
    // A missing field is an error.
    let err = n.get_field::<i32>("no_such_field").unwrap_err();
    assert!(matches!(err, JavaError::Jni(_) | JavaError::JavaException { .. }));
}

#[test]
fn threads_auto_attach() {
    let Some(java) = jvm() else { return };
    let mut handles = Vec::new();
    for i in 0..4 {
        let java = java.clone();
        handles.push(std::thread::spawn(move || -> JavaResult<i64> {
            let math = java.class("java.lang.Math")?;
            let n: i64 = math.call_static("multiplyExact", (i as i64, 10_000_000_000_i64))?;
            Ok(n)
        }));
    }
    for (i, h) in handles.into_iter().enumerate() {
        let n = h.join().expect("thread panicked").unwrap();
        assert_eq!(n, i as i64 * 10_000_000_000);
    }
}

#[test]
fn attach_thread_raii_detach() {
    let Some(java) = jvm() else { return };
    std::thread::spawn(move || -> JavaResult<()> {
        {
            let _guard = java.attach_thread()?;
            let n: i32 = java
                .class("java.lang.Math")?
                .call_static("abs", (-5_i32,))?;
            assert_eq!(n, 5);
            // guard dropped here -> thread detached
        }
        // Re-attaching after the guard is dropped works automatically.
        let m: i32 = java.class("java.lang.Math")?.call_static("abs", (-9_i32,))?;
        assert_eq!(m, 9);
        Ok(())
    })
    .join()
    .expect("thread panicked")
    .unwrap();
}

#[test]
fn clone_is_cheap_and_shares_lifetime() {
    let Some(java) = jvm() else { return };
    let sb = java.new_object("java.lang.StringBuilder", ("x",)).unwrap();
    let sb2 = sb.clone();
    sb2.call_void("append", ("y",)).unwrap();
    let s: String = sb.call("toString", ()).unwrap();
    assert_eq!(s, "xy"); // same underlying object
    drop(sb2);
    let s: String = sb.call("toString", ()).unwrap();
    assert_eq!(s, "xy"); // still alive via sb
}

#[test]
fn handles_are_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Java>();
    assert_send_sync::<JObject>();
    assert_send_sync::<JClass>();
    assert_send_sync::<JClassLoader>();
    assert_send_sync::<JArray<i32>>();
    assert_send_sync::<JavaThread>();
    assert_send_sync::<JavaError>();
}

#[test]
fn prelude_exports_the_public_api() {
    // Compile-time check that the prelude surface is intact.
    fn _uses(_: &Java, _: &JavaThread, _: &JClass, _: &JObject, _: &JArray<i32>, _: JavaError) {}
    let _ = rjava::prelude::JavaResult::<()>::Ok(());
    let _ = rjava::prelude::JavaError::InvalidArgument("x");
}

// ---------------------------------------------------------------------------
// Runtime class loading: plugin jars (URLClassLoader)
// ---------------------------------------------------------------------------

// The Rust implementation the host injects into the API's Bridge.rustEcho.
fn plugin_rust_echo(_env: &mut jni::Env, (s,): (String,)) -> JavaResult<String> {
    Ok(format!("{s} from rust"))
}

#[test]
fn plugin_workflow_end_to_end() {
    let Some(java) = jvm() else { return };
    if let Err(reason) = PLUGIN_FIXTURE.get_or_init(compile_plugin_fixture) {
        eprintln!("SKIPPING plugin test: {reason}");
        return;
    }
    // 1) The API jar is the compile-time contract: interfaces + a Bridge
    //    declaring `native` methods. The host loads it at runtime (it is NOT
    //    on the JVM's system class path) and injects the Rust implementation.
    let api = java.load_jar("target/rjava-test-api.jar").unwrap();
    let bridge = api.load_class("com.example.plugin.api.Bridge").unwrap();
    // Explicit-signature form: `rustEcho`'s Rust signature `(String,) ->
    // String` is the same as the derived `shout` registration in the native
    // section, so a derived registration here would trip the shared-trampoline
    // collision check — the explicit form is exactly that escape hatch.
    bridge
        .register_natives(&[native!(
            "rustEcho",
            "(Ljava/lang/String;)Ljava/lang/String;",
            plugin_rust_echo
        )
        .unwrap()])
        .unwrap();
    // 2) The plugin jar is loaded by a loader whose parent is the API loader,
    //    so plugin code resolves the API interface + Bridge through the very
    //    classes the host registered natives on.
    let plugin_jar = java
        .class_loader_with_parent(&["target/rjava-test-plugin.jar"], &api)
        .unwrap();
    let hello = plugin_jar.load_class("com.example.plugin.HelloPlugin").unwrap();
    let obj = hello.new_instance(()).unwrap();
    let name: String = obj.call("name", ()).unwrap();
    // Proves: runtime jar loading, Rust injection via natives, and plugin code
    // calling the Rust-backed Bridge.
    assert_eq!(name, "HelloPlugin(hi from rust)");
    // Explicit close is optional; Drop does not close. The plugin loader
    // still holds a reference to the api loader object, but we are done.
    api.close().unwrap();
}

#[test]
fn class_loader_missing_class_errors() {
    let Some(java) = jvm() else { return };
    if let Err(reason) = PLUGIN_FIXTURE.get_or_init(compile_plugin_fixture) {
        eprintln!("SKIPPING plugin test: {reason}");
        return;
    }
    let api = java.load_jar("target/rjava-test-api.jar").unwrap();
    let err = api.load_class("no.such.Class").unwrap_err();
    match err {
        JavaError::WithContext { operation, source } => {
            assert_eq!(
                operation, "calling loadClass(String) on java.net.URLClassLoader",
                "operation must name the failed call"
            );
            match *source {
                JavaError::JavaException { class, message } => {
                    assert!(
                        class.contains("ClassNotFound"),
                        "expected a ClassNotFoundException, got {class}"
                    );
                    let _ = message;
                }
                other => panic!("expected a JavaException source, got {other:?}"),
            }
        }
        other => panic!("expected WithContext, got {other:?}"),
    }
}

#[test]
fn class_loader_rejects_empty_paths() {
    let Some(java) = jvm() else { return };
    let err = java.class_loader(&[] as &[&str]).unwrap_err();
    assert!(matches!(err, JavaError::InvalidArgument(_)));
}

#[test]
fn load_class_finds_java_lang_classes() {
    let Some(java) = jvm() else { return };
    if let Err(reason) = PLUGIN_FIXTURE.get_or_init(compile_plugin_fixture) {
        eprintln!("SKIPPING plugin test: {reason}");
        return;
    }
    // URLClassLoader delegates to the platform loader, so java.lang classes
    // resolve through any class loader.
    let api = java.load_jar("target/rjava-test-api.jar").unwrap();
    let s = api.load_class("java.lang.String").unwrap();
    assert_eq!(s.name().unwrap(), "java.lang.String");
}

// ---------------------------------------------------------------------------
// Native methods: Rust functions callable from Java (RegisterNatives)
// ---------------------------------------------------------------------------

// The Rust implementations. Signature: fn(&mut jni::Env, args_tuple) ->
// JavaResult<R> — `env` is the jni Env (mlua's `lua` analog), the tuple
// carries the method's arguments, and R is the single-value ToJava type
// matching the JNI return type.

fn native_add(_env: &mut jni::Env, (a, b): (i32, i32)) -> JavaResult<i32> {
    Ok(a + b)
}

fn native_shout(_env: &mut jni::Env, (s,): (String,)) -> JavaResult<String> {
    Ok(s.to_uppercase())
}

fn native_avg(_env: &mut jni::Env, (xs,): (Vec<f64>,)) -> JavaResult<f64> {
    Ok(xs.iter().sum::<f64>() / xs.len() as f64)
}

fn native_range(_env: &mut jni::Env, (n,): (i32,)) -> JavaResult<Vec<i32>> {
    Ok((0..n).collect())
}

fn native_fail(_env: &mut jni::Env, (msg,): (String,)) -> JavaResult<()> {
    Err(JavaError::JavaException {
        class: "java.lang.IllegalArgumentException".to_string(),
        message: msg,
    })
}

fn native_java_abs(env: &mut jni::Env, (x,): (i32,)) -> JavaResult<i32> {
    // Call back into Java from inside a native method.
    let java = Java::from_env(env)?;
    let abs: i32 = java.class("java.lang.Math")?.call_static("abs", (x,))?;
    Ok(abs)
}

fn native_null_or_value(_env: &mut jni::Env, (give,): (bool,)) -> JavaResult<Option<String>> {
    Ok(if give { Some("x".to_string()) } else { None })
}

/// Concrete-array return: `R = Vec<JObject>` derives `[Ljava/lang/Object;`,
/// and the registration fallback resolves it to `[Ljava/lang/String;`.
fn native_split_csv(env: &mut jni::Env, (s,): (String,)) -> JavaResult<Vec<JObject>> {
    let java = Java::from_env(env)?;
    let mut out = Vec::new();
    for part in s.split(',') {
        out.push(java.new_object("java.lang.String", (part,))?);
    }
    Ok(out)
}

/// `joinCSV(String[])` — the NativeArg direction: a `Vec<String>` parameter
/// derives the exact `([Ljava/lang/String;)Ljava/lang/String;` descriptor.
fn native_join_csv(_env: &mut jni::Env, (xs,): (Vec<String>,)) -> JavaResult<String> {
    Ok(xs.join(","))
}

/// `withNull()` — returns `{"a", null, "b"}`: a `Vec<Option<String>>`
/// return builds a `String[]` whose `None` element becomes Java `null`.
fn native_with_null(_env: &mut jni::Env, (): ()) -> JavaResult<Vec<Option<String>>> {
    Ok(vec![Some("a".to_string()), None, Some("b".to_string())])
}

/// Genuinely `Object`-typed return: `R = JObject` derives the exact
/// `Ljava/lang/Object;`, so the fallback corrects it to the same fragment.
fn native_identity(_env: &mut jni::Env, (o,): (JObject,)) -> JavaResult<JObject> {
    Ok(o)
}

fn native_boom(_env: &mut jni::Env, (): ()) -> JavaResult<i32> {
    panic!("boom went off")
}

// Deliberate mismatch: the JNI signature `(I)I` declares one parameter but
// the Rust function takes two — dispatch must reject it with an arity error.
fn native_arity_mismatch(_env: &mut jni::Env, (_a, _b): (i32, i32)) -> JavaResult<i32> {
    Ok(0)
}

fn native_times(_env: &mut jni::Env, (this, factor): (JObject, i32)) -> JavaResult<i32> {
    let base: i32 = this.get_field("base")?;
    Ok(base * factor)
}

// Two *different* fn items with the same Rust signature `(i64,) -> i64`:
// type-derived registrations of both would share one trampoline, so
// `register_natives` must reject them (see native_shared_trampoline_collision_detected).
// The signature is deliberately one no other test registers — the collision
// check compares against the process-global registry, and tests run in
// parallel.
fn native_plus_one(_env: &mut jni::Env, (x,): (i64,)) -> JavaResult<i64> {
    Ok(x + 1)
}

fn native_plus_two(_env: &mut jni::Env, (x,): (i64,)) -> JavaResult<i64> {
    Ok(x + 2)
}

// ---------------------------------------------------------------------------
// Many-parameter natives (beyond the old 8-parameter ceiling)
// ---------------------------------------------------------------------------

fn native_many10(
    _env: &mut jni::Env,
    (a, b, c, d, e, f, g, h, i, j): (i32, i32, i32, i32, i32, i32, i32, i32, i32, i32),
) -> JavaResult<i64> {
    Ok((a + b + c + d + e + f + g + h + i + j) as i64)
}

/// The 20-`i32` argument tuple shared by the derived and explicit `many20`
/// implementations (clippy's `type_complexity` aside, the fn signatures
/// read better with the alias).
type Many20Args = (
    i32, i32, i32, i32, i32, i32, i32, i32, i32, i32, i32, i32, i32, i32, i32, i32, i32, i32,
    i32, i32,
);

fn native_many20(
    _env: &mut jni::Env,
    (a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q, r, s, t): Many20Args,
) -> JavaResult<i64> {
    Ok((a + b + c + d + e + f + g + h + i + j + k + l + m + n + o + p + q + r + s + t) as i64)
}

/// The explicit-signature twin of [`native_many20`]: same 20-int method, but
/// registered by `native_explicit_many_parameters` with the explicit form and
/// a *distinct* fn that doubles the sum, so the two forms' dispatch is
/// distinguishable end to end.
fn native_many20_explicit(
    _env: &mut jni::Env,
    (a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q, r, s, t): Many20Args,
) -> JavaResult<i64> {
    Ok(2 * (a + b + c + d + e + f + g + h + i + j + k + l + m + n + o + p + q + r + s + t) as i64)
}

/// Mixed types incl. `long`/`double` — validates the unit accounting and
/// float arguments across the FFI boundary on the real ABI.
fn native_many_mix(
    _env: &mut jni::Env,
    (a, b, c, d, e, f, g, h, i, j): (i8, i16, i32, i64, f32, f64, bool, char, String, i64),
) -> JavaResult<i64> {
    Ok(a as i64
        + b as i64
        + c as i64
        + d
        + e as i64
        + f as i64
        + g as i64
        + h as i64
        + i.len() as i64
        + j)
}

/// Serializes the JVM binding of the fixture's `many20` method: both
/// `native_many_parameters` (type-derived) and
/// `native_explicit_many_parameters` (explicit-signature) re-register it
/// under this lock before calling, so each call provably dispatches through
/// the registering test's own trampoline (JNI `RegisterNatives` overrides the
/// previous binding; tests run in parallel).
static MANY20_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

// ---------------------------------------------------------------------------
// Userdata: Rust-backed Java objects (the mlua UserData analog)
// ---------------------------------------------------------------------------

/// The Rust-side state behind a `com.example.Counter` Java shell. The shell
/// carries no data of its own — the registry binds it to this value by the
/// object's identity (`System.identityHashCode`). A `Mutex` makes concurrent
/// increments deterministic.
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

/// Register every native method on the fixture class exactly once per test
/// process.
///
/// Most registrations use the **type-derived** form (`native!("name", f)` —
/// no signature string; closures and fn items both work). Several stay in the
/// explicit-signature form on purpose:
///
/// * `add` — its Rust signature `(i32, i32) -> i32` is the *same* one the
///   closure test registers for `addClosure`; the shared-trampoline rule
///   allows only one derived registration per signature, so `add` keeps its
///   unique explicit trampoline.
/// * `javaAbs` — its `(i32,) -> i32` signature is the same one the closure
///   test registers for `addOffset`, for the same reason.
/// * `arityMismatch` — the fixture declares `(I)I` but the Rust fn takes two
///   args; only the explicit form can declare a "wrong" signature.
/// * `fail` — proof the two forms coexist.
///
/// (The plugin test's `rustEcho` is likewise explicit: its `(String,) ->
/// String` signature would collide with the derived `shout` below.)
static NATIVES_REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();

fn register_natives() -> Result<(), String> {
    NATIVES_REGISTERED
        .get_or_init(|| {
            let java = match jvm() {
                Some(java) => java,
                None => return Err("no JVM available".to_string()),
            };
            let result: JavaResult<()> = (|| {
                let clazz = java.class("com.example.NativeLib")?;
                clazz.register_natives(&[
                    native!("add", "(II)I", native_add)?,
                    native!("shout", native_shout)?,
                    native!("avg", native_avg)?,
                    native!("range", native_range)?,
                    native!("fail", "(Ljava/lang/String;)V", native_fail)?,
                    native!("javaAbs", "(I)I", native_java_abs)?,
                    native!("nullOrValue", native_null_or_value)?,
                    native!("boom", native_boom)?,
                    native!("arityMismatch", "(I)I", native_arity_mismatch)?,
                    // Reference-typed arrays: Vec<String> ⇄ String[] both
                    // ways (joinCSV = argument, withNull = return).
                    native!("joinCSV", native_join_csv)?,
                    native!("withNull", native_with_null)?,
                    native_inst!("times", native_times)?,
                    // Type-derived instance registration with a closure.
                    native_inst!("timesLong", |env, (this, factor): (JObject, i32)| -> JavaResult<i64> {
                        let _ = env;
                        let base: i32 = this.get_field("base")?;
                        Ok((base * factor) as i64)
                    })?,
                    // Beyond-8-parameter natives, type-derived (fn items).
                    // `many20` is deliberately absent: it is registered by
                    // the two many-parameter tests themselves, under
                    // MANY20_LOCK, in both the derived and explicit forms.
                    native!("many10", native_many10)?,
                    native!("manyMix", native_many_mix)?,
                ])?;
                // The userdata fixture. `increment`/`value` are instance
                // natives: the receiver JObject is stripped from the JNI
                // descriptor, so their derived signatures `(J)J` / `()J`
                // match the Java declarations exactly. `create` returns a
                // concrete class (`Counter`): the type-derived form derives
                // `()Ljava/lang/Object;` (R = JObject), and
                // `register_natives` now resolves the exact return type via
                // reflection at registration time — the explicit-signature
                // workaround is gone.
                let counter = java.class("com.example.Counter")?;
                counter.register_natives(&[
                    native!("create", counter_create)?,
                    native_inst!("increment", counter_increment)?,
                    native_inst!("value", counter_value)?,
                ])?;
                Ok(())
            })();
            result.map_err(|e| e.to_string())
        })
        .clone()
}

#[test]
fn native_static_methods() {
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING native test: {reason}");
        return;
    }
    let clazz = java.class("com.example.NativeLib").unwrap();
    let sum: i32 = clazz.call_static("add", (2_i32, 3_i32)).unwrap();
    assert_eq!(sum, 5);
    let s: String = clazz.call_static("shout", ("hi",)).unwrap();
    assert_eq!(s, "HI");
    let avg: f64 = clazz
        .call_static("avg", (vec![1.0_f64, 2.0, 3.0],))
        .unwrap();
    assert!((avg - 2.0).abs() < 1e-12);
    let range: Vec<i32> = clazz.call_static("range", (4_i32,)).unwrap();
    assert_eq!(range, vec![0, 1, 2, 3]);
}

#[test]
fn native_instance_method() {
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING native test: {reason}");
        return;
    }
    let clazz = java.class("com.example.NativeLib").unwrap();
    let obj = clazz.new_instance(()).unwrap();
    obj.set_field("base", 10).unwrap();
    let r: i32 = obj.call("times", (2_i32,)).unwrap();
    assert_eq!(r, 20);
}

#[test]
fn native_exceptions() {
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING native test: {reason}");
        return;
    }
    let clazz = java.class("com.example.NativeLib").unwrap();
    let err = clazz.call_static::<_, ()>("fail", ("boom",)).unwrap_err();
    match err {
        JavaError::WithContext { operation, source } => {
            assert_eq!(
                operation, "calling fail(String) on com.example.NativeLib",
                "operation must name the failed call"
            );
            match *source {
                JavaError::JavaException { class, message } => {
                    assert_eq!(class, "java.lang.IllegalArgumentException");
                    assert!(message.contains("boom"), "message was {message:?}");
                }
                other => panic!("expected a JavaException source, got {other:?}"),
            }
        }
        other => panic!("expected WithContext, got {other:?}"),
    }
    // A failed native call must not poison the thread.
    let sum: i32 = clazz.call_static("add", (1_i32, 1_i32)).unwrap();
    assert_eq!(sum, 2);
}

#[test]
fn native_callbacks_into_java() {
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING native test: {reason}");
        return;
    }
    let clazz = java.class("com.example.NativeLib").unwrap();
    let abs: i32 = clazz.call_static("javaAbs", (-42_i32,)).unwrap();
    assert_eq!(abs, 42);
}

#[test]
fn native_null_returns() {
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING native test: {reason}");
        return;
    }
    let clazz = java.class("com.example.NativeLib").unwrap();
    let none: Option<String> = clazz.call_static("nullOrValue", (false,)).unwrap();
    assert_eq!(none, None);
    let some: Option<String> = clazz.call_static("nullOrValue", (true,)).unwrap();
    assert_eq!(some.as_deref(), Some("x"));
    let direct: String = clazz.call_static("nullOrValue", (true,)).unwrap();
    assert_eq!(direct, "x");
}

#[test]
fn native_arity_mismatch_is_rejected() {
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING native test: {reason}");
        return;
    }
    let clazz = java.class("com.example.NativeLib").unwrap();
    // `arityMismatch` is registered with the signature `(I)I` but its Rust
    // function takes a two-element tuple; dispatch rejects the call with a
    // clear arity error that surfaces as a Java RuntimeException.
    let err = clazz
        .call_static::<_, i32>("arityMismatch", (5_i32,))
        .unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("arity") || text.contains("argument"),
        "unexpected error: {text}"
    );
}

#[test]
fn native_panics_become_java_exceptions() {
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING native test: {reason}");
        return;
    }
    let clazz = java.class("com.example.NativeLib").unwrap();
    let err = clazz.call_static::<_, i32>("boom", ()).unwrap_err();
    match err {
        JavaError::WithContext { operation, source } => {
            assert_eq!(
                operation, "calling boom() on com.example.NativeLib",
                "operation must name the failed call"
            );
            match *source {
                JavaError::JavaException { class, message } => {
                    assert_eq!(class, "java.lang.RuntimeException");
                    assert!(
                        message.contains("panicked") && message.contains("boom went off"),
                        "message was {message:?}"
                    );
                }
                other => panic!("expected a JavaException source, got {other:?}"),
            }
        }
        other => panic!("expected WithContext, got {other:?}"),
    }
    // The panic was caught at the FFI boundary; the thread is still usable.
    let sum: i32 = clazz.call_static("add", (2_i32, 2_i32)).unwrap();
    assert_eq!(sum, 4);
}

#[test]
fn native_type_derived_closures() {
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING native test: {reason}");
        return;
    }
    let clazz = java.class("com.example.NativeLib").unwrap();
    // The headline form: a plain (non-capturing) closure, no signature
    // string. (Registered here rather than in the shared setup because its
    // `(i32, i32) -> i32` signature is the same as `add`'s, which is
    // registered explicit — the shared-trampoline rule allows only one
    // derived registration per signature.)
    let method = native!(
        "addClosure",
        |env, (a, b): (i32, i32)| -> rjava::JavaResult<i32> {
            let _ = env;
            Ok(a + b)
        }
    )
    .unwrap();
    // A CAPTURING closure: `offset` lives on the stack of this test and is
    // moved into the closure (Send + Sync + 'static).
    let offset = 100;
    let method_offset = native!(
        "addOffset",
        move |env, (x,): (i32,)| -> rjava::JavaResult<i32> {
            let _ = env;
            Ok(x + offset)
        }
    )
    .unwrap();
    clazz.register_natives(&[method, method_offset]).unwrap();
    let sum: i32 = clazz.call_static("addClosure", (20_i32, 22_i32)).unwrap();
    assert_eq!(sum, 42);
    let shifted: i32 = clazz.call_static("addOffset", (5_i32,)).unwrap();
    assert_eq!(shifted, 105);
}

#[test]
fn native_type_derived_strings_and_arrays() {
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING native test: {reason}");
        return;
    }
    let clazz = java.class("com.example.NativeLib").unwrap();
    // String param + String return (shout), Vec<f64> param (avg),
    // Vec<i32> return (range) — all registered type-derived (no signature
    // string) in the shared setup.
    let s: String = clazz.call_static("shout", ("type-derived",)).unwrap();
    assert_eq!(s, "TYPE-DERIVED");
    let avg: f64 = clazz
        .call_static("avg", (vec![1.0_f64, 2.0, 3.0, 4.0],))
        .unwrap();
    assert!((avg - 2.5).abs() < 1e-12);
    let range: Vec<i32> = clazz.call_static("range", (3_i32,)).unwrap();
    assert_eq!(range, vec![0, 1, 2]);
}

#[test]
fn vec_string_read() {
    let Some(java) = jvm() else { return };
    if let Err(reason) = compile_fixture() {
        eprintln!("SKIPPING native test: {reason}");
        return;
    }
    let clazz = java.class("com.example.NativeLib").unwrap();
    // `splitCSV` returns `String[]` but is registered type-derived with
    // `R = Vec<JObject>`, which derives `[Ljava/lang/Object;`. The first
    // RegisterNatives attempt fails with NoSuchMethodError; the fallback
    // resolves the exact return type `[Ljava/lang/String;` via reflection
    // and the retry succeeds.
    let method = native!("splitCSV", native_split_csv).unwrap();
    clazz.register_natives(&[method]).unwrap();
    // The headline proof: `String[]` reads back as `Vec<String>` directly —
    // no element-wise `JObject` + `toString` conversion.
    let parts: Vec<String> = clazz.call_static("splitCSV", ("a,b,c",)).unwrap();
    assert_eq!(parts, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    // The `Vec<JObject>` annotation still works on the same method — both
    // families share the unified `Vec<T>` machinery (JObject elements read
    // from the very same `String[]`).
    let parts: Vec<JObject> = clazz.call_static("splitCSV", ("a,b,c",)).unwrap();
    let parts: Vec<String> = parts.iter().map(|p| p.to_string().unwrap()).collect();
    assert_eq!(parts, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
}

#[test]
fn vec_string_write_and_call() {
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING native test: {reason}");
        return;
    }
    let clazz = java.class("com.example.NativeLib").unwrap();
    // `Vec<String>` → `String[]` argument: the derived signature
    // `([Ljava/lang/String;)Ljava/lang/String;` is exact, so the call
    // resolves without the reflection fallback and the native joins.
    let joined: String = clazz
        .call_static("joinCSV", (vec!["a".to_string(), "b".to_string(), "c".to_string()],))
        .unwrap();
    assert_eq!(joined, "a,b,c");
}

#[test]
fn vec_option_string_nulls() {
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING native test: {reason}");
        return;
    }
    let clazz = java.class("com.example.NativeLib").unwrap();
    // `withNull()` returns `{"a", null, "b"}`: read as `Vec<Option<String>>`
    // the null element becomes `None` (the underlying array is still
    // `String[]` — Option is only a null-tolerance wrapper).
    let arr: Vec<Option<String>> = clazz.call_static("withNull", ()).unwrap();
    assert_eq!(arr, vec![Some("a".to_string()), None, Some("b".to_string())]);
    // The same array read as `Vec<String>` rejects the null element via the
    // plain `String` conversion's null error. The failure names the
    // operation and keeps the underlying InvalidArgument.
    let err = clazz.call_static::<_, Vec<String>>("withNull", ()).unwrap_err();
    match err {
        JavaError::WithContext { operation, source } => {
            assert_eq!(
                operation, "calling withNull() on com.example.NativeLib",
                "operation must name the failed call"
            );
            assert!(
                matches!(*source, JavaError::InvalidArgument(_)),
                "expected InvalidArgument on a null String[] element, got {source:?}"
            );
        }
        other => panic!("expected WithContext, got {other:?}"),
    }
}

#[test]
fn native_derived_generic_object_return_unchanged() {
    let Some(java) = jvm() else { return };
    if let Err(reason) = compile_fixture() {
        eprintln!("SKIPPING native test: {reason}");
        return;
    }
    let clazz = java.class("com.example.NativeLib").unwrap();
    // `identity` genuinely returns `Object`: the derived descriptor
    // `(Ljava/lang/Object;)Ljava/lang/Object;` is exact, so the fallback
    // resolves it to the same fragment and the call round-trips the object.
    let method = native!("identity", native_identity).unwrap();
    clazz.register_natives(&[method]).unwrap();
    let obj = java.new_object("java.lang.String", ("hello",)).unwrap();
    let back: JObject = clazz.call_static("identity", (obj.clone(),)).unwrap();
    let same: bool = obj.call("equals", (back,)).unwrap();
    assert!(same, "identity must return the same object");
}

#[test]
fn native_wrong_explicit_sig_still_errors() {
    let Some(java) = jvm() else { return };
    if let Err(reason) = compile_fixture() {
        eprintln!("SKIPPING native test: {reason}");
        return;
    }
    let clazz = java.class("com.example.Counter").unwrap();
    // The explicit-signature form with a WRONG return type: `create` returns
    // `Counter`, the descriptor claims `String`. The fallback never touches
    // explicit-form descriptors (`call: None`), so registration still fails
    // with NoSuchMethodError — and fails at registration time, not call time
    // (nothing gets registered: RegisterNatives is all-or-nothing).
    let method = native!("create", "()Ljava/lang/String;", counter_create).unwrap();
    let err = clazz.register_natives(&[method]).unwrap_err();
    // The mismatch surfaces as a NoSuchMethodError: either the raw JNI error
    // the `jni` crate returns for RegisterNatives failures
    // (`Error::NoSuchMethod` — it catches + clears the JVM exception itself),
    // or a captured JavaException on other paths. The explicit form is never
    // auto-corrected, so this must not resolve to the real descriptor.
    let is_nosuchmethod = match &err {
        JavaError::Jni(jni::errors::Error::NoSuchMethod(_)) => true,
        JavaError::JavaException { class, .. } => class.contains("NoSuchMethodError"),
        _ => false,
    };
    assert!(is_nosuchmethod, "expected NoSuchMethodError, got {err:?}");
}

#[test]
fn native_shared_trampoline_collision_detected() {
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING native test: {reason}");
        return;
    }
    let clazz = java.class("com.example.NativeLib").unwrap();
    // Two different fn items with the same Rust signature `(i64,) -> i64`
    // map to the *same* shared trampoline; registering both must be rejected
    // before any JVM mutation (the fixture even lacks these method names).
    let a = native!("collisionA", native_plus_one).unwrap();
    let b = native!("collisionB", native_plus_two).unwrap();
    let err = clazz.register_natives(&[a, b]).unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("signature") || text.contains("trampoline"),
        "unexpected error: {text}"
    );
}

#[test]
fn native_type_derived_instance() {
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING native test: {reason}");
        return;
    }
    let clazz = java.class("com.example.NativeLib").unwrap();
    // `timesLong` is registered type-derived with a closure in the shared
    // setup: `native_inst!("timesLong", |env, (this, factor): (JObject, i32)|
    // -> JavaResult<i64> …)` — the receiver is the first tuple element, the
    // JNI signature `(I)J` is derived from the Rust types.
    let obj = clazz.new_instance(()).unwrap();
    obj.set_field("base", 7).unwrap();
    let r: i64 = obj.call("timesLong", (3_i32,)).unwrap();
    assert_eq!(r, 21);
}

#[test]
fn native_many_parameters() {
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING native test: {reason}");
        return;
    }
    let clazz = java.class("com.example.NativeLib").unwrap();
    // `many20` is re-registered type-derived here (the shared setup registers
    // `many10` and `manyMix`); the lock serializes the JVM binding with the
    // explicit-signature registration in `native_explicit_many_parameters`.
    let _guard = MANY20_LOCK.lock();
    let method = native!("many20", native_many20).unwrap();
    clazz.register_natives(&[method]).unwrap();
    // A 10-tuple through the normal call machinery (many10 is registered
    // type-derived in the shared setup) and a 20-tuple for many20 — the
    // 20-tuple exercises the extended ToJava argument tuples (beyond 12).
    let sum: i64 = clazz
        .call_static("many10", (1_i32, 2, 3, 4, 5, 6, 7, 8, 9, 10))
        .unwrap();
    assert_eq!(sum, 55);
    let sum: i64 = clazz
        .call_static(
            "many20",
            (1_i32, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20),
        )
        .unwrap();
    assert_eq!(sum, 210);
    // Mixed types incl. long/double: 1+2+3+4+5+6+1+65+2+10 = 99. Exercises
    // float args across the FFI boundary and the explicit-signature unit
    // accounting (13 units) via the derived descriptor.
    let sum: i64 = clazz
        .call_static(
            "manyMix",
            (1_i8, 2_i16, 3_i32, 4_i64, 5.0_f32, 6.0_f64, true, 'A', "hi", 10_i64),
        )
        .unwrap();
    assert_eq!(sum, 99);
}

#[test]
fn native_explicit_many_parameters() {
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING native test: {reason}");
        return;
    }
    let clazz = java.class("com.example.NativeLib").unwrap();
    let _guard = MANY20_LOCK.lock();
    // The explicit-signature form at 20 parameters: the per-call trampoline
    // is generated from the parsed sig `(IIIIIIIIIIIIIIIIIIII)J` and
    // dispatches to the *distinct* fn (which doubles the sum) — proving the
    // raised explicit-form cap end to end.
    let method = native!("many20", "(IIIIIIIIIIIIIIIIIIII)J", native_many20_explicit).unwrap();
    clazz.register_natives(&[method]).unwrap();
    let sum: i64 = clazz
        .call_static(
            "many20",
            (1_i32, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20),
        )
        .unwrap();
    assert_eq!(sum, 420);
}

// ---------------------------------------------------------------------------
// Userdata: Rust-backed Java objects (the mlua UserData analog)
// ---------------------------------------------------------------------------

/// Run `f` with the current thread attached, handing it a `&mut Env`. The
/// userdata API takes an `Env`; tests reach one through the `jni` crate's
/// process-wide JVM singleton (the same one `jvm()` created).
fn with_env<T>(f: impl FnOnce(&mut jni::Env<'_>) -> JavaResult<T>) -> JavaResult<T> {
    let vm = rjava::jni::JavaVM::singleton().map_err(JavaError::from)?;
    vm.attach_current_thread::<_, T, JavaError>(f)
}

#[test]
fn userdata_round_trip() {
    let _userdata_guard = USERDATA_LOCK.lock();
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING userdata test: {reason}");
        return;
    }
    let clazz = java.class("com.example.Counter").unwrap();
    let counter: JObject = clazz.call_static("create", ()).unwrap();
    // The state lives in Rust: the Java shell carries nothing of its own, so
    // the count persists across separate native calls.
    let v: i64 = counter.call("increment", (5_i64,)).unwrap();
    assert_eq!(v, 5);
    let v: i64 = counter.call("increment", (3_i64,)).unwrap();
    assert_eq!(v, 8);
    let v: i64 = counter.call("value", ()).unwrap();
    assert_eq!(v, 8);
}

#[test]
fn userdata_independent_objects() {
    let _userdata_guard = USERDATA_LOCK.lock();
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING userdata test: {reason}");
        return;
    }
    let clazz = java.class("com.example.Counter").unwrap();
    let a: JObject = clazz.call_static("create", ()).unwrap();
    let b: JObject = clazz.call_static("create", ()).unwrap();
    // Distinct shells bind distinct state: no cross-talk.
    let _: i64 = a.call("increment", (10_i64,)).unwrap();
    let _: i64 = b.call("increment", (99_i64,)).unwrap();
    let va: i64 = a.call("value", ()).unwrap();
    let vb: i64 = b.call("value", ()).unwrap();
    assert_eq!((va, vb), (10, 99));
}

#[test]
fn userdata_type_mismatch() {
    let _userdata_guard = USERDATA_LOCK.lock();
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING userdata test: {reason}");
        return;
    }
    let clazz = java.class("com.example.Counter").unwrap();
    let counter: JObject = clazz.call_static("create", ()).unwrap();
    // Bound to Counter, requested as String -> InvalidArgument.
    let err = with_env(|env| rjava::userdata::get::<String>(env, &counter)).unwrap_err();
    assert!(
        matches!(err, JavaError::InvalidArgument(_)),
        "expected InvalidArgument, got {err:?}"
    );
    // A fresh, never-bound shell -> InvalidArgument (the "missing key" case:
    // there is no key — unbound objects simply have no registry entry).
    let fresh = clazz.new_instance(()).unwrap();
    let err = with_env(|env| rjava::userdata::get::<Counter>(env, &fresh)).unwrap_err();
    assert!(
        matches!(err, JavaError::InvalidArgument(_)),
        "expected InvalidArgument, got {err:?}"
    );
}

#[test]
fn userdata_threads() {
    let _userdata_guard = USERDATA_LOCK.lock();
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING userdata test: {reason}");
        return;
    }
    let clazz = java.class("com.example.Counter").unwrap();
    let counter: JObject = clazz.call_static("create", ()).unwrap();
    // JObject handles are Send + Sync (global refs) and native calls attach
    // the calling thread automatically, so plain std::thread works. Both
    // threads resolve the SAME Arc<Counter> from the registry; its Mutex
    // serializes the increments, making the total deterministic.
    let t1 = {
        let c = counter.clone();
        std::thread::spawn(move || {
            for _ in 0..5 {
                let _: i64 = c.call("increment", (1_i64,)).unwrap();
            }
        })
    };
    let t2 = {
        let c = counter.clone();
        std::thread::spawn(move || {
            for _ in 0..5 {
                let _: i64 = c.call("increment", (1_i64,)).unwrap();
            }
        })
    };
    t1.join().unwrap();
    t2.join().unwrap();
    let v: i64 = counter.call("value", ()).unwrap();
    assert_eq!(v, 10);
}

#[test]
fn userdata_unbind() {
    let _userdata_guard = USERDATA_LOCK.lock();
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING userdata test: {reason}");
        return;
    }
    let clazz = java.class("com.example.Counter").unwrap();
    let counter: JObject = clazz.call_static("create", ()).unwrap();
    let bound = with_env(|env| Ok(rjava::userdata::unbind(env, &counter))).unwrap();
    assert!(bound, "create() must leave the shell bound");
    // Second unbind: nothing left to remove.
    let again = with_env(|env| Ok(rjava::userdata::unbind(env, &counter))).unwrap();
    assert!(!again);
    // The state is gone, so the native now throws a Java exception; the
    // failure names the operation.
    let err = counter.call::<_, i64>("value", ()).unwrap_err();
    match err {
        JavaError::WithContext { operation, source } => {
            assert_eq!(
                operation, "calling value() on com.example.Counter",
                "operation must name the failed call"
            );
            assert!(matches!(*source, JavaError::JavaException { .. }));
        }
        other => panic!("expected WithContext, got {other:?}"),
    }
}

#[test]
fn userdata_rebind_same_object() {
    let _userdata_guard = USERDATA_LOCK.lock();
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING userdata test: {reason}");
        return;
    }
    let clazz = java.class("com.example.Counter").unwrap();
    let counter: JObject = clazz.call_static("create", ()).unwrap();
    let _: i64 = counter.call("increment", (5_i64,)).unwrap();
    // Re-binding the SAME object is allowed, and the new state wins.
    with_env(|env| rjava::userdata::bind(env, &counter, Counter::new())).unwrap();
    let v: i64 = counter.call("value", ()).unwrap();
    assert_eq!(v, 0, "re-binding must replace the old state");
}

#[test]
fn userdata_java_constructed_shell() {
    let _userdata_guard = USERDATA_LOCK.lock();
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING userdata test: {reason}");
        return;
    }
    let clazz = java.class("com.example.Counter").unwrap();
    // Plain-Java construction: `new Counter()` — no factory, no natives
    // involved. The host binds state to the shell afterwards.
    let shell = clazz.new_instance(()).unwrap();
    // Unbound: calling a method throws before bind; the failure names the
    // operation.
    let err = shell.call::<_, i64>("value", ()).unwrap_err();
    match err {
        JavaError::WithContext { operation, source } => {
            assert_eq!(
                operation, "calling value() on com.example.Counter",
                "operation must name the failed call"
            );
            assert!(matches!(*source, JavaError::JavaException { .. }));
        }
        other => panic!("expected WithContext, got {other:?}"),
    }
    with_env(|env| rjava::userdata::bind(env, &shell, Counter::new())).unwrap();
    let v: i64 = shell.call("increment", (7_i64,)).unwrap();
    assert_eq!(v, 7);
    let v: i64 = shell.call("value", ()).unwrap();
    assert_eq!(v, 7);
}

#[test]
fn userdata_with() {
    let _userdata_guard = USERDATA_LOCK.lock();
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING userdata test: {reason}");
        return;
    }
    let clazz = java.class("com.example.Counter").unwrap();
    let counter: JObject = clazz.call_static("create", ()).unwrap();
    // `with` is `get` + a closure: fetch the state, borrow it as `&T`, run
    // the closure, return its result.
    let v = with_env(|env| rjava::userdata::with::<Counter, _>(env, &counter, |c| c.value()))
        .unwrap();
    assert_eq!(v, 0);
    // The closure may mutate interior state through the same `&T` `get`
    // hands out (the crate's synchronization-inside-the-state pattern).
    let v =
        with_env(|env| rjava::userdata::with::<Counter, _>(env, &counter, |c| c.increment(5)))
            .unwrap();
    assert_eq!(v, 5);
    let v = with_env(|env| rjava::userdata::with::<Counter, _>(env, &counter, |c| c.value()))
        .unwrap();
    assert_eq!(v, 5);
    // Missing state fails exactly like `get`: the same InvalidArgument.
    let fresh = clazz.new_instance(()).unwrap();
    let err = with_env(|env| rjava::userdata::with::<Counter, _>(env, &fresh, |c| c.value()))
        .unwrap_err();
    assert!(
        matches!(err, JavaError::InvalidArgument(_)),
        "expected InvalidArgument, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Userdata auto-release (P0-3): weak bindings + the background cleaner
// ---------------------------------------------------------------------------

/// Serializes the userdata tests. The auto-release tests assert on
/// [`rjava::userdata::live_bindings`] counts, which change when *any*
/// userdata binding is created or released; without the lock a parallel
/// userdata test could finish mid-loop and its shell get collected and
/// drained, moving the count under the assertion. (The cleaner thread is
/// global and outside this lock; with the lock, no other *userdata* binding
/// exists or is created while one userdata test runs.)
static USERDATA_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

#[test]
fn userdata_auto_release_on_gc() {
    let _userdata_guard = USERDATA_LOCK.lock();
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING userdata test: {reason}");
        return;
    }
    let clazz = java.class("com.example.Counter").unwrap();
    let system = java.class("java.lang.System").unwrap();

    // Flush dead bindings left over from earlier userdata tests: force a GC,
    // then wait out a full cleaner tick (500 ms) so the baseline is stable.
    system.call_static::<_, ()>("gc", ()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(700));
    let baseline = rjava::userdata::live_bindings();

    // Bind a fresh shell (`create` → create_shell) and drop every Rust
    // handle to it: the shell is now unreachable, so its binding — and the
    // Rust state — must be released automatically.
    let shell: JObject = clazz.call_static("create", ()).unwrap();
    assert_eq!(
        rjava::userdata::live_bindings(),
        baseline + 1,
        "the fresh shell's bind must be registered"
    );
    drop(shell);

    // Generous retries: `System.gc()` on HotSpot collects the unreachable
    // shell, the weak ref stops resolving, and the cleaner thread (or the
    // lazy drain) removes the binding. If it never drops, the contract is
    // broken and the test FAILS.
    let mut released = false;
    for _ in 0..100 {
        system.call_static::<_, ()>("gc", ()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        if rjava::userdata::live_bindings() == baseline {
            released = true;
            break;
        }
    }
    assert!(
        released,
        "auto-release contract violated: the unreachable shell's binding must \
         be released by GC (live_bindings() did not return to baseline {baseline})"
    );
    assert_eq!(
        rjava::userdata::live_bindings(),
        baseline,
        "no other binding may have been released (or left) besides ours"
    );
}

#[test]
fn userdata_auto_release_keeps_live_shells() {
    let _userdata_guard = USERDATA_LOCK.lock();
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING userdata test: {reason}");
        return;
    }
    let clazz = java.class("com.example.Counter").unwrap();
    let system = java.class("java.lang.System").unwrap();

    // Flush leftovers first (see the GC test above) so `before` is stable.
    system.call_static::<_, ()>("gc", ()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(700));

    // A shell with a live Rust handle must survive GC: the weak ref keeps
    // resolving, so the binding is not dead and is never released.
    let shell: JObject = clazz.call_static("create", ()).unwrap();
    let before = rjava::userdata::live_bindings();
    for _ in 0..20 {
        system.call_static::<_, ()>("gc", ()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(
        rjava::userdata::live_bindings(),
        before,
        "a shell with a live Rust handle must not be auto-released"
    );
    // And the binding still works end to end (existing round-trip behavior).
    let v: i64 = shell.call("increment", (3_i64,)).unwrap();
    assert_eq!(v, 3);
}
