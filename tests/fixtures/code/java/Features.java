package sample;

import java.util.List;

@Deprecated
public record Result<T>(T value) {}

public class Features<A extends List<List<String>>>
{
    private final String text = "{ // }";

    public static class Inner
    {
        public String map(
            String value,
            int count
        )
        {
            String block = """
                { // text }
                """;
            return Helpers.format(value, count);
        }
    }

    public Result<A> make(A value) {
        return new Result<>(value);
    }
}
