using System;
using Collections = System.Collections.Generic;

namespace Sample;

[Obsolete]
public record Box<T>(T Value);

public class Service<T>
    where T : Box<Box<string>>
{
    private readonly string _text = "{ // not a comment }";

    [Obsolete]
    public
    async
    Task<Result<List<int>>> LoadAsync(
        T value,
        int count)
    {
        var escaped = @"{ ""//"" }"; var interpolated = $"{{ {value.ToString()} }}"; Logger.Write(_text);
        return await ConvertAsync(value, count);
    }

    public int Count(T value) => Measure(value);
}
