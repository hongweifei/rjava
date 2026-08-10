#![forbid(unsafe_code)]
//! Integration tests for the optional `serde` feature (`rjava::serde`):
//! Rust structs ⇄ `java.util.HashMap` / `java.util.ArrayList` value trees.
//!
//! Only JDK classes are used — no fixtures need compiling. The `jvm()`
//! helper builds the JVM once per test process; if no JVM can be located
//! every test skips (the same convention as `tests/integration.rs`).
//!
//! The `#![forbid(unsafe_code)]` on the first line matches the main test
//! suite: the feature itself introduces no `unsafe`.

use std::sync::LazyLock;

use rjava::bean::JavaBean;
use rjava::prelude::*;
use rjava::serde::{from_object, JavaMap};
use serde::{Deserialize, Serialize};

/// Build the JVM once per test process; `None` (after an `eprintln`) if no
/// JVM is available so tests can skip instead of failing on JVM-less
/// machines.
static JVM: LazyLock<Option<Java>> = LazyLock::new(|| match Java::builder().build() {
    Ok(java) => Some(java),
    Err(e) => {
        eprintln!("SKIPPING test: no JVM available: {e}");
        None
    }
});

fn jvm() -> Option<Java> {
    JVM.clone()
}

/// Run `f` with the current thread attached, handing it a `&mut Env` — the
/// `from_object` entry takes an `Env` (mirroring the userdata tests in
/// `tests/integration.rs`).
fn with_env<T>(f: impl FnOnce(&mut jni::Env<'_>) -> JavaResult<T>) -> JavaResult<T> {
    let vm = rjava::jni::JavaVM::singleton().map_err(JavaError::from)?;
    vm.attach_current_thread::<_, T, JavaError>(f)
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct RoundTrip {
    name: String,
    count: i64,
    ratio: f64,
    tags: Vec<String>,
    maybe: Option<i32>,
    flag: bool,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Item {
    id: i32,
    label: String,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Nested {
    name: String,
    items: Vec<Item>,
    matrix: Option<Vec<i64>>,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Sparse {
    a: Option<i32>,
    b: Option<String>,
    c: i32,
}

/// Every primitive the serde value tree supports, one field each — the
/// wrapper-class round-trip audit (boxed as the exact `ToJava` wrapper, read
/// back range-checked by serde).
#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Primitives {
    small: i8,
    wide: i16,
    tall: i32,
    big: i64,
    tiny: u8,
    frac: f32,
    precise: f64,
    yes: bool,
    letter: char,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Matrix {
    rows: Vec<Vec<i32>>,
}

/// A struct with a `JavaBean<T>` field travelling through the **value**
/// tree: serializing writes the reserved `__rjava_bean_class` marker map,
/// reading unwraps it back into the plain struct. The marker's class string
/// is write-side metadata only, so no fixture class is needed here. (No
/// `PartialEq`/`Debug` derive — `JavaBean<T>` has neither; the test
/// compares the inner values by hand.)
#[derive(Serialize, Deserialize)]
struct WithBean {
    name: String,
    inner: JavaBean<Inner>,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Inner {
    id: i32,
    label: String,
}

#[test]
fn serde_round_trip_via_map() {
    let Some(java) = jvm() else { return };
    let v = RoundTrip {
        name: "Ada".to_string(),
        count: 42,
        ratio: 3.25,
        tags: vec!["a".to_string(), "b".to_string()],
        maybe: Some(7),
        flag: true,
    };
    // Serialize the struct into a java.util.HashMap via `putAll(Map)`.
    let hm = java.new_object("java.util.HashMap", ()).unwrap();
    hm.call_void("putAll", (JavaMap(&v),)).unwrap();

    // The map holds one entry per field…
    let size: i32 = hm.call("size", ()).unwrap();
    assert_eq!(size, 6);
    // …and `name` came through as a Java String.
    let name: String = hm.call("get", ("name",)).unwrap();
    assert_eq!(name, "Ada");

    // Read the whole map back into the struct.
    let got: RoundTrip = with_env(|env| from_object(env, &hm)).unwrap();
    assert_eq!(got, v);
}

#[test]
fn serde_nested_and_sequences() {
    let Some(java) = jvm() else { return };
    let v = Nested {
        name: "root".to_string(),
        items: vec![
            Item {
                id: 1,
                label: "one".to_string(),
            },
            Item {
                id: 2,
                label: "two".to_string(),
            },
        ],
        matrix: Some(vec![1_i64, 2, 3]),
    };
    let hm = java.new_object("java.util.HashMap", ()).unwrap();
    hm.call_void("putAll", (JavaMap(&v),)).unwrap();

    let got: Nested = with_env(|env| from_object(env, &hm)).unwrap();
    assert_eq!(got, v);
}

#[test]
fn serde_option_none_is_null() {
    let Some(java) = jvm() else { return };
    let v = Sparse {
        a: None,
        b: Some("present".to_string()),
        c: 5,
    };
    let hm = java.new_object("java.util.HashMap", ()).unwrap();
    hm.call_void("putAll", (JavaMap(&v),)).unwrap();

    // `None` fields land in the map as Java null…
    let a: Option<JObject> = hm.call("get", ("a",)).unwrap();
    assert!(a.is_none(), "`a` (None) must be stored as a Java null entry");
    let b: String = hm.call("get", ("b",)).unwrap();
    assert_eq!(b, "present");

    // …and reading the map back restores `None`.
    let got: Sparse = with_env(|env| from_object(env, &hm)).unwrap();
    assert_eq!(got, v);
}

#[test]
fn serde_unsupported_value_errors() {
    let Some(java) = jvm() else { return };
    let v = RoundTrip {
        name: "Ada".to_string(),
        count: 42,
        ratio: 3.25,
        tags: vec![],
        maybe: None,
        flag: true,
    };
    let hm = java.new_object("java.util.HashMap", ()).unwrap();
    hm.call_void("putAll", (JavaMap(&v),)).unwrap();
    // Replace the `count` value with a java.util.Date — not a serde-mappable
    // Java value — and verify the error names it.
    let date = java.new_object("java.util.Date", ()).unwrap();
    hm.call_void("put", ("count", &date)).unwrap();

    let err = with_env(|env| from_object::<RoundTrip>(env, &hm)).unwrap_err();
    assert!(
        matches!(err, JavaError::Serde(_)),
        "expected JavaError::Serde, got {err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("java.util.Date"),
        "the error must name the offending Java class: {msg}"
    );
}

#[test]
fn serde_null_unit() {
    let Some(java) = jvm() else { return };
    // `()` follows the crate's "call it and discard" semantics (`FromJava
    // for ()` ignores the value): any Java value reads back as `()`. A
    // direct `()`-from-null check is unreachable through the public API —
    // the crate's `JObject` handle always wraps a live object, so Java null
    // only appears as a map/list element (where it flows through
    // `Option<T>`, see serde_option_none_is_null). This test pins the
    // discard behavior instead.
    let hm = java.new_object("java.util.HashMap", ()).unwrap();
    hm.call_void("put", ("k", 1_i32)).unwrap();

    let unit: () = with_env(|env| from_object(env, &hm)).unwrap();
    assert_eq!(unit, ());
}

#[test]
fn serde_all_primitives_round_trip() {
    let Some(java) = jvm() else { return };
    // The full supported primitive set — including `i8`/`i16`/`f32` (boxed
    // as their exact `ToJava` wrappers Byte/Short/Float) and `char` (boxed
    // as `java.lang.Character`). Reading back range-checks each value into
    // the annotated Rust type.
    let v = Primitives {
        small: -5,
        wide: 300,
        tall: 1_000_000,
        big: 9_000_000_000,
        tiny: 200, // ≥ 128: the bit pattern is a negative Java byte; u8 reads it back unsigned
        frac: 0.5,
        precise: 2.5,
        yes: true,
        letter: 'A',
    };
    let hm = java.new_object("java.util.HashMap", ()).unwrap();
    hm.call_void("putAll", (JavaMap(&v),)).unwrap();

    // The `letter` entry is a boxed `java.lang.Character`, not a String.
    let letter: JObject = hm.call("get", ("letter",)).unwrap();
    let cls: JObject = letter.call("getClass", ()).unwrap();
    let cls_name: String = cls.call("getName", ()).unwrap();
    assert_eq!(cls_name, "java.lang.Character");

    let got: Primitives = with_env(|env| from_object(env, &hm)).unwrap();
    assert_eq!(got, v);
}

#[test]
fn serde_nested_sequences() {
    let Some(java) = jvm() else { return };
    // `Vec<Vec<T>>` → `ArrayList` of `ArrayList`s — both directions flow
    // through the same sequence machinery.
    let v = Matrix {
        rows: vec![vec![1, 2], vec![3, 4, 5]],
    };
    let hm = java.new_object("java.util.HashMap", ()).unwrap();
    hm.call_void("putAll", (JavaMap(&v),)).unwrap();

    let rows: JObject = hm.call("get", ("rows",)).unwrap();
    let size: i32 = rows.call("size", ()).unwrap();
    assert_eq!(size, 2);
    let first: JObject = rows.call("get", (0,)).unwrap();
    let first_size: i32 = first.call("size", ()).unwrap();
    assert_eq!(first_size, 2);

    let got: Matrix = with_env(|env| from_object(env, &hm)).unwrap();
    assert_eq!(got, v);
}

#[test]
fn serde_map_with_bean_field_stays_sane() {
    let Some(java) = jvm() else { return };
    // A struct with a `JavaBean<T>` field serialized through the value tree:
    // the nested bean lands as an ordinary nested HashMap with the two
    // reserved marker keys (`__rjava_bean_class` + `value`) — harmless to
    // every other consumer — and reading the map back unwraps the marker
    // into the plain struct, so the value tree round-trips.
    let v = WithBean {
        name: "outer".to_string(),
        inner: JavaBean {
            value: Inner {
                id: 1,
                label: "inner".to_string(),
            },
            class: "unused.Class",
        },
    };
    let hm = java.new_object("java.util.HashMap", ()).unwrap();
    hm.call_void("putAll", (JavaMap(&v),)).unwrap();

    // The `inner` entry is a HashMap holding exactly the two marker keys.
    let inner: JObject = hm.call("get", ("inner",)).unwrap();
    let size: i32 = inner.call("size", ()).unwrap();
    assert_eq!(size, 2, "the marker map must hold exactly the two reserved keys");
    let has_class: bool = inner.call("containsKey", ("__rjava_bean_class",)).unwrap();
    let has_value: bool = inner.call("containsKey", ("value",)).unwrap();
    assert!(has_class && has_value, "the marker keys must be present");

    let got: WithBean = with_env(|env| from_object(env, &hm)).unwrap();
    assert_eq!(got.name, v.name);
    assert_eq!(got.inner.value, v.inner.value);
    assert_eq!(
        got.inner.class,
        "",
        "a read value's class string is write-side only and comes back empty"
    );
}
