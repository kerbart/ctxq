from tools.formatting import render, clean


@registered("worker")
class Worker:
    """Coordinates { work } and // text."""

    @traced
    def run(
        self,
        value: str,
    ) -> str:
        """Render one value."""
        message = f"value={format_value({value!r})} {{literal}}"
        return render(message)


def helper(value):
    return clean(value)
