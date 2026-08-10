//! Rust structs ⇄ plain Java beans via getter/setter reflection (feature
//! `serde`).
//!
//! The serde feature's sibling module ([`crate::serde`]) maps a Rust struct
//! to a `java.util.HashMap` value tree. A **bean** is the other common Java
//! contract: an arbitrary class with private fields and public accessors —
//! `getXxx` / `setXxx` (and `isXxx` for booleans). **Java records** — the
//! accessor contract `x()` plus a canonical constructor — are mapped too
//! (see [Java records](#java-records) below).
//! [`JavaBean`](crate::bean::JavaBean) wraps a
//! serde-serializable Rust value so it can travel through the
//! [`ToJava`]/[`FromJava`] machinery:
//!
//! * **Write** — pass `JavaBean { value, class }` wherever a parameter of
//!   `class` (or a supertype, or `Object`) is expected. The object is
//!   created with the class's **public no-arg constructor** (or, for a
//!   record, its **canonical constructor** — see below) and then filled
//!   with **one setter call per struct field**. The field name is camelCased
//!   with a simple word-boundary rule (`user_id` → `UserId`, so the property
//!   is addressed as `setUserId`; `id` → `Id`, `setId` — **no acronym
//!   special-casing**). The field *value* goes through the existing serde
//!   machinery, so primitives, `String` and `Option` (→ Java `null`) all
//!   work: an `i64` field boxed to `Long` unboxes into a `long` setter
//!   parameter, a `String` matches a `String` (or `Object`) parameter, and
//!   `None` becomes a `null` argument (which matches reference parameters
//!   only). A struct field whose Rust type is itself a `JavaBean`
//!   (**bean-to-bean nesting**) is built as a nested bean object and passed
//!   to the property's setter — see [Bean-to-bean nesting](#bean-to-bean-nesting).
//! * **Read** — annotate a call result as `JavaBean<T>` (or read an existing
//!   object with
//!   [`JavaBean::from_object`](crate::bean::JavaBean::from_object)) to read
//!   each property through
//!   its getter: `get<Name>`, with an `is<Name>` fallback (the JavaBeans
//!   convention for boolean properties); on a record, the component
//!   accessor `x()` is the final fallback (see below). Each getter's raw
//!   value feeds the
//!   existing value-level deserializer, so boxed primitives unbox, `String`
//!   comes back as `String`, `null` as `Option::None`. The class is derived
//!   from the object's **runtime** class — the `class` string is write-side
//!   only.
//!
//! # Example
//!
//! ```no_run
//! use rjava::prelude::*;
//! use rjava::bean::JavaBean;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize, Deserialize, PartialEq, Debug)]
//! struct User {
//!     id: i64,
//!     name: String,
//!     active: bool,
//! }
//!
//! # fn main() -> JavaResult<()> {
//! let java = Java::builder().class_path("target/bean-classes").build()?;
//! let user = User { id: 7, name: "Ada".to_string(), active: true };
//!
//! // Write: the `JavaBean` argument constructs a `bean.User` (`new User()`)
//! // and fills it with one setter call per field — `setId`, `setName`,
//! // `setActive`. Pass it wherever a `bean.User` (or `Object`) parameter is
//! // expected:
//! let obj: JObject = java
//!     .class("bean.User")?
//!     .call_static("echo", (JavaBean { value: &user, class: "bean.User" },))?;
//!
//! // Read: `from_object` derives the class from the object's runtime class
//! // and reads each property through its getter (`getId`, `getName`,
//! // `isActive`):
//! let vm = rjava::jni::JavaVM::singleton().map_err(JavaError::from)?;
//! let back: User = vm.attach_current_thread::<_, User, JavaError>(|env| {
//!     JavaBean::from_object(env, &obj)
//! })?;
//! assert_eq!(back, user);
//! # Ok(()) }
//! ```
//!
//! Records and bean-to-bean nesting work through the same wrapper:
//!
//! ```no_run
//! use rjava::prelude::*;
//! use rjava::bean::JavaBean;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize, Deserialize, PartialEq, Debug)]
//! struct Point {
//!     x: i32,
//!     y: i32,
//! }
//!
//! #[derive(Serialize, Deserialize)]
//! struct Shape {
//!     name: String,
//!     origin: JavaBean<Point>, // a record property: built via the canonical ctor, read via x()/y()
//! }
//!
//! # fn main() -> JavaResult<()> {
//! let java = Java::builder().class_path("target/bean-classes").build()?;
//! let shape = Shape {
//!     name: "square".to_string(),
//!     origin: JavaBean {
//!         value: Point { x: 0, y: 0 },
//!         class: "bean.Point",
//!     },
//! };
//!
//! // The nested `JavaBean<Point>` field builds a `bean.Point` object
//! // (canonical `Point(int, int)` constructor — no setters) and passes it
//! // to `setOrigin`. Reading back derives the class from the runtime
//! // object and reads `x()` / `y()`.
//! let obj: JObject = java
//!     .class("bean.Shape")?
//!     .call_static("echo", (JavaBean { value: &shape, class: "bean.Shape" },))?;
//! let vm = rjava::jni::JavaVM::singleton().map_err(JavaError::from)?;
//! let back: Shape = vm.attach_current_thread::<_, Shape, JavaError>(|env| {
//!     JavaBean::from_object(env, &obj)
//! })?;
//! assert_eq!(back.origin.value, Point { x: 0, y: 0 });
//! # Ok(()) }
//! ```
//!
//! # Supported field types
//!
//! The value level is exactly [`crate::serde`]'s: `String`/`&str` ⇄
//! `java.lang.String`, `bool` ⇄ `Boolean`, `i8` ⇄ `Byte`, `i16` ⇄ `Short`,
//! `i32` ⇄ `Integer`, `i64` ⇄ `Long`, `u8` ⇄ `Byte`, `f32` ⇄ `Float`,
//! `f64` ⇄ `Double`, `char` ⇄ `Character`, `Option<T>` (`None` ⇄ `null`),
//! `Vec<T>`/arrays/tuples ⇄ `java.util.ArrayList`, nested structs ⇄ nested
//! `HashMap`s (see below). A property setter whose
//! parameter is a **primitive** receives the boxed value unboxed — with the
//! crate's no-widening rule: the wrapper must match the primitive exactly
//! (`i64` → `Long` → `long` works; an `Integer` box does *not* fill a
//! `long` property), and `null` never matches a primitive parameter.
//!
//! # Java records
//!
//! A class whose `Class.isRecord()` returns `true` is mapped through its
//! record contract instead of the bean contract:
//!
//! * **Read** — a record has no `getX`/`isX` accessors; its properties are
//!   its components, read through the accessor `x()`. The read order is
//!   deterministic: `get<Name>` first, then `is<Name>` (a record may declare
//!   such methods as extras), then — for records only — the component
//!   accessor `<name>()`. A *non-record* bean is never probed for `x()`, so
//!   a plain bean property can never collide with an unrelated no-argument
//!   method (e.g. `wait()` on `Object`).
//! * **Write** — a record has **no setters**; the object is constructed
//!   through its **canonical constructor** (`new Point(int x, int y, …)`)
//!   with the struct's field values in **declaration order**. The canonical
//!   constructor's parameter order is the record's component order, so the
//!   Rust struct's field order must match the Java component order — the
//!   mapping checks this at runtime (`Class.getRecordComponents()` names,
//!   compared in order) and **errors loudly** naming both orders when they
//!   differ. (The constructor itself is resolved by the usual exact/box/unbox
//!   matching, so component types must accept the serialized values; records
//!   should not overload the canonical signature.)
//!
//! Detection is a static `Class.isRecord()` call — available since **Java
//! 16**; on an older JVM the lookup fails and the class is treated as a
//! plain bean (a pre-16 class cannot be a record). Records therefore
//! document **Java 16 as the JVM floor** for the record mapping.
//!
//! # Bean-to-bean nesting
//!
//! A struct field whose Rust type is `JavaBean<Inner>` maps to a nested
//! bean object of the class named by that `JavaBean`'s `class` string:
//!
//! * **Write** — `JavaBean<T>` implements `serde::Serialize` as a struct
//!   with the reserved shape `{ "__rjava_bean_class": class, "value": … }`.
//!   The bean write path recognizes this marker and builds the nested object
//!   (`new` + setters, or the canonical constructor for a record — nested
//!   beans inside nested beans recurse); the object is passed to the outer
//!   property's setter. Through the **value** tree (`JavaMap` /
//!   `from_object`, [`crate::serde`]) the same marker lands as an ordinary
//!   nested `HashMap` with the two reserved keys — harmless to every other
//!   consumer, and read back by unwrapping it.
//! * **Read** — a `JavaBean<Inner>` field deserializes from the nested
//!   object's runtime class, property by property through its getters (`x()`
//!   on a record) — **no class string is needed in the stream**; the class
//!   is derived at runtime, so a read value's `class` string is `""` as
//!   usual. The reserved marker map from the value tree is also readable
//!   (its `value` entry is the plain struct), so a `JavaMap`-written
//!   structure with a `JavaBean` field round-trips.
//!
//! A nested *plain* struct (not a `JavaBean`) still serializes into a
//! `HashMap` value, exactly as in [`crate::serde`] — a setter expecting the
//! concrete class does not accept it, which is the pre-existing loud error.
//!
//! # Errors
//!
//! The mapping never silently skips a field: a struct field whose Java class
//! has no matching property is a **loud error** — a [`JavaError::Serde`]
//! naming the property and the attempted method (e.g. `no getter for
//! property 'active' on bean.User (tried 'getActive' and 'isActive')`).
//! Mismatched schemas are contract violations, so they fail rather than
//! partially filling an object. JNI reflection failures (a missing class, a
//! missing no-arg constructor, a getter/setter that throws) propagate as the
//! existing [`JavaError`] variants.
//!
//! # Not supported
//!
//! * `enum`/unsigned fields — already rejected at the value level
//!   ([`crate::serde`]), unchanged here.
//! * **Non-public accessors** — only public methods are visible to the
//!   reflection (the JavaBeans convention; `Class.getMethods()` semantics).
//!
//! # The `class` string
//!
//! `class` names the *write target*: the class constructed and filled on the
//! write side (both dotted `bean.User` and slash `bean/User` forms are
//! accepted). Reads derive the class from the object itself, so a value read
//! through `FromJava` carries an **empty** `class` string — set `class`
//! explicitly before passing a read value back as an argument.

