package com.example;

import java.util.concurrent.CompletableFuture;

/**
 * Fixture for rjava's {@code register_natives!} batch-macro tests: a mix of
 * static, instance and async native methods registered in one batch call.
 */
public class BatchLib {
    // Static native (native!): type-derived signature (II)I.
    public static native int add(int a, int b);

    // Instance native (native_inst!): `this` is the first tuple element; the
    // derived descriptor strips the receiver, leaving (I)I. Reads `base`.
    public native int times(int factor);

    public int base;

    // Async native (async_native!): Java sees a CompletableFuture<Integer>
    // completed by the Rust worker thread.
    public static native CompletableFuture<Integer> compute(int a, int b);

    // Static native (native!) with a distinct signature, for the single-item
    // batch test.
    public static native int negate(int x);
}
