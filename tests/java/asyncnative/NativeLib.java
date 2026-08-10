package asyncnative;

import java.util.concurrent.CompletableFuture;

/**
 * Fixture for rjava's {@code async_native!} integration tests: Rust async
 * functions registered as static native methods whose Java return type is a
 * {@code CompletableFuture}.
 */
public class NativeLib {
    // A=(i32, i32), R=i32 — registered with a Rust fn item.
    public static native CompletableFuture<Integer> compute(int a, int b);

    // A=(String,), R=String — registered with a Rust closure (the closure
    // form). Different argument-tuple type from compute, so no collision.
    public static native CompletableFuture<String> shout(String s);

    // A=(String, i32), R=String — the Rust impl returns Err.
    public static native CompletableFuture<String> fail(String msg, int code);

    // A=(), R=String — the Rust impl panics.
    public static native CompletableFuture<String> boom();

    // A=(i32,), R=String — the Rust future parks the polling thread in poll
    // and only completes when its waker is invoked (the classic noop-waker
    // breaker; async-std internals do this).
    public static native CompletableFuture<String> parked(int n);

    // A=(i64,), R=String — the Rust future awaits a tokio timer, so it only
    // completes when the worker runs inside a tokio runtime (with the
    // `tokio` feature).
    public static native CompletableFuture<String> tokioSleep(long ms);

    /**
     * A Java host method that chains two Rust-completed futures through the
     * normal Java future combinators: proves Java-side future semantics
     * (thenApply runs once the Rust worker completes the native future).
     */
    public static CompletableFuture<String> chain(int a, int b) {
        return compute(a, b).thenApply(n -> "sum=" + n);
    }

    /**
     * Calls the Rust-native {@code fail} and attaches a Java-side exception
     * handler: the exceptional completion (an IllegalArgumentException
     * materialized by the Rust worker) is caught by Java code. The
     * {@code exceptionally} handler receives the decoded throwable (the
     * {@code CompletionException} wrapper is unwrapped), so it reads the
     * message straight off the exception.
     */
    public static CompletableFuture<String> handledFail(String msg, int code) {
        return fail(msg, code).exceptionally(ex -> "caught:" + ex.getMessage());
    }
}