use std::fmt;

use jni::objects::{
    Global, JClass as JniClass, JObject as JniObject, JObjectArray, JString,
};
use jni::strings::JNIString;
use jni::{Env, JValueOwned};

use crate::call;
use crate::convert::{FromJava, JavaArg, ToJava};
use crate::error::{JavaError, JavaResult};
use crate::handles::JObject;
use crate::serde::{JavaDeserializer, JavaSerializer, SerdeError};
use serde::ser::SerializeStruct as _;

// ---------------------------------------------------------------------------
// The nested-bean serde marker
// ---------------------------------------------------------------------------

/// The serde struct name a [`JavaBean`] serializes as, and its two reserved
/// field names.
///
/// `JavaBean<T>` implements `Serialize`/`Deserialize` so it can appear as a
/// struct field (bean-to-bean nesting). Serializing emits a struct named
/// `JavaBean` with exactly two fields — the class string and the value. The
/// bean write serializer recognizes this struct and builds a *nested bean
/// object*; the value-tree serializer ([`crate::serde`]) treats it as an
/// ordinary nested `HashMap` with two reserved keys (harmless — see the
/// [module docs](self)). The read side routes through
/// `deserialize_newtype_struct("JavaBean", …)`, which the value-tree
/// deserializer intercepts: a bean object is read getter by getter, and the
/// reserved marker map is unwrapped back into the plain struct.
pub(crate) const BEAN_MARKER_STRUCT: &str = "JavaBean";
pub(crate) const BEAN_MARKER_CLASS_KEY: &str = "__rjava_bean_class";
pub(crate) const BEAN_MARKER_VALUE_KEY: &str = "value";

