#![forbid(unsafe_code)]
//! `JavaError` ergonomics: the `Display` impl (renders like the Java
//! exception), the `JavaError::WithContext` operation context on
//! dynamic-call failures, and `JavaError::with_operation`.
//!
//! The JVM-free tests construct `JavaError` values directly; the JVM-backed
//! tests exercise the real dynamic-call paths (`call_static`, `new_object`,
//! instance `call`, field reads) and assert that a failure names the
//! operation *and* keeps the underlying error.
//!
//! If no JVM can be located at test time, the JVM-backed tests skip
//! gracefully with an `eprintln` reason (the `jni` crate's java-locator
//! uses `JAVA_HOME`, then `java` on `PATH`, then the Windows registry).

use std::error::Error as _;

use rjava::prelude::*;

/// Attempt to build the JVM; returns `None` (after an `eprintln`) if no JVM
/// is available so tests can skip instead of failing on JVM-less machines.
/// These tests need only JDK classes, so no `javac` fixture is compiled.
fn jvm() -> Option<Java> {
    match Java::builder().build() {
        Ok(java) => Some(java),
        Err(e) => {
            eprintln!("SKIPPING test: could not create a JVM: {e}");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Display
// ---------------------------------------------------------------------------

#[test]
fn display_renders_like_the_java_exception() {
    let err = JavaError::JavaException {
        class: "java.lang.NumberFormatException".to_string(),
        message: "For input string: \"x\"".to_string(),
    };
    assert_eq!(
        err.to_string(),
        "java.lang.NumberFormatException: For input string: \"x\""
    );
    // An empty message renders just the class.
    let err = JavaError::JavaException {
        class: "java.lang.NullPointerException".to_string(),
        message: String::new(),
    };
    assert_eq!(err.to_string(), "java.lang.NullPointerException");
    // Debug stays derived (the machine-readable struct shape).
    assert_eq!(
        format!("{err:?}"),
        "JavaException { class: \"java.lang.NullPointerException\", message: \"\" }"
    );
}

#[test]
fn display_other_variants_are_one_liners() {
    let err = JavaError::InvalidArgument("null is not a valid argument");
    assert_eq!(err.to_string(), "invalid argument: null is not a valid argument");

    let err = JavaError::JvmStart("jvm.dll not found".to_string());
    assert_eq!(err.to_string(), "failed to start the JVM: jvm.dll not found");

    let err = JavaError::Serde("cannot serialize u32".to_string());
    assert_eq!(err.to_string(), "serde conversion failed: cannot serialize u32");

    let err = JavaError::Jni(jni::errors::Error::NullPtr("test"));
    // The raw jni error's own Display is carried through.
    assert_eq!(err.to_string(), "JNI error: Null pointer in test");
}

// ---------------------------------------------------------------------------
// with_operation / WithContext
// ---------------------------------------------------------------------------

#[test]
fn with_operation_wraps_and_is_idempotent() {
    let inner = JavaError::JavaException {
        class: "java.lang.NumberFormatException".to_string(),
        message: "For input string: \"x\"".to_string(),
    };
    let err = inner.with_operation("calling parseInt(String) on java.lang.Integer");
    match &err {
        JavaError::WithContext { operation, source } => {
            assert_eq!(operation, "calling parseInt(String) on java.lang.Integer");
            assert!(matches!(
                &**source,
                JavaError::JavaException { class, .. } if class == "java.lang.NumberFormatException"
            ));
        }
        other => panic!("expected WithContext, got {other:?}"),
    }
    // Idempotent: wrapping an already-wrapped error keeps the FIRST
    // operation — a failure never accumulates a stack of contexts.
    let again = err.with_operation("calling somethingElse() on java.lang.Object");
    match &again {
        JavaError::WithContext { operation, .. } => {
            assert_eq!(operation, "calling parseInt(String) on java.lang.Integer");
        }
        other => panic!("expected the unchanged WithContext, got {other:?}"),
    }
    // Display: `{operation}: {source}`, with the source rendered by ITS
    // Display (the Java-exception form).
    assert_eq!(
        again.to_string(),
        "calling parseInt(String) on java.lang.Integer: \
         java.lang.NumberFormatException: For input string: \"x\""
    );
}

#[test]
fn source_chain_is_preserved() {
    // WithContext delegates source() to the inner error.
    let inner = JavaError::JavaException {
        class: "java.lang.NumberFormatException".to_string(),
        message: "bad".to_string(),
    };
    let err = inner.with_operation("op");
    assert!(
        err.source().is_some(),
        "a WithContext must expose its inner error as the source"
    );
    // A plain JavaException has no further chain.
    let plain = JavaError::JavaException {
        class: "x".to_string(),
        message: "y".to_string(),
    };
    assert!(plain.source().is_none());
    // A raw JNI error keeps exposing the underlying jni error.
    let jni_err = JavaError::Jni(jni::errors::Error::NullPtr("test"));
    assert!(jni_err.source().is_some());
}

// ---------------------------------------------------------------------------
// Dynamic-call failures name the operation (JVM-backed)
// ---------------------------------------------------------------------------

#[test]
fn static_call_failure_names_operation() {
    let Some(java) = jvm() else { return };
    let err = java
        .class("java.lang.Integer")
        .unwrap()
        .call_static::<_, i32>("parseInt", ("not-a-number",))
        .unwrap_err();
    match err {
        JavaError::WithContext { operation, source } => {
            assert_eq!(
                operation, "calling parseInt(String) on java.lang.Integer",
                "operation = method name + arg types + target class"
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
    // End to end, Display reads like the JVM stack-trace line with the
    // operation in front.
    let err = java
        .class("java.lang.Integer")
        .unwrap()
        .call_static::<_, i32>("parseInt", ("not-a-number",))
        .unwrap_err();
    assert!(
        err.to_string().starts_with(
            "calling parseInt(String) on java.lang.Integer: \
             java.lang.NumberFormatException:"
        ),
        "unexpected rendering: {err}"
    );
}

#[test]
fn instance_call_failure_names_operation() {
    let Some(java) = jvm() else { return };
    let sb = java.new_object("java.lang.StringBuilder", ()).unwrap();
    // charAt(99) on an empty builder throws StringIndexOutOfBoundsException.
    let err = sb.call::<_, char>("charAt", (99_i32,)).unwrap_err();
    match err {
        JavaError::WithContext { operation, source } => {
            assert_eq!(
                operation, "calling charAt(int) on java.lang.StringBuilder",
                "instance calls name the object's runtime class"
            );
            assert!(matches!(*source, JavaError::JavaException { .. }));
        }
        other => panic!("expected WithContext, got {other:?}"),
    }
}

#[test]
fn constructor_failure_names_operation() {
    let Some(java) = jvm() else { return };
    // Integer(String) with a non-numeric string throws NumberFormatException.
    let err = java
        .new_object("java.lang.Integer", ("not-a-number",))
        .unwrap_err();
    match err {
        JavaError::WithContext { operation, source } => {
            assert_eq!(
                operation, "constructing java.lang.Integer(String)",
                "constructors name the class and its argument types"
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
}

#[test]
fn instance_field_failure_names_operation() {
    let Some(java) = jvm() else { return };
    // CompletableFuture.result is an Object-typed field that is null until
    // the future completes; reading it as a JObject (non-nullable) is a
    // conversion failure — the kind of field failure that is not a missing
    // field (a missing field stays a raw FieldNotFound, which bind!'s
    // bool-field accessor probes for).
    let cf = java
        .new_object("java.util.concurrent.CompletableFuture", ())
        .unwrap();
    let err = cf.get_field::<JObject>("result").unwrap_err();
    match err {
        JavaError::WithContext { operation, source } => {
            assert_eq!(
                operation, "reading field result on java.util.concurrent.CompletableFuture",
                "field reads name the field and the object's class"
            );
            assert!(
                matches!(*source, JavaError::InvalidArgument(_)),
                "the null-object rejection must be the source, got {source:?}"
            );
        }
        other => panic!("expected WithContext, got {other:?}"),
    }
}
