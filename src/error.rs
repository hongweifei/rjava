//! The single error type used across the whole crate.
//!
//! Everything that can go wrong — a raw JNI failure, a pending Java
//! exception, a type/arity mismatch, or a failure to start/attach a JVM —
//! collapses into [`JavaError`]. This mirrors mlua's single [`mlua::Error`]
//! and keeps the public API small: every fallible `rjava` call returns
//! [`JavaResult<T>`].

use std::fmt;

/// Result alias used by every fallible `rjava` API.
pub type JavaResult<T> = Result<T, JavaError>;

/// The one error type of the crate.
///
/// # Variants
///
/// * [`JavaError::Jni`] — a raw JNI failure surfaced by the underlying `jni`
///   crate (null pointers, wrong argument lists, class-format problems, a JVM
///   that is not attached, ...). The [`jni::errors::Error`] is kept for
///   inspection.
///
/// * [`JavaError::JavaException`] — a pending Java exception was detected
///   after a JNI call. The exception has **already been captured and
///   cleared**: `class` and `message` are read from the throwable before the
///   pending exception is cleared, so subsequent JNI calls on this thread are
///   not poisoned by it.
///
/// * [`JavaError::InvalidArgument`] — a misuse of the `rjava` API (wrong type
///   annotation, null where a value was required, a class name in the wrong
///   form, ...). The message is a `'static` string that names the offending
///   class/method/argument.
///
/// * [`JavaError::JvmStart`] — the JVM could not be created or located
///   (missing `JAVA_HOME`, `jvm.dll` not found, `-X` option rejected, ...).
///
/// * [`JavaError::Serde`] — a conversion failure from the optional `serde`
///   feature (see `rjava::serde`, which only exists when the feature is
///   enabled). The message is dynamic because it names the offending Java
///   value (e.g. the class of an unsupported object) or carries a serde
///   `Error::custom` string, neither of which a `'static` message can hold.
///   Nothing outside the `serde` feature constructs it.
///
/// * [`JavaError::WithContext`] — a dynamic JNI operation failed and the
///   error names the operation. `operation` is a human-readable description
///   like `calling parseInt(String) on java.lang.Integer`; `source` is the
///   underlying error ([`JavaError::with_operation`] attaches it, and only
///   ever once — a `WithContext` is never wrapped again).
///
/// `JavaError` is `Send + Sync + 'static` and implements
/// [`std::error::Error`], so it works with `Box<dyn Error>` and `anyhow`.
///
/// [`std::fmt::Display`] is the human-readable one-liner — for a
/// [`JavaError::JavaException`] it renders exactly like the Java exception
/// (`java.lang.NumberFormatException: For input string: "x"`), so
/// `println!("{e}")` in a CLI or log line reads like the JVM's own stack
/// trace. [`Debug`] (derived) remains the canonical machine-readable
/// rendering of the enum shape.
#[derive(Debug)]
pub enum JavaError {
    /// A raw JNI error from the underlying `jni` crate.
    Jni(jni::errors::Error),
    /// A Java exception was thrown and has been captured and cleared.
    JavaException {
        /// Binary name of the exception class, e.g. `java.lang.NumberFormatException`.
        class: String,
        /// The value of `Throwable.getMessage()` (may be empty).
        message: String,
    },
    /// A misuse of the `rjava` API with an actionable static message.
    InvalidArgument(&'static str),
    /// The JVM could not be created or located.
    JvmStart(String),
    /// A serde conversion failure (feature `serde`): a dynamic message
    /// naming the offending Java value, or a serde `Error::custom` string.
    ///
    /// [`JavaError::InvalidArgument`] carries a `'static` message and so
    /// cannot name a runtime Java class; the `serde` feature reports its
    /// conversion failures through this variant instead. See the
    /// `rjava::serde` module (enabled by the `serde` feature) for the
    /// supported value types and the error contract.
    Serde(String),
    /// A dynamic JNI operation failed, and the error names the operation.
    ///
    /// `operation` is a human-readable description of the Java operation
    /// that failed — e.g. `calling parseInt(String) on java.lang.Integer`,
    /// `constructing java.lang.Integer(String)`, `reading field count on
    /// com.example.Counter` — and `source` is the underlying error. The
    /// dynamic-call entry points (`call`, `call_static`, `new_object`, the
    /// field accessors) attach this context once via
    /// [`JavaError::with_operation`], which is idempotent: a `WithContext`
    /// is never wrapped a second time, so a failure never accumulates a
    /// stack of operation contexts.
    ///
    /// [`std::fmt::Display`] renders `{operation}: {source}`, e.g.
    /// `calling parseInt(String) on java.lang.Integer:
    /// java.lang.NumberFormatException: For input string: "x"`.
    WithContext {
        /// The underlying error — whatever the operation itself failed
        /// with, before the context was attached.
        source: Box<JavaError>,
        /// A human-readable description of the failed operation.
        operation: String,
    },
}

impl fmt::Display for JavaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JavaError::Jni(e) => write!(f, "JNI error: {e}"),
            JavaError::JavaException { class, message } => {
                // Render exactly like the Java exception's own stack trace
                // line: `java.lang.NumberFormatException: For input string:
                // "x"`. An empty message renders just the class name.
                if message.is_empty() {
                    write!(f, "{class}")
                } else {
                    write!(f, "{class}: {message}")
                }
            }
            JavaError::InvalidArgument(msg) => write!(f, "invalid argument: {msg}"),
            JavaError::JvmStart(msg) => write!(f, "failed to start the JVM: {msg}"),
            JavaError::Serde(msg) => write!(f, "serde conversion failed: {msg}"),
            JavaError::WithContext { source, operation } => {
                write!(f, "{operation}: {source}")
            }
        }
    }
}

impl JavaError {
    /// Attach the operation that failed to this error, wrapping it in
    /// [`JavaError::WithContext`].
    ///
    /// The operation is a human-readable description of the Java operation
    /// that failed, e.g. `calling parseInt(String) on java.lang.Integer` or
    /// `reading field count on com.example.Counter`. It is attached only
    /// **once**: if `self` is already a [`JavaError::WithContext`] it is
    /// returned unchanged, so wrapping at an outer boundary never stacks a
    /// second context on an error that already names its operation.
    ///
    /// The dynamic-call entry points of the crate use this automatically —
    /// a failed `call`/`call_static`/`new_object`/field access names the
    /// operation in its error. Native-method authors returning
    /// `Err(JavaError::JavaException { .. })` from their own code do not
    /// need it; it is for failures that bubble out of the dynamic machinery.
    pub fn with_operation(self, operation: impl Into<String>) -> JavaError {
        match self {
            // Idempotent: never stack operation contexts.
            JavaError::WithContext { .. } => self,
            other => JavaError::WithContext {
                source: Box::new(other),
                operation: operation.into(),
            },
        }
    }
}

impl std::error::Error for JavaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            JavaError::Jni(e) => Some(e),
            JavaError::WithContext { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

impl From<jni::errors::Error> for JavaError {
    fn from(err: jni::errors::Error) -> Self {
        match err {
            // jni's `CaughtJavaException` is exactly a Java exception that has
            // already been captured and cleared; it carries the class name and
            // message we want, so surface it as our own `JavaException`.
            jni::errors::Error::CaughtJavaException { name, msg, .. } => {
                JavaError::JavaException {
                    class: name,
                    message: msg,
                }
            }
            other => JavaError::Jni(other),
        }
    }
}
