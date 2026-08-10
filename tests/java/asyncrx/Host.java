/**
 * Fixture for the async integration tests (tests/async_rx.rs): holds a
 * {@link Listener} and fires its events from a background Java thread after
 * a delay — the "event arrives from a Java thread, Rust awaits" shape the
 * `rjava::rx` bridge exists for.
 */
public class Host {
    private final Listener listener;

    public Host(Listener listener) {
        this.listener = listener;
    }

    /** Fires {@code onEvent(value)} on a fresh daemon thread after a delay. */
    public void fireEventAfter(long delayMs, String value) {
        Thread t = new Thread(() -> {
            try {
                Thread.sleep(delayMs);
                listener.onEvent(value);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
        });
        t.setDaemon(true);
        t.start();
    }

    /** Fires {@code onDone(n)} on a fresh daemon thread after a delay. */
    public void fireDoneAfter(long delayMs, int n) {
        Thread t = new Thread(() -> {
            try {
                Thread.sleep(delayMs);
                listener.onDone(n);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
        });
        t.setDaemon(true);
        t.start();
    }

    /** Fires {@code onEvent(value)} then {@code onDone(n)}, each after a
     *  delay, on one fresh daemon thread — the "first event wins" shape. */
    public void fireBothAfter(long delayMs, String value, int n) {
        Thread t = new Thread(() -> {
            try {
                Thread.sleep(delayMs);
                listener.onEvent(value);
                Thread.sleep(delayMs);
                listener.onDone(n);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
        });
        t.setDaemon(true);
        t.start();
    }
}