// ---------------------------------------------------------------------------
// The public wrapper
// ---------------------------------------------------------------------------

/// Opt-in bean mapping of a Rust struct to a plain Java object (and back).
///
/// Wraps a serde value so it can travel through the
/// [`ToJava`]/[`FromJava`] machinery without
/// overlapping the existing scalar implementations (a blanket
/// `impl<T: Serialize> ToJava for T` would collide with `String`, the
/// primitives, …).
///
/// * **Write side** — `JavaBean { value, class }` as a method argument
///   constructs `class` via its public no-arg constructor and fills it with
///   one setter call per struct field (`user_id` → `setUserId`; see the
///   [module docs](self) for the camelCase rule, the supported field types
///   and the loud-error contract).
/// * **Read side** — annotate a call result as `JavaBean<T>` to read an
///   existing object back into `T` getter by getter (`get<Name>`, with an
///   `is<Name>` fallback). The read derives the class from the object's
///   runtime class and never consults `class`; a read value therefore
///   carries an **empty** `class` string (set it before writing back).
///   [`JavaBean::from_object`] is the wrapper-free reader.
pub struct JavaBean<T> {
    /// The Rust value being mapped to (or read from) the Java bean.
    pub value: T,
    /// The write-side target class, dotted (`bean.User`) or slash-separated
    /// (`bean/User`). Read results carry `""` — see the [module docs](self).
    pub class: &'static str,
}

impl<T: serde::Serialize> serde::Serialize for JavaBean<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // The reserved marker shape: a struct named `JavaBean` whose two
        // fields are the write-side class string and the value. The bean
        // write serializer turns this into a nested bean object; the value
        // tree keeps it as a nested HashMap with two reserved keys (see the
        // module docs).
        let mut st = serializer.serialize_struct(BEAN_MARKER_STRUCT, 2)?;
        st.serialize_field(BEAN_MARKER_CLASS_KEY, &self.class)?;
        st.serialize_field(BEAN_MARKER_VALUE_KEY, &self.value)?;
        st.end()
    }
}

impl<'de, T: serde::de::DeserializeOwned> serde::Deserialize<'de> for JavaBean<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Reads route through `deserialize_newtype_struct("JavaBean", …)`:
        // the rjava value-tree deserializer intercepts the marker name and
        // reads the held Java value as a bean (nested object → getters,
        // marker map → plain struct), handing the inner deserializer to
        // [`Visitor::visit_newtype_struct`] below. Any other deserializer
        // sees an ordinary newtype around `T`.
        struct JavaBeanVisitor<T>(std::marker::PhantomData<T>);

        impl<'de, T: serde::de::DeserializeOwned> serde::de::Visitor<'de> for JavaBeanVisitor<T> {
            type Value = JavaBean<T>;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str(
                    "a `JavaBean<T>` field: a Java bean object read through its getters, \
                     or the `__rjava_bean_class` marker map read through the value tree",
                )
            }

            fn visit_newtype_struct<D: serde::Deserializer<'de>>(
                self,
                deserializer: D,
            ) -> Result<Self::Value, D::Error> {
                // The class is derived from the object at runtime (or
                // discarded from the marker map); a read value's `class`
                // string is always the write-side-only `""` — see the
                // module docs.
                Ok(JavaBean {
                    value: T::deserialize(deserializer)?,
                    class: "",
                })
            }
        }

        deserializer.deserialize_newtype_struct(
            BEAN_MARKER_STRUCT,
            JavaBeanVisitor(std::marker::PhantomData),
        )
    }
}

impl<T: serde::Serialize> ToJava for JavaBean<T> {
    fn to_java<'env>(&self, env: &mut Env<'env>) -> JavaResult<Vec<JavaArg<'env>>> {
        let obj = self
            .value
            .serialize(BeanSerializer {
                env,
                class: self.class,
            })
            .map_err(SerdeError::into_java_error)?;
        Ok(vec![JavaArg::Object(obj)])
    }
    fn java_args(&self) -> String {
        format!("L{};", self.class.replace('.', "/"))
    }
}

impl<T: serde::de::DeserializeOwned> FromJava for JavaBean<T> {
    fn from_java<'env>(env: &mut Env<'env>, value: JValueOwned<'env>) -> JavaResult<Self> {
        let obj = match value {
            JValueOwned::Object(o) if !o.is_null() => o,
            _ => {
                return Err(JavaError::Serde(
                    "bean deserialization requires a non-null Java object \
                     (got null or a primitive value)"
                        .to_string(),
                ))
            }
        };
        let global = env.new_global_ref(obj)?;
        let de = BeanDeserializer {
            env,
            obj: &global,
        };
        let value = T::deserialize(de).map_err(SerdeError::into_java_error)?;
        Ok(JavaBean { value, class: "" })
    }
    fn java_return_type() -> String {
        // Deliberately generic: the real return type is resolved via the
        // reflection fallback, and `from_java` dispatches on the runtime
        // value.
        String::from("Ljava/lang/Object;")
    }
}

impl<T: serde::de::DeserializeOwned> JavaBean<T> {
    /// Deserialize a Java bean object into `T` through its getters, deriving
    /// the property lookup from the object's **runtime** class.
    ///
    /// This is the wrapper-free reader (the bean analog of
    /// [`crate::serde::from_object`]): `obj` may be any Java object whose
    /// public getters match `T`'s fields (`get<Name>`, with an `is<Name>`
    /// fallback for boolean properties). The `class` string of a
    /// [`JavaBean`] is write-side only and is never consulted here.
    ///
    /// The caller provides an attached JNI environment — e.g. inside a
    /// `with_env` helper over `jni::JavaVM::singleton()` (see the
    /// integration tests).
    pub fn from_object(env: &mut Env<'_>, obj: &JObject) -> JavaResult<T> {
        let de = BeanDeserializer {
            env,
            obj: &obj.global,
        };
        T::deserialize(de).map_err(SerdeError::into_java_error)
    }
}

