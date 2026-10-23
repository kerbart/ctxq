package sample

import sample.tools.format

class Outer<T : Box<Box<String>>> {
    class Inner {
        fun render(
            value: String,
            count: Int,
        ): String {
            return format("${value}: ${count} { // }")
        }
    }

    fun expression(value: T): String = format(value.toString())

    fun raw(): String = """
        { ${expressionSample} // text }
    """.trimIndent()
}
