package runmain;

public class MainProbe {
    public static void main(String[] args) {
        System.setProperty("rjava.main.args", String.join(",", args));
    }
}
