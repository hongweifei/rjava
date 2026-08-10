package com.example.plugin.api;

/**
 * The API's bridge: native methods the Rust host injects implementations for
 * via RegisterNatives, after loading this jar at runtime.
 */
public class Bridge {
    public static native String rustEcho(String s);
}