// ---------------------------------------------------------------------------
// The Serializer
// ---------------------------------------------------------------------------

/// The write-side serializer of [`JavaBean`]: only `serialize_struct`
/// succeeds (it captures one `(field, boxed value)` pair per struct field);
/// every other shape — scalars, sequences, maps, tuples, enums — is rejected
/// because the bean is exactly one struct mapped to one object's properties.
struct BeanSerializer<'a, 'env> {
    env: &'a mut Env<'env>,
    class: &'static str,
}

/// The message for every non-struct serialization shape: the bean shape is
/// exactly one struct per `JavaBean`.
fn not_a_struct_ser() -> SerdeError {
    SerdeError::ser_custom(
        "JavaBean<T> requires `T` to serialize as a struct — bean properties map \
         one-to-one to named struct fields (scalars, sequences, maps, tuples and \
         enums are not bean shapes)",
    )
}

impl<'a, 'env> serde::Serializer for BeanSerializer<'a, 'env> {
    type Ok = JniObject<'env>;
    type Error = SerdeError;
    type SerializeSeq = serde::ser::Impossible<Self::Ok, Self::Error>;
    type SerializeTuple = serde::ser::Impossible<Self::Ok, Self::Error>;
    type SerializeTupleStruct = serde::ser::Impossible<Self::Ok, Self::Error>;
    type SerializeTupleVariant = serde::ser::Impossible<Self::Ok, Self::Error>;
    type SerializeMap = serde::ser::Impossible<Self::Ok, Self::Error>;
    type SerializeStruct = BeanBuilder<'a, 'env>;
    type SerializeStructVariant = serde::ser::Impossible<Self::Ok, Self::Error>;

    fn serialize_bool(self, _v: bool) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_i8(self, _v: i8) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_i16(self, _v: i16) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_i32(self, _v: i32) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_i64(self, _v: i64) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_u8(self, _v: u8) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_u16(self, _v: u16) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_u32(self, _v: u32) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_u64(self, _v: u64) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_f32(self, _v: f32) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_f64(self, _v: f64) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_char(self, _v: char) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_str(self, _v: &str) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_bytes(self, _v: &[u8]) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_some<T: ?Sized + serde::Serialize>(
        self,
        _value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_newtype_struct<T: ?Sized + serde::Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        // `struct Wrapper(Inner)` maps the *inner* value with this same
        // serializer, so a newtype around a struct addresses the bean's
        // properties; a newtype around anything else errors above.
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + serde::Serialize>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_seq(
        self,
        _len: Option<usize>,
    ) -> Result<Self::SerializeSeq, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_map(
        self,
        _len: Option<usize>,
    ) -> Result<Self::SerializeMap, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        let BeanSerializer { env, class } = self;
        Ok(BeanBuilder {
            env,
            class,
            pairs: Vec::new(),
        })
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        Err(not_a_struct_ser())
    }
}

/// Captures one struct field's `(name, boxed value)` pair so that [`end`]
/// can emit one setter call per pair.
///
/// [`end`]: serde::ser::SerializeStruct::end
struct BeanBuilder<'a, 'env> {
    env: &'a mut Env<'env>,
    class: &'static str,
    pairs: Vec<(String, JniObject<'env>)>,
}

impl<'a, 'env> serde::ser::SerializeStruct for BeanBuilder<'a, 'env> {
    type Ok = JniObject<'env>;
    type Error = SerdeError;
    fn serialize_field<T: ?Sized + serde::Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        // The field *value* goes through the marker-aware value serializer
        // (boxed wrappers / String / ArrayList / HashMap / null) — only the
        // top-level struct turns into setters. A nested `JavaBean` field is
        // recognized by its marker struct and built as a nested bean object.
        let v = value.serialize(BeanValueSerializer {
            env: &mut *self.env,
        })?;
        self.pairs.push((key.to_string(), v));
        Ok(())
    }
    fn end(self) -> Result<Self::Ok, Self::Error> {
        let BeanBuilder { env, class, pairs } = self;
        build_bean(env, class, pairs)
    }
}

// ---------------------------------------------------------------------------
// Nested-bean write path (the `JavaBean` serde marker)
// ---------------------------------------------------------------------------

/// The write-side serializer for a bean field's *value*: the existing
/// value-level [`JavaSerializer`] plus one interception — a struct named
/// `JavaBean` (the nested-bean marker, see [`BEAN_MARKER_STRUCT`]) builds a
/// nested bean object instead of a marker `HashMap`. This is what makes a
/// struct field typed `JavaBean<T>` round-trip as a bean rather than a map.
struct BeanValueSerializer<'a, 'env> {
    env: &'a mut Env<'env>,
}

impl<'a, 'env> serde::Serializer for BeanValueSerializer<'a, 'env> {
    type Ok = JniObject<'env>;
    type Error = SerdeError;
    type SerializeSeq = crate::serde::SeqBuilder<'a, 'env>;
    type SerializeTuple = crate::serde::SeqBuilder<'a, 'env>;
    type SerializeTupleStruct = crate::serde::SeqBuilder<'a, 'env>;
    type SerializeTupleVariant = crate::serde::UnsupportedBuilder<'a, 'env>;
    type SerializeMap = crate::serde::MapBuilder<'a, 'env>;
    type SerializeStruct = StructOrMarker<'a, 'env>;
    type SerializeStructVariant = crate::serde::UnsupportedBuilder<'a, 'env>;

