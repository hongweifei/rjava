package rjava.shell;

/**
 * The one fixed JVM-side class behind rjava's Java-interface feature
 * (cargo feature {@code interface}, module {@code rjava::interface}).
 *
 * <p>rjava implements Java interfaces from Rust with <b>zero user-written
 * Java implementation classes</b>: the JDK's own
 * {@code java.lang.reflect.Proxy} generates the interface implementation at
 * runtime, and every generated proxy delegates to one shared
 * {@code InvocationHandler} — this class. Its {@code invoke} method is
 * {@code native} and routes (method name, arguments) to the Rust closure the
 * host registered for the proxy; the closure's state lives in rjava's
 * userdata registry, keyed by this object's identity and released
 * automatically when the proxy (and therefore this shell) becomes
 * unreachable.
 *
 * <p>The constructor binds the Rust handler state to {@code this} via the
 * native {@code init()} — the ctor-bind pattern (the JLS forbids {@code
 * native} constructors, so the binding happens from the constructor body).
 * The host must have registered the {@code init}/{@code invoke} natives and
 * armed the pending-handler slot before constructing (that is exactly what
 * {@code rjava::interface::proxy} does); constructing one of these shells
 * any other way fails with a Java {@code RuntimeException} from {@code
 * init()}.
 *
 * <p>This file is compiled once with {@code javac --release 8} (class-file
 * version 52, Java 8) and the resulting {@code .class} is committed to the
 * repository at {@code interface/java/rjava/shell/InvocationHandlerShell.class};
 * the rjava library embeds those bytes, so building rjava with the
 * {@code interface} feature needs no JDK. <b>After editing this file,
 * recompile and commit the {@code .class}:</b>
 * {@code javac --release 8 -d <tmp> interface/java/rjava/shell/InvocationHandlerShell.java}
 * and move {@code <tmp>/rjava/shell/InvocationHandlerShell.class} into place;
 * a regression test fails the build if the committed {@code .class} is stale.
 * At first use rjava writes the embedded bytes to a per-process temp
 * directory and loads the class through a {@code URLClassLoader} — no Java
 * code is ever generated or compiled at runtime.
 *
 * <p>This is the only Java source file in the crate. Do not add more: the
 * entire design depends on the JDK's {@code Proxy} machinery doing the
 * interface-implementation work.
 */
public final class InvocationHandlerShell implements java.lang.reflect.InvocationHandler {

    /** Bind the pending Rust handler state to this shell (see class docs). */
    public InvocationHandlerShell() {
        init();
    }

    private native void init();

    /**
     * Routes a proxied interface method call to the Rust handler bound to
     * {@code this}: the handler receives the method name and the (boxed)
     * argument values and returns the (boxed) result, or an exception to
     * throw into the caller. Implemented natively by rjava.
     */
    public native Object invoke(Object proxy, java.lang.reflect.Method method, Object[] args);
}
