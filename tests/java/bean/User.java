package bean;

/**
 * A plain Java bean used by the bean-mapping tests (tests/bean_map.rs):
 * long / String / boolean properties with public getters and setters, a
 * public no-arg constructor, and a static {@code echo} hook so tests can
 * receive a bean built by {@code JavaBean} and return it for reading.
 *
 * <p>Note the boolean property uses the JavaBeans convention
 * {@code isActive()} — there is deliberately no {@code getActive()}, so the
 * mapping's {@code is<Name>} fallback is exercised. {@code getClass()} is
 * inherited from {@code Object}; no struct field is ever named {@code class},
 * so the mapping never touches it.
 */
public class User {
    private long id;
    private String name;
    private boolean active;

    public User() {}

    public User(long id, String name, boolean active) {
        this.id = id;
        this.name = name;
        this.active = active;
    }

    public long getId() {
        return id;
    }

    public void setId(long id) {
        this.id = id;
    }

    public String getName() {
        return name;
    }

    public void setName(String name) {
        this.name = name;
    }

    public boolean isActive() {
        return active;
    }

    public void setActive(boolean active) {
        this.active = active;
    }

    /** Test hook: returns the same instance, so tests can pass a bean built
     *  by {@code JavaBean} through a Java call and read it back. */
    public static User echo(User u) {
        return u;
    }

    @Override
    public String toString() {
        return "User(" + id + ", " + name + ", " + active + ")";
    }
}