    fn serialize_bool(self, v: bool) -> Result<Self::Ok, Self::Error> {
        JavaSerializer { env: self.env }.serialize_bool(v)
    }
    fn serialize_i8(self, v: i8) -> Result<Self::Ok, Self::Error> {
        JavaSerializer { env: self.env }.serialize_i8(v)
    }
    fn serialize_i16(self, v: i16) -> Result<Self::Ok, Self::Error> {
        JavaSerializer { env: self.env }.serialize_i16(v)
    }
    fn serialize_i32(self, v: i32) -> Result<Self::Ok, Self::Error> {
        JavaSerializer { env: self.env }.serialize_i32(v)
    }
    fn serialize_i64(self, v: i64) -> Result<Self::Ok, Self::Error> {
        JavaSerializer { env: self.env }.serialize_i64(v)
    }
    fn serialize_u8(self, v: u8) -> Result<Self::Ok, Self::Error> {
        JavaSerializer { env: self.env }.serialize_u8(v)
    }
    fn serialize_u16(self, v: u16) -> Result<Self::Ok, Self::Error> {
        JavaSerializer { env: self.env }.serialize_u16(v)
    }
    fn serialize_u32(self, v: u32) -> Result<Self::Ok, Self::Error> {
        JavaSerializer { env: self.env }.serialize_u32(v)
    }
    fn serialize_u64(self, v: u64) -> Result<Self::Ok, Self::Error> {
        JavaSerializer { env: self.env }.serialize_u64(v)
    }
    fn serialize_f32(self, v: f32) -> Result<Self::Ok, Self::Error> {
        JavaSerializer { env: self.env }.serialize_f32(v)
    }
    fn serialize_f64(self, v: f64) -> Result<Self::Ok, Self::Error> {
        JavaSerializer { env: self.env }.serialize_f64(v)
    }
    fn serialize_char(self, v: char) -> Result<Self::Ok, Self::Error> {
        JavaSerializer { env: self.env }.serialize_char(v)
    }
    fn serialize_str(self, v: &str) -> Result<Self::Ok, Self::Error> {
        JavaSerializer { env: self.env }.serialize_str(v)
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<Self::Ok, Self::Error> {
        JavaSerializer { env: self.env }.serialize_bytes(v)
    }
    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        JavaSerializer { env: self.env }.serialize_none()
    }
    fn serialize_some<T: ?Sized + serde::Serialize>(
        self,
        value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        JavaSerializer { env: self.env }.serialize_unit()
    }
    fn serialize_unit_struct(self, name: &'static str) -> Result<Self::Ok, Self::Error> {
        JavaSerializer { env: self.env }.serialize_unit_struct(name)
    }
    fn serialize_unit_variant(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        JavaSerializer { env: self.env }.serialize_unit_variant(name, index, variant)
    }
    fn serialize_newtype_struct<T: ?Sized + serde::Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        // `struct Wrapper(Inner)` keeps the marker interception for the
        // inner value (mirroring `BeanSerializer`'s newtype transparency).
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + serde::Serialize>(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        JavaSerializer { env: self.env }.serialize_newtype_variant(name, index, variant, value)
    }
    fn serialize_seq(
        self,
        len: Option<usize>,
    ) -> Result<Self::SerializeSeq, Self::Error> {
        JavaSerializer { env: self.env }.serialize_seq(len)
    }
    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        JavaSerializer { env: self.env }.serialize_tuple(len)
    }
    fn serialize_tuple_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        JavaSerializer { env: self.env }.serialize_tuple_struct(name, len)
    }
    fn serialize_tuple_variant(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        JavaSerializer { env: self.env }.serialize_tuple_variant(name, index, variant, len)
    }
    fn serialize_map(
        self,
        len: Option<usize>,
    ) -> Result<Self::SerializeMap, Self::Error> {
        JavaSerializer { env: self.env }.serialize_map(len)
    }
    fn serialize_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        if name == BEAN_MARKER_STRUCT {
            Ok(StructOrMarker::Marker(MarkerBuilder {
                env: self.env,
                class: None,
                pairs: Vec::new(),
            }))
        } else {
            Ok(StructOrMarker::Map(JavaSerializer { env: self.env }.serialize_struct(name, len)?))
        }
    }
    fn serialize_struct_variant(
        self,
        name: &'static str,
        index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        JavaSerializer { env: self.env }.serialize_struct_variant(name, index, variant, len)
    }
}

/// The `SerializeStruct` of [`BeanValueSerializer`]: either an ordinary
/// struct → `HashMap` (the value-tree behavior) or the nested-bean marker →
/// a nested bean object.
enum StructOrMarker<'a, 'env> {
    Map(crate::serde::MapBuilder<'a, 'env>),
    Marker(MarkerBuilder<'a, 'env>),
}

impl<'a, 'env> serde::ser::SerializeStruct for StructOrMarker<'a, 'env> {
    type Ok = JniObject<'env>;
    type Error = SerdeError;
    fn serialize_field<T: ?Sized + serde::Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        match self {
            StructOrMarker::Map(m) => m.serialize_field(key, value),
            StructOrMarker::Marker(m) => m.serialize_field(key, value),
        }
    }
    fn end(self) -> Result<Self::Ok, Self::Error> {
        match self {
            StructOrMarker::Map(m) => m.end(),
            StructOrMarker::Marker(m) => m.end(),
        }
    }
}

/// Captures one nested `JavaBean` marker: the `__rjava_bean_class` string
/// and the `value` struct's `(name, boxed value)` pairs, so that [`end`]
/// can build the nested bean object (canonical constructor for a record,
/// no-arg constructor + setters otherwise — recursively, a nested `JavaBean`
/// field inside the value builds its own bean object).
///
/// [`end`]: serde::ser::SerializeStruct::end
struct MarkerBuilder<'a, 'env> {
    env: &'a mut Env<'env>,
    class: Option<String>,
    pairs: Vec<(String, JniObject<'env>)>,
}

impl<'a, 'env> serde::ser::SerializeStruct for MarkerBuilder<'a, 'env> {
    type Ok = JniObject<'env>;
    type Error = SerdeError;
    fn serialize_field<T: ?Sized + serde::Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        match key {
            // The marker's class is the `&str` field of the serialized
            // `JavaBean`; read it back out of the stream without keeping a
            // Java value.
            BEAN_MARKER_CLASS_KEY => {
                let v = value.serialize(JavaSerializer {
                    env: &mut *self.env,
                })?;
                self.class = Some(crate::serde::java_string_of(self.env, v)?);
            }
            // The value is the inner struct; capture its field pairs so the
            // nested bean can be filled with one setter call per field.
            BEAN_MARKER_VALUE_KEY => {
                self.pairs = value.serialize(PairCaptureSerializer {
                    env: &mut *self.env,
                })?;
            }
            _ => {
                return Err(SerdeError::ser_custom(format!(
                    "rjava bean: internal error — the `{BEAN_MARKER_STRUCT}` marker struct \
                     emitted an unknown field `{key}`"
                )))
            }
        }
        Ok(())
    }
    fn end(self) -> Result<Self::Ok, Self::Error> {
        let MarkerBuilder { env, class, pairs } = self;
        let class = class.ok_or_else(|| {
            SerdeError::ser_custom(format!(
                "rjava bean: internal error — the `{BEAN_MARKER_STRUCT}` marker struct is \
                 missing its `{BEAN_MARKER_CLASS_KEY}` field"
            ))
        })?;
        build_bean(env, &class, pairs)
    }
}

