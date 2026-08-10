package constructor_bind;

/**
 * Fixture for rjava's "direct new" pattern: the Java constructor binds the
 * Rust state itself. The constructor is plain Java (a `native` constructor
 * is illegal in Java source — the JLS forbids it, and class-file rewriting
 * is out of scope), but its body calls the native binder `init()`, so
 * `new DirectCounter()` yields an already-bound object — no factory, no
 * post-construction bind.
 */
public class DirectCounter {
    public DirectCounter() { init(); }   // plain Java ctor calls the native binder
    private native void init();          // binds Rust state to `this`
    public native long add(int by);      // returns the new value
}
