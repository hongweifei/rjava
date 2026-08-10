//! Runtime class loading: [`JClassLoader`], a RAII handle over a
//! `java.net.URLClassLoader`.
//!
//! The JVM's system class path is fixed when the JVM is created, so classes
//! cannot be added to it at runtime. [`JClassLoader`] wraps a
//! `URLClassLoader` instead: the host creates one per jar (or class-path
//! directory) and looks classes up through it — see
//! [`Java::class_loader`](crate::Java::class_loader),
//! [`Java::load_jar`](crate::Java::load_jar) and
//! [`Java::class_loader_with_parent`](crate::Java::class_loader_with_parent).

use std::sync::Arc;

use jni::objects::{Global, JObject as JniObject};
use jni::Env;

use crate::call;
use crate::convert::{JavaArg, ToJava};
use crate::error::JavaResult;
use crate::handles::{with_env, JClass, JObject};

/// An owned, thread-safe handle to a `java.net.URLClassLoader`.
///
/// Like the other handles ([`JObject`](crate::JObject),
/// [`JClass`](crate::JClass)), this wraps a JNI *global* reference behind an
/// `Arc`: `Clone` is `O(1)` and shares the reference, the loader stays alive
/// (not garbage-collectable) as long as any clone exists, and `Drop` releases
/// the global reference. **`Drop` does *not* call `close()`** — that is
/// explicit and optional (see [`JClassLoader::close`]); when the last handle
/// drops, the JVM's garbage collector collects the loader.
///
/// # Plugin workflow
///
/// This is the piece that makes runtime *plugin* loading possible:
///
/// 1. An API author writes a Java API — one or more interfaces plus a
///    `Bridge` class whose static methods are `native` — and ships it as an
///    API jar. The API jar is the **compile-time contract** for plugin
///    developers: they compile their plugins against it.
/// 2. The Rust host loads the API jar at runtime with
///    [`Java::load_jar`](crate::Java::load_jar), looks up the `Bridge` class
///    with [`JClassLoader::load_class`], and registers Rust implementations
///    for its `native` methods with
///    [`JClass::register_natives`](crate::JClass::register_natives).
/// 3. Plugin developers ship jars compiled against the API jar. The host
///    loads each plugin jar with a loader whose **parent** is the API loader
///    ([`Java::class_loader_with_parent`](crate::Java::class_loader_with_parent)),
///    so plugin code resolves the API interfaces and the `Bridge` through the
///    very classes the host registered natives on.
/// 4. Plugin classes are instantiated and called like any other class loaded
///    from a [`JClass`] handle.
///
/// Security note: a `URLClassLoader` loads whatever jar you point it at, so
/// only load jars you trust — a hostile jar can run arbitrary Java.
#[derive(Clone, Debug)]
pub struct JClassLoader {
    global: Arc<Global<JniObject<'static>>>,
}

impl JClassLoader {
    pub(crate) fn from_handle(handle: JObject) -> Self {
        JClassLoader {
            global: Arc::clone(&handle.global),
        }
    }

    /// Load a class by its binary name (`com.example.plugin.HelloPlugin`) —
    /// `ClassLoader.loadClass(String)`.
    ///
    /// The loader delegates to its parent first (so `java.lang.String` and
    /// any class on the JVM's system class path resolve through any loader),
    /// then searches its own URLs. A class that cannot be found surfaces as
    /// [`JavaError::JavaException`](crate::JavaError::JavaException) with
    /// class `java.lang.ClassNotFoundException` (and no special handling is
    /// needed — the existing exception machinery reports it).
    pub fn load_class(&self, name: &str) -> JavaResult<JClass> {
        with_env(|env| call::call_method(env, &self.global, "loadClass", &(name,)))
    }

    /// Close the class loader (`URLClassLoader.close()`), releasing the
    /// underlying jar/directory file handles.
    ///
    /// Classes already loaded stay usable; loading *new* classes through a
    /// closed loader throws a Java `java.io.IOException`. This is explicit
    /// and optional — `Drop` does **not** close the loader (see the [type
    /// docs](JClassLoader)); the JVM collects it once the last handle drops.
    pub fn close(&self) -> JavaResult<()> {
        with_env(|env| call::call_method(env, &self.global, "close", &()))
    }
}

impl ToJava for JClassLoader {
    fn to_java<'env>(&self, env: &mut Env<'env>) -> JavaResult<Vec<JavaArg<'env>>> {
        Ok(vec![JavaArg::Object(env.new_local_ref(&*self.global)?)])
    }
    fn java_args(&self) -> String {
        String::from("Ljava/lang/ClassLoader;")
    }
}