/// The write-side serializer used for a nested bean marker's `value`: the
/// inner struct's fields are captured as `(name, boxed value)` pairs so the
/// [`MarkerBuilder`] can emit one setter call per field. Any non-struct
/// shape is rejected — a bean's value is exactly one struct.
struct PairCaptureSerializer<'a, 'env> {
    env: &'a mut Env<'env>,
}

impl<'a, 'env> serde::Serializer for PairCaptureSerializer<'a, 'env> {
    type Ok = Vec<(String, JniObject<'env>)>;
    type Error = SerdeError;
    type SerializeSeq = serde::ser::Impossible<Self::Ok, Self::Error>;
    type SerializeTuple = serde::ser::Impossible<Self::Ok, Self::Error>;
    type SerializeTupleStruct = serde::ser::Impossible<Self::Ok, Self::Error>;
    type SerializeTupleVariant = serde::ser::Impossible<Self::Ok, Self::Error>;
    type SerializeMap = serde::ser::Impossible<Self::Ok, Self::Error>;
    type SerializeStruct = PairCaptureBuilder<'a, 'env>;
    type SerializeStructVariant = serde::ser::Impossible<Self::Ok, Self::Error>;

    fn serialize_bool(self, _v: bool) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_i8(self, _v: i8) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_i16(self, _v: i16) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_i32(self, _v: i32) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_i64(self, _v: i64) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_u8(self, _v: u8) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_u16(self, _v: u16) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_u32(self, _v: u32) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_u64(self, _v: u64) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_f32(self, _v: f32) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_f64(self, _v: f64) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_char(self, _v: char) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_str(self, _v: &str) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_bytes(self, _v: &[u8]) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_some<T: ?Sized + serde::Serialize>(
        self,
        _value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_unit_struct(self, _name: &'static str) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_newtype_struct<T: ?Sized + serde::Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        // `struct Wrapper(Inner)` captures the inner value with this same
        // serializer.
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + serde::Serialize>(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        Err(not_a_struct_ser())
    }
    fn serialize_struct(
        self,
        name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        if name == BEAN_MARKER_STRUCT {
            // `JavaBean<JavaBean<X>>` — the marker's value must be a plain
            // struct; a bean nested directly inside another bean's value has
            // no setter shape.
            return Err(SerdeError::ser_custom(
                "rjava bean: a `JavaBean<T>` nested directly inside another `JavaBean<T>`'s \
                 value is not a bean shape — the value must be a plain struct",
            ));
        }
        Ok(PairCaptureBuilder {
            env: self.env,
            pairs: Vec::new(),
        })
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        Err(not_a_struct_ser())
    }
}

/// Captures one struct's `(name, boxed value)` pairs so a [`MarkerBuilder`]
/// can emit one setter call per pair. Field values go through
/// [`BeanValueSerializer`], so a field that is itself a `JavaBean` marker
/// builds a nested bean object (recursion).
struct PairCaptureBuilder<'a, 'env> {
    env: &'a mut Env<'env>,
    pairs: Vec<(String, JniObject<'env>)>,
}

impl<'a, 'env> serde::ser::SerializeStruct for PairCaptureBuilder<'a, 'env> {
    type Ok = Vec<(String, JniObject<'env>)>;
    type Error = SerdeError;
    fn serialize_field<T: ?Sized + serde::Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        let v = value.serialize(BeanValueSerializer {
            env: &mut *self.env,
        })?;
        self.pairs.push((key.to_string(), v));
        Ok(())
    }
    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(self.pairs)
    }
}

// ---------------------------------------------------------------------------
// Object construction

/// Construct `class` with its public no-arg constructor and fill it with one
/// setter call per captured `(property, value)` pair. A property whose
/// setter does not exist — or does not accept the serialized value — is a
/// loud error naming the property and the attempted method; fields are never
/// silently skipped. A **record** class is constructed through its canonical
/// constructor instead (no setters — see [the module docs](self)); the
/// struct's field order must match the record's component order, checked
/// eagerly.
fn build_bean<'env>(
    env: &mut Env<'env>,
    class: &str,
    pairs: Vec<(String, JniObject<'env>)>,
) -> Result<JniObject<'env>, SerdeError> {
    let cls = call::find_class(env, JNIString::from(class.replace('.', "/")))?;
    let cls_global = env.new_global_ref(cls)?;
    let cls_local: JniClass = env.new_local_ref(&*cls_global)?;
    if is_record_class(env, &cls_local)? {
        return build_record(env, class, &cls_global, pairs);
    }
    let obj = call::new_object(env, &cls_global, &())?;
    for (field, value) in pairs {
        let setter = format!("set{}", camel_case(&field));
        match call::call_method_raw(env, &obj.global, &setter, vec![JavaArg::Object(value)]) {
            Ok(_) => {}
            Err(JavaError::InvalidArgument(_)) => {
                return Err(SerdeError::ser_custom(format!(
                    "rjava bean `{class}`: no setter for property `{field}` accepting the \
                     serialized value (tried `{setter}` — the setter must be a public method \
                     whose parameter type accepts the value: a boxed primitive does not widen \
                     to a wider primitive, and `null` only matches a reference parameter)"
                )));
            }
            Err(e) => return Err(SerdeError::from(e)),
        }
    }
    Ok(env.new_local_ref(&*obj.global)?)
}

/// Construct a Java **record** through its canonical constructor, with the
/// struct's field values in declaration order (the canonical constructor's
/// parameter order is the record's component order). The struct field names
/// are checked against `Class.getRecordComponents()` **in order** first: a
/// mismatch is a loud error naming both orders, because the values would
/// silently land in the wrong components otherwise.
fn build_record<'env>(
    env: &mut Env<'env>,
    class: &str,
    cls_global: &Global<JniClass<'static>>,
    pairs: Vec<(String, JniObject<'env>)>,
) -> Result<JniObject<'env>, SerdeError> {
    let cls_local: JniClass = env.new_local_ref(cls_global)?;
    let components = record_component_names(env, &cls_local)?;
    let names: Vec<String> = pairs.iter().map(|(n, _)| n.clone()).collect();
    if components != names {
        return Err(SerdeError::ser_custom(format!(
            "rjava bean `{class}` is a Java record: the Rust struct's field order must match \
             the record's component order (components: {}) but the struct serialized fields: {}",
            components.join(", "),
            names.join(", "),
        )));
    }
    let args: Vec<JavaArg> = pairs.into_iter().map(|(_, v)| JavaArg::Object(v)).collect();
    let obj = call::new_object_raw(env, cls_global, args)?;
    Ok(env.new_local_ref(&*obj.global)?)
}

/// Does the class `class` represent a Java record?
///
/// `Class.isRecord()` exists since Java 16. On an older JVM the lookup fails
/// (either as a JNI `MethodNotFound` or as a pending `NoSuchMethodError`
/// exception, depending on the JVM); both are treated as "not a record" —
/// the record mapping documents **Java 16 as its floor**, and a pre-16 class
/// cannot be a record.
fn is_record_class<'env>(env: &mut Env<'env>, class: &JniClass<'env>) -> JavaResult<bool> {
    match call::with_check(env, |env| {
        env.call_method(class, jni::jni_str!("isRecord"), jni::jni_sig!("()Z"), &[])
    }) {
        Ok(JValueOwned::Bool(b)) => Ok(b),
        Ok(_) => Err(JavaError::InvalidArgument(
            "internal error: Class.isRecord() did not return a boolean",
        )),
        Err(JavaError::Jni(jni::errors::Error::MethodNotFound { .. })) => Ok(false),
        Err(JavaError::JavaException { class, .. }) if class == "java.lang.NoSuchMethodError" => {
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

/// `Class.getRecordComponents()` → the record's component names in
/// declaration order (the canonical constructor's parameter order).
fn record_component_names<'env>(
    env: &mut Env<'env>,
    class: &JniClass<'env>,
) -> JavaResult<Vec<String>> {
    let comps: JValueOwned = call::with_check(env, |env| {
        env.call_method(
            class,
            jni::jni_str!("getRecordComponents"),
            jni::jni_sig!("()[Ljava/lang/reflect/RecordComponent;"),
            &[],
        )
    })?;
    let arr: JObjectArray<'env> = match comps {
        JValueOwned::Object(o) => JObjectArray::<JniObject>::cast_local(env, o)?,
        _ => {
            return Err(JavaError::InvalidArgument(
                "internal error: Class.getRecordComponents() did not return an array",
            ))
        }
    };
    let n = arr.len(env)?;
    let mut names = Vec::with_capacity(n);
    for i in 0..n {
        let rc: JniObject = arr.get_element(env, i)?;
        let name: JValueOwned = call::with_check(env, |env| {
            env.call_method(&rc, jni::jni_str!("getName"), jni::jni_sig!("()Ljava/lang/String;"), &[])
        })?;
        let js = match name {
            JValueOwned::Object(o) => env.cast_local::<JString>(o)?,
            _ => {
                return Err(JavaError::InvalidArgument(
                    "internal error: RecordComponent.getName() did not return a String",
                ))
            }
        };
        names.push(js.mutf8_chars(env)?.into());
    }
    Ok(names)
}

// ---------------------------------------------------------------------------
// The Deserializer
// ---------------------------------------------------------------------------

/// The read-side deserializer of [`JavaBean`]: `deserialize_struct`
/// enumerates the struct's field names and reads each property through its
/// getter, feeding every getter result through the value-level
/// [`JavaDeserializer`]. Any non-struct target errors — a bean has no
/// scalar, sequence or map shape.
///
/// `pub(crate)` because the value-tree deserializer ([`crate::serde`])
/// constructs it for a nested `JavaBean<T>` field: a bean object is read
/// through its getters, deriving the property lookup from the object's
/// runtime class.
pub(crate) struct BeanDeserializer<'a, 'env> {
    pub(crate) env: &'a mut Env<'env>,
    pub(crate) obj: &'a Global<JniObject<'static>>,
}

/// The message for every non-struct deserialization shape.
fn not_a_struct_de() -> SerdeError {
    SerdeError::de_custom(
        "JavaBean<T> requires `T` to be a struct — bean properties map one-to-one to \
         named struct fields, read through the getters `get<Name>` / `is<Name>` \
         (`x()` on a record)",
    )
}

impl<'de, 'a, 'env> serde::de::Deserializer<'de> for BeanDeserializer<'a, 'env> {
    type Error = SerdeError;

