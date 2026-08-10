package com.example;

/**
 * Fixture for rjava's userdata facility: a Java "shell" object whose state
 * lives in Rust. No handle field — the registry addresses it by the object's
 * own identity (System.identityHashCode), so the shell is a plain Java class
 * and Java code can even construct it with `new` (the host binds state
 * later).
 */
public class Counter {
    public static native Counter create();
    public native long increment(long by);
    public native long value();
}
