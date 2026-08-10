/**
 * Fixture for the async integration tests (tests/async_rx.rs): a Java
 * listener interface whose events are bridged to Rust futures by
 * `rjava::rx::from_callback`.
 */
public interface Listener {
    void onEvent(String value);

    void onDone(int n);
}