    fn deserialize_any<V: serde::de::Visitor<'de>>(
        self,
        _visitor: V,
    ) -> Result<V::Value, Self::Error> {
        // Without the field list there is no property to enumerate; the
        // struct shape is the only supported read.
        Err(not_a_struct_de())
    }

    fn deserialize_struct<V: serde::de::Visitor<'de>>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        // Whether the object's runtime class is a record is decided once per
        // struct read; it selects the extra `x()` accessor fallback for each
        // property (see [`getter_value`]).
        let obj_local = self.env.new_local_ref(self.obj)?;
        let cls = call::get_object_class(self.env, &obj_local)?;
        let record = is_record_class(self.env, &cls)?;
        visitor.visit_map(BeanFieldAccess {
            env: self.env,
            obj: self.obj,
            fields,
            index: 0,
            record,
        })
    }

    fn deserialize_newtype_struct<V: serde::de::Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        // `struct Wrapper(Inner)` reads the inner value with this same
        // deserializer, so a newtype around a struct maps the bean's fields.
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_ignored_any<V: serde::de::Visitor<'de>>(
        self,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.deserialize_any(visitor)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct seq tuple tuple_struct map enum
        identifier
    }
}

/// Sequential key/value access over a bean object's properties: the keys are
/// the struct's field names (in declaration order), the values are read from
/// the object's getters. `record` (the precomputed `Class.isRecord()` answer
/// for the object's runtime class) selects the extra `x()` accessor fallback.
struct BeanFieldAccess<'a, 'env> {
    env: &'a mut Env<'env>,
    obj: &'a Global<JniObject<'static>>,
    fields: &'static [&'static str],
    index: usize,
    record: bool,
}

