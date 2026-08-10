package com.example.plugin;

/**
 * A plugin compiled against the API jar: it implements the API interface and
 * calls the API's Rust-backed Bridge. It must NOT be on the JVM's system
 * class path — the host loads this jar at runtime.
 */
public class HelloPlugin implements com.example.plugin.api.Plugin {
    public String name() {
        return "HelloPlugin(" + com.example.plugin.api.Bridge.rustEcho("hi") + ")";
    }
}
