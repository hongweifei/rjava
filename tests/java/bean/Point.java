package bean;

/**
 * A Java record used by the record-mapping tests (tests/bean_map.rs):
 * components {@code x}, {@code y}, {@code label} with the canonical
 * constructor {@code Point(int, int, String)} and **no** no-arg constructor,
 * so writing a {@code JavaBean} with class {@code bean.Point} must go
 * through the canonical constructor rather than {@code new + setters}
 * (records have no setters). Reading uses the component accessors
 * {@code x()}, {@code y()}, {@code label()} — there are deliberately no
 * {@code getX}/{@code isX} accessors, so the record {@code <name>()}
 * fallback is exercised.
 */
public record Point(int x, int y, String label) {
    /** Test hook: returns the same instance. */
    public static Point echo(Point p) {
        return p;
    }
}