impl<'de, 'a, 'env> serde::de::MapAccess<'de> for BeanFieldAccess<'a, 'env> {
    type Error = SerdeError;
    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: serde::de::DeserializeSeed<'de>,
    {
        if self.index >= self.fields.len() {
            return Ok(None);
        }
        seed.deserialize(FieldName(self.fields[self.index]))
            .map(Some)
    }
    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::DeserializeSeed<'de>,
    {
        let field = self.fields[self.index];
        let value = getter_value(self.env, self.obj, field, self.record)?;
        self.index += 1;
        let de = JavaDeserializer {
            env: &mut *self.env,
            value,
        };
        seed.deserialize(de)
    }
    fn size_hint(&self) -> Option<usize> {
        Some(self.fields.len().saturating_sub(self.index))
    }
}

/// A `Deserializer` whose only job is to hand a struct's field name to the
/// visitor — the `DeserializeSeed` serde generates for the field-identifier
/// enum of a derived struct.
struct FieldName(&'static str);

impl<'de> serde::Deserializer<'de> for FieldName {
    type Error = SerdeError;
    fn deserialize_any<V: serde::de::Visitor<'de>>(
        self,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_str(self.0)
    }
    fn deserialize_identifier<V: serde::de::Visitor<'de>>(
        self,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_str(self.0)
    }
    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct enum ignored_any
    }
}

/// Read the property `field` through its getter: `get<Name>` first, then —
/// the JavaBeans boolean convention — `is<Name>`, and finally — when the
/// object's class is a **record** (see [`is_record_class`]) — the component
/// accessor `<name>()`. The order is deterministic and documented: a record
/// may declare `get<Name>`/`is<Name>` methods as extras, so they win over
/// the component accessor; a non-record bean is never probed for `<name>()`,
/// so a plain property cannot collide with an unrelated no-argument method
/// (`wait()` on `Object`, say). All accessors are read with the raw-value
/// call helper, so a primitive `long` return stays a raw `JValueOwned` for
/// the value-level deserializer to dispatch on. A field with none of the
/// accessors is a loud error naming the property and every attempted method.
fn getter_value<'env>(
    env: &mut Env<'env>,
    obj: &Global<JniObject<'static>>,
    field: &str,
    record: bool,
) -> Result<JValueOwned<'env>, SerdeError> {
    let getter = format!("get{}", camel_case(field));
    match call::call_method_raw(env, obj, &getter, Vec::new()) {
        Ok(value) => Ok(value),
        Err(JavaError::InvalidArgument(_)) => {
            let is_getter = format!("is{}", camel_case(field));
            match call::call_method_raw(env, obj, &is_getter, Vec::new()) {
                Ok(value) => Ok(value),
                Err(JavaError::InvalidArgument(_)) => {
                    if record {
                        // The record accessor is the component accessor
                        // `<name>()` — no prefix.
                        match call::call_method_raw(env, obj, field, Vec::new()) {
                            Ok(value) => return Ok(value),
                            Err(JavaError::InvalidArgument(_)) => {}
                            Err(e) => return Err(SerdeError::from(e)),
                        }
                    }
                    let class = object_class_name(env, obj)
                        .unwrap_or_else(|_| "<unknown class>".to_string());
                    let tried = if record {
                        format!("`{field}()`, `{getter}` and `{is_getter}`")
                    } else {
                        format!("`{getter}` and `{is_getter}`")
                    };
                    Err(SerdeError::de_custom(format!(
                        "rjava bean: no getter for property `{field}` on {class} (tried {tried})"
                    )))
                }
                Err(e) => Err(SerdeError::from(e)),
            }
        }
        Err(e) => Err(SerdeError::from(e)),
    }
}

/// The runtime binary name of `obj` (`Object.getClass().getName()`), used to
/// enrich error messages with the offending class.
fn object_class_name<'env>(
    env: &mut Env<'env>,
    obj: &Global<JniObject<'static>>,
) -> JavaResult<String> {
    let local = env.new_local_ref(obj)?;
    let cls = call::get_object_class(env, &local)?;
    call::class_name(env, &cls)
}

// ---------------------------------------------------------------------------
// camelCase
// ---------------------------------------------------------------------------

/// CamelCase a field name with the crate's simple word-boundary rule: each
/// `_`-separated word starts with an uppercase letter, everything else is
/// kept verbatim. `user_id` → `UserId`, `id` → `Id`, `url` → `Url` — no
/// acronym special-casing. The bean accessors are `set`/`get`/`is` + the
/// camelCased name.
fn camel_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut word_start = true;
    for ch in name.chars() {
        if ch == '_' {
            word_start = true;
        } else if word_start {
            out.extend(ch.to_uppercase());
            word_start = false;
        } else {
            out.push(ch);
        }
    }
    out
}
