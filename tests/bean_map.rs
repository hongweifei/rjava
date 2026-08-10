#![forbid(unsafe_code)]
//! Integration tests for the bean mapping (`rjava::bean`, feature `serde`):
//! Rust structs ⇄ plain Java beans via getter/setter reflection.
//!
//! The fixtures (`tests/java/bean/*.java`) are compiled with `javac` into
//! `target/rjava-bean-classes` once per test process; if no JVM (or no
//! `javac`) can be located every test skips with an `eprintln` reason (the
//! same convention as `tests/run_main.rs`).

use std::sync::LazyLock;

use rjava::bean::JavaBean;
use rjava::prelude::*;
use serde::{Deserialize, Serialize};

/// Compile the `bean` fixtures exactly once per test process. `Err` carries
/// the reason, reported once; when it fails every test skips.
static FIXTURE_COMPILED: LazyLock<Result<(), String>> = LazyLock::new(compile_fixture);

fn compile_fixture() -> Result<(), String> {
    // `javac -d` creates the output directory itself; create it first anyway
    // so a failure here is reported as a compile problem, not a mystery.
    let out_dir = "target/rjava-bean-classes";
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
        .arg("tests/java/bean/User.java")
        .arg("tests/java/bean/ReadOnly.java")
        .arg("tests/java/bean/WriteOnly.java")
        .arg("tests/java/bean/Point.java")
        .arg("tests/java/bean/City.java")
        .arg("tests/java/bean/Address.java")
        .arg("tests/java/bean/Contact.java")
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

/// Attempt to build the JVM with the fixture classes on the class path;
/// returns `None` (after an `eprintln`) if no JVM is available so tests can
/// skip instead of failing on JVM-less machines.
fn jvm() -> Option<Java> {
    if let Err(reason) = &*FIXTURE_COMPILED {
        eprintln!("SKIPPING test: could not compile Java fixture: {reason}");
        return None;
    }
    match Java::builder()
        .class_path("target/rjava-bean-classes")
        .build()
    {
        Ok(java) => Some(java),
        Err(e) => {
            eprintln!("SKIPPING test: no JVM available: {e}");
            None
        }
    }
}

/// Run `f` with the current thread attached, handing it a `&mut Env` — the
/// `from_object` entry takes an `Env` (mirroring the serde-map tests).
fn with_env<T>(f: impl FnOnce(&mut jni::Env<'_>) -> JavaResult<T>) -> JavaResult<T> {
    let vm = rjava::jni::JavaVM::singleton().map_err(JavaError::from)?;
    vm.attach_current_thread::<_, T, JavaError>(f)
}

/// Build a `bean.User` from `bean` (through the `echo` fixture hook) and
/// return it.
fn echo_bean<T: serde::Serialize>(java: &Java, bean: &JavaBean<T>) -> JObject {
    echo_class(java, "bean.User", bean)
}

/// Build an object of `class` from `bean` (through that class's `echo`
/// fixture hook) and return it.
fn echo_class<T: serde::Serialize>(java: &Java, class: &str, bean: &JavaBean<T>) -> JObject {
    java.class(class)
        .expect("fixture class")
        .call_static("echo", (bean,))
        .expect("echo should build and return the bean")
}

/// Read a bean object back into `T` through its getters.
fn read<T: serde::de::DeserializeOwned>(obj: &JObject) -> T {
    with_env(|env| JavaBean::from_object(env, obj)).expect("from_object should read the bean")
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct User {
    id: i64,
    name: Option<String>,
    active: bool,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Flags {
    active: bool,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct ReadOnly {
    id: i64,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct WriteOnly {
    label: String,
}

// --- Java records (tests/java/bean/Point.java) ---

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Point {
    x: i32,
    y: i32,
    label: String,
}

/// The same shape with the `x`/`y` fields swapped: writing it into
/// `bean.Point` must fail loudly (the canonical constructor would silently
/// swap the values otherwise).
#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct SwappedPoint {
    y: i32,
    x: i32,
    label: String,
}

// --- Bean-to-bean nesting (tests/java/bean/Contact / Address / City) ---

#[derive(Serialize, Deserialize)]
struct City {
    name: String,
}

#[derive(Serialize, Deserialize)]
struct Address {
    street: String,
    city: JavaBean<City>,
}

#[derive(Serialize, Deserialize)]
struct Contact {
    name: String,
    address: JavaBean<Address>,
}

/// The same shape with a nullable nested bean — `None` ⇄ Java null.
#[derive(Serialize, Deserialize)]
struct ContactOpt {
    name: String,
    address: Option<JavaBean<Address>>,
}

impl PartialEq for Contact {
    fn eq(&self, other: &Self) -> bool {
        // The `class` strings differ by design (write-side target vs the
        // empty read-side string), so equality compares the values only.
        self.name == other.name
            && self.address.value.street == other.address.value.street
            && self.address.value.city.value.name == other.address.value.city.value.name
    }
}

impl PartialEq for ContactOpt {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && match (&self.address, &other.address) {
                (Some(a), Some(b)) => {
                    a.value.street == b.value.street
                        && a.value.city.value.name == b.value.city.value.name
                }
                (None, None) => true,
                _ => false,
            }
    }
}

#[test]
fn bean_round_trip() {
    let Some(java) = jvm() else { return };
    let v = User {
        id: 42,
        name: Some("Ada".to_string()),
        active: true,
    };
    let obj = echo_bean(&java, &JavaBean { value: &v, class: "bean.User" });
    let back: User = read(&obj);
    assert_eq!(back, v, "struct → object → struct must round-trip");
}

#[test]
fn bean_bool_via_is_getter() {
    let Some(java) = jvm() else { return };
    // `bean.User` has `isActive()` but no `getActive()`: reading `active`
    // must fall back to the `is<Name>` accessor.
    let f = Flags { active: true };
    let obj = echo_bean(&java, &JavaBean { value: &f, class: "bean.User" });
    let back: Flags = read(&obj);
    assert_eq!(back, f);
}

#[test]
fn bean_missing_setter_errors() {
    let Some(java) = jvm() else { return };
    // `bean.ReadOnly` has `getId()` but no `setId()` — the write must fail
    // loudly, naming the property and the attempted method.
    let err = java
        .new_object(
            "bean.ReadOnly",
            (
                JavaBean {
                    value: ReadOnly { id: 5 },
                    class: "bean.ReadOnly",
                },
            ),
        )
        .unwrap_err();
    // The bean write runs inside the dynamic constructor call, so the serde
    // failure carries the operation context.
    match err {
        JavaError::WithContext { operation, source } => {
            assert_eq!(
                operation, "constructing bean.ReadOnly(ReadOnly)",
                "operation must name the failed construction"
            );
            assert!(
                matches!(*source, JavaError::Serde(_)),
                "expected JavaError::Serde, got {source:?}"
            );
            let msg = source.to_string();
            assert!(
                msg.contains("bean.ReadOnly") && msg.contains("`id`") && msg.contains("setId"),
                "the error must name the class, the property and the attempted setter: {msg}"
            );
        }
        other => panic!("expected WithContext, got {other:?}"),
    }
}

#[test]
fn bean_missing_getter_errors() {
    let Some(java) = jvm() else { return };
    // `bean.WriteOnly` has `setLabel()` but no getter — the read must fail
    // loudly, naming the property and both attempted accessors.
    let obj = java.new_object("bean.WriteOnly", ()).expect("WriteOnly instance");
    let err = with_env(|env| JavaBean::<WriteOnly>::from_object(env, &obj)).unwrap_err();
    assert!(
        matches!(err, JavaError::Serde(_)),
        "expected JavaError::Serde, got {err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("bean.WriteOnly") && msg.contains("`label`") && msg.contains("getLabel"),
        "the error must name the class, the property and the attempted getter: {msg}"
    );
    assert!(
        msg.contains("isLabel"),
        "the error must name the `is<Name>` fallback too: {msg}"
    );
}

#[test]
fn bean_option_some_and_none() {
    let Some(java) = jvm() else { return };
    // Some → the setter receives the String; None → the setter receives
    // Java null.
    let some = User {
        id: 10,
        name: Some("Grace".to_string()),
        active: false,
    };
    let obj = echo_bean(&java, &JavaBean { value: &some, class: "bean.User" });
    let back: User = read(&obj);
    assert_eq!(back, some);

    let none = User {
        id: 11,
        name: None,
        active: true,
    };
    let obj = echo_bean(&java, &JavaBean { value: &none, class: "bean.User" });
    let back: User = read(&obj);
    assert_eq!(back, none, "None must round-trip through a Java null");
    // And the Java property really is null:
    let name: Option<JObject> = obj.call("getName", ()).expect("getName");
    assert!(name.is_none(), "name must be stored as a Java null");
}

#[test]
fn bean_independent_objects() {
    let Some(java) = jvm() else { return };
    // Two structs with the same shape must build two independent objects —
    // no shared state between JavaBean instances.
    let a = User {
        id: 1,
        name: Some("a".to_string()),
        active: false,
    };
    let b = User {
        id: 2,
        name: Some("b".to_string()),
        active: true,
    };
    let oa = echo_bean(&java, &JavaBean { value: &a, class: "bean.User" });
    let ob = echo_bean(&java, &JavaBean { value: &b, class: "bean.User" });
    assert_eq!(read::<User>(&oa), a);
    assert_eq!(read::<User>(&ob), b);

    let sys = java.class("java.lang.System").expect("System class");
    let ha: i32 = sys.call_static("identityHashCode", (&oa,)).expect("identityHashCode");
    let hb: i32 = sys.call_static("identityHashCode", (&ob,)).expect("identityHashCode");
    assert_ne!(ha, hb, "each JavaBean must build its own object");
}

#[test]
fn bean_from_object_derives_class_from_object() {
    let Some(java) = jvm() else { return };
    let v = User {
        id: 7,
        name: Some("read".to_string()),
        active: true,
    };
    let obj = echo_bean(&java, &JavaBean { value: &v, class: "bean.User" });

    // `from_object` needs no class string: the property lookup derives from
    // the object's runtime class.
    let back: User = with_env(|env| JavaBean::from_object(env, &obj)).expect("from_object");
    assert_eq!(back, v);

    // The `FromJava` read path likewise reads the runtime class — the
    // result's `class` string is write-side only and comes back empty, even
    // though the object's runtime class is `bean.User`.
    let echoed: JavaBean<User> = java
        .class("bean.User")
        .expect("bean.User class")
        .call_static("echo", (&obj,))
        .expect("echo");
    assert_eq!(echoed.value, v);
    assert_eq!(echoed.class, "", "read results carry an empty write-side class");
}

// ---------------------------------------------------------------------------
// Java records (tests/java/bean/Point.java)
// ---------------------------------------------------------------------------

#[test]
fn record_round_trip_via_canonical_ctor_and_accessors() {
    let Some(java) = jvm() else { return };
    // `bean.Point` is a record with **no** no-arg constructor: writing must
    // go through the canonical `Point(int, int, String)`, reading through
    // the component accessors `x()`, `y()`, `label()`.
    let v = Point {
        x: 3,
        y: 4,
        label: "origin".to_string(),
    };
    let obj = echo_class(&java, "bean.Point", &JavaBean { value: &v, class: "bean.Point" });
    let back: Point = read(&obj);
    assert_eq!(back, v);

    // The record's properties really are readable through the component
    // accessors (`x()` — no `get`/`is` prefix).
    let x: i32 = obj.call("x", ()).expect("x()");
    let y: i32 = obj.call("y", ()).expect("y()");
    let label: String = obj.call("label", ()).expect("label()");
    assert_eq!((x, y, label.as_str()), (3, 4, "origin"));
}

#[test]
fn record_field_order_mismatch_errors() {
    let Some(java) = jvm() else { return };
    // The Rust struct's field order must match the record's component order
    // (the canonical constructor's parameter order); a swap would silently
    // put values in the wrong components, so it must error loudly, naming
    // both orders.
    let err = java
        .class("bean.Point")
        .expect("bean.Point class")
        .call_static::<_, JObject>(
            "echo",
            (
                JavaBean {
                    value: &SwappedPoint {
                        y: 1,
                        x: 2,
                        label: "p".to_string(),
                    },
                    class: "bean.Point",
                },
            ),
        )
        .expect_err("the canonical-constructor write must reject a field-order mismatch");
    // The bean serialization runs inside the dynamic call, so the serde
    // failure carries the operation context.
    match err {
        JavaError::WithContext { operation, source } => {
            assert_eq!(
                operation, "calling echo(Point) on bean.Point",
                "operation must name the failed call"
            );
            assert!(
                matches!(*source, JavaError::Serde(_)),
                "expected JavaError::Serde, got {source:?}"
            );
            let msg = source.to_string();
            assert!(
                msg.contains("bean.Point")
                    && msg.contains("x, y, label")
                    && msg.contains("y, x, label"),
                "the error must name the record and both field orders: {msg}"
            );
        }
        other => panic!("expected WithContext, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Bean-to-bean nesting (tests/java/bean/Contact / Address / City)
// ---------------------------------------------------------------------------

#[test]
fn bean_nested_round_trip() {
    let Some(java) = jvm() else { return };
    // Contact { name, address: JavaBean<Address> } with Address { street,
    // city: JavaBean<City> }: writing must build nested bean objects and
    // pass them to the outer setters; reading must derive each nested class
    // from the object at runtime and read getter by getter.
    let v = Contact {
        name: "Ada".to_string(),
        address: JavaBean {
            value: Address {
                street: "Main St".to_string(),
                city: JavaBean {
                    value: City {
                        name: "Springfield".to_string(),
                    },
                    class: "bean.City",
                },
            },
            class: "bean.Address",
        },
    };
    let obj = echo_class(&java, "bean.Contact", &JavaBean { value: &v, class: "bean.Contact" });
    let back: Contact = read(&obj);
    assert!(back == v, "struct → object → struct must round-trip");
    assert_eq!(
        back.address.class,
        "",
        "a nested read derives the class at runtime; the class string stays write-side only"
    );

    // The nested address really is a bean object (not a HashMap): its
    // property is readable through the Java getter.
    let addr: Option<JObject> = obj.call("getAddress", ()).expect("getAddress");
    let addr = addr.expect("the nested address must be non-null");
    let street: String = addr.call("getStreet", ()).expect("getStreet");
    assert_eq!(street, "Main St");
    let city: Option<JObject> = addr.call("getCity", ()).expect("getCity");
    let city = city.expect("the nested city must be non-null");
    let name: String = city.call("getName", ()).expect("getName");
    assert_eq!(name, "Springfield");
}

#[test]
fn bean_nested_option_some_and_none() {
    let Some(java) = jvm() else { return };
    // `Option<JavaBean<Address>>` as a field: `None` → the outer setter
    // receives Java null; `Some` → a nested bean object. `bean.Contact`'s
    // `setAddress(Address)` accepts both.
    let none = ContactOpt {
        name: "none".to_string(),
        address: None,
    };
    let obj = echo_class(&java, "bean.Contact", &JavaBean { value: &none, class: "bean.Contact" });
    let back: ContactOpt = read(&obj);
    assert!(back == none, "None must round-trip through a Java null property");
    let null: Option<JObject> = obj.call("getAddress", ()).expect("getAddress");
    assert!(null.is_none(), "None must be stored as a Java null property");

    let some = ContactOpt {
        name: "some".to_string(),
        address: Some(JavaBean {
            value: Address {
                street: "Broadway".to_string(),
                city: JavaBean {
                    value: City {
                        name: "Metropolis".to_string(),
                    },
                    class: "bean.City",
                },
            },
            class: "bean.Address",
        }),
    };
    let obj = echo_class(&java, "bean.Contact", &JavaBean { value: &some, class: "bean.Contact" });
    let back: ContactOpt = read(&obj);
    assert!(back == some, "Some must round-trip as a nested bean object");
}
