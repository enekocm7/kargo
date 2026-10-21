// file: kotlin/Core.kt
package kotlin

/** The root of the Kotlin class hierarchy. Every Kotlin class has [Any] as a superclass. */
public open class Any {
    /** Indicates whether some [other] object is "equal to" this one. */
    public open operator fun equals(other: Any?): Boolean
    /** Returns a hash code value for the object. */
    public open fun hashCode(): Int
    /** Returns a string representation of the object. */
    public open fun toString(): String
}

/** Nothing has no instances. */
public class Nothing private constructor()

/** The type with only one value: the `Unit` object. */
public object Unit

/** Classes which inherit from this interface have a defined total ordering between their instances. */
public interface Comparable<in T> {
    /** Compares this object with the specified object for order. */
    public operator fun compareTo(other: T): Int
}

/** Represents a readable sequence of [Char] values. */
public interface CharSequence {
    /** Returns the length of this character sequence. */
    public val length: Int
    /** Returns the character at the specified [index] in this character sequence. */
    public operator fun get(index: Int): Char
    /** Returns a new character sequence that is a subsequence of this character sequence. */
    public fun subSequence(startIndex: Int, endIndex: Int): CharSequence
}

/** The `String` class represents character strings. */
public class String : Comparable<String>, CharSequence {
    public companion object
    /** Returns a string obtained by concatenating this string with the string representation of the given [other] object. */
    public operator fun plus(other: Any?): String
    public override val length: Int
    public override fun get(index: Int): Char
    public override fun subSequence(startIndex: Int, endIndex: Int): CharSequence
    public override fun compareTo(other: String): Int
}

/** Superclass for all platform classes representing numeric values. */
public abstract class Number {
    public abstract fun toDouble(): Double
    public abstract fun toFloat(): Float
    public abstract fun toLong(): Long
    public abstract fun toInt(): Int
    public abstract fun toShort(): Short
    public abstract fun toByte(): Byte
}

/** Represents a 32-bit signed integer. */
public class Int : Number(), Comparable<Int> {
    public companion object {
        /** A constant holding the minimum value an instance of Int can have. */
        public const val MIN_VALUE: Int = -2147483648
        /** A constant holding the maximum value an instance of Int can have. */
        public const val MAX_VALUE: Int = 2147483647
    }
    public override operator fun compareTo(other: Int): Int
    public operator fun plus(other: Int): Int
    public operator fun minus(other: Int): Int
    public operator fun times(other: Int): Int
    public operator fun div(other: Int): Int
    public operator fun rem(other: Int): Int
    public operator fun inc(): Int
    public operator fun dec(): Int
    public operator fun unaryMinus(): Int
    public operator fun rangeTo(other: Int): kotlin.ranges.IntRange
    public infix fun shl(bitCount: Int): Int
    public infix fun shr(bitCount: Int): Int
    public infix fun and(other: Int): Int
    public infix fun or(other: Int): Int
    public infix fun xor(other: Int): Int
    public override fun toDouble(): Double
    public override fun toFloat(): Float
    public override fun toLong(): Long
    public override fun toInt(): Int
    public override fun toShort(): Short
    public override fun toByte(): Byte
    public fun toChar(): Char
}

/** Represents a 64-bit signed integer. */
public class Long : Number(), Comparable<Long> {
    public override operator fun compareTo(other: Long): Int
    public operator fun plus(other: Long): Long
    public operator fun minus(other: Long): Long
    public operator fun times(other: Long): Long
    public operator fun div(other: Long): Long
    public operator fun rangeTo(other: Long): kotlin.ranges.LongRange
    public override fun toDouble(): Double
    public override fun toFloat(): Float
    public override fun toLong(): Long
    public override fun toInt(): Int
    public override fun toShort(): Short
    public override fun toByte(): Byte
}

/** Represents a double-precision 64-bit IEEE 754 floating point number. */
public class Double : Number(), Comparable<Double> {
    public override operator fun compareTo(other: Double): Int
    public operator fun plus(other: Double): Double
    public operator fun minus(other: Double): Double
    public operator fun times(other: Double): Double
    public operator fun div(other: Double): Double
    public override fun toDouble(): Double
    public override fun toFloat(): Float
    public override fun toLong(): Long
    public override fun toInt(): Int
    public override fun toShort(): Short
    public override fun toByte(): Byte
}

/** Represents a single-precision 32-bit IEEE 754 floating point number. */
public class Float : Number(), Comparable<Float> {
    public override operator fun compareTo(other: Float): Int
    public override fun toDouble(): Double
    public override fun toFloat(): Float
    public override fun toLong(): Long
    public override fun toInt(): Int
    public override fun toShort(): Short
    public override fun toByte(): Byte
}

/** Represents a 16-bit signed integer. */
public class Short : Number(), Comparable<Short> {
    public override operator fun compareTo(other: Short): Int
    public override fun toDouble(): Double
    public override fun toFloat(): Float
    public override fun toLong(): Long
    public override fun toInt(): Int
    public override fun toShort(): Short
    public override fun toByte(): Byte
}

/** Represents an 8-bit signed integer. */
public class Byte : Number(), Comparable<Byte> {
    public override operator fun compareTo(other: Byte): Int
    public override fun toDouble(): Double
    public override fun toFloat(): Float
    public override fun toLong(): Long
    public override fun toInt(): Int
    public override fun toShort(): Short
    public override fun toByte(): Byte
}

/** Represents a 16-bit Unicode character. */
public class Char : Comparable<Char> {
    public override fun compareTo(other: Char): Int
    public operator fun rangeTo(other: Char): kotlin.ranges.CharRange
    /** The code of this Char. */
    public val code: Int
}

/** Represents a value which is either `true` or `false`. */
public class Boolean : Comparable<Boolean> {
    public operator fun not(): Boolean
    public infix fun and(other: Boolean): Boolean
    public infix fun or(other: Boolean): Boolean
    public infix fun xor(other: Boolean): Boolean
    public override fun compareTo(other: Boolean): Int
}

/** Represents an array. */
public class Array<T>(size: Int, init: (Int) -> T) {
    /** Returns the array element at the specified [index]. */
    public operator fun get(index: Int): T
    /** Sets the array element at the specified [index] to the specified [value]. */
    public operator fun set(index: Int, value: T): Unit
    /** Returns the number of elements in the array. */
    public val size: Int
    /** Creates an iterator for iterating over the elements of the array. */
    public operator fun iterator(): Iterator<T>
}

/** The base class for all errors and exceptions. */
public open class Throwable(public open val message: String?, public open val cause: Throwable?) {
    public constructor(message: String?) : this(message, null)
    public constructor() : this(null, null)
    /** Prints the stack trace of this throwable to the standard output. */
    public fun printStackTrace(): Unit
}

public open class Exception(message: String?, cause: Throwable?) : Throwable(message, cause)
public open class RuntimeException(message: String?, cause: Throwable?) : Exception(message, cause)
public open class IllegalArgumentException(message: String?) : RuntimeException(message, null)
public open class IllegalStateException(message: String?) : RuntimeException(message, null)
public open class IndexOutOfBoundsException(message: String?) : RuntimeException(message, null)
public open class UnsupportedOperationException(message: String?) : RuntimeException(message, null)
public open class NumberFormatException(message: String?) : IllegalArgumentException(message)
public class NotImplementedError(message: String = "An operation is not implemented.") : Error(message)
public open class Error(message: String?) : Throwable(message)

/** The common base class of all enum classes. */
public abstract class Enum<E : Enum<E>>(name: String, ordinal: Int) : Comparable<E> {
    /** Returns the name of this enum constant, exactly as declared in its enum declaration. */
    public val name: String
    /** Returns the ordinal of this enumeration constant. */
    public val ordinal: Int
    public final override fun compareTo(other: E): Int
}

/** Represents a generic pair of two values. */
public data class Pair<out A, out B>(public val first: A, public val second: B) : java.io.Serializable {
    public override fun toString(): String = "($first, $second)"
}

/** Represents a triad of values. */
public data class Triple<out A, out B, out C>(public val first: A, public val second: B, public val third: C)

/** Represents a value with lazy initialization. */
public interface Lazy<out T> {
    /** Gets the lazily initialized value of the current Lazy instance. */
    public val value: T
    /** Returns `true` if a value for this Lazy instance has been already initialized. */
    public fun isInitialized(): Boolean
}

/** Creates a tuple of type [Pair] from this and [that]. */
public infix fun <A, B> A.to(that: B): Pair<A, B> = Pair(this, that)

/** Creates a new instance of the [Lazy] that uses the specified initialization function [initializer]. */
public fun <T> lazy(initializer: () -> T): Lazy<T>
/** Creates a new instance of the [Lazy] that uses the specified initialization function [initializer] and the thread-safety [mode]. */
public fun <T> lazy(mode: LazyThreadSafetyMode, initializer: () -> T): Lazy<T>
/** Specifies how a [Lazy] instance synchronizes initialization among multiple threads. */
public enum class LazyThreadSafetyMode { SYNCHRONIZED, PUBLICATION, NONE }
/** Gets the value of a property delegated to a [Lazy] instance. */
public inline operator fun <T> Lazy<T>.getValue(thisRef: Any?, property: kotlin.reflect.KProperty<*>): T

/** Calls the specified function [block] with `this` value as its argument and returns its result. */
public inline fun <T, R> T.let(block: (T) -> R): R
/** Calls the specified function [block] with `this` value as its argument and returns `this` value. */
public inline fun <T> T.also(block: (T) -> Unit): T
/** Calls the specified function [block] with `this` value as its receiver and returns `this` value. */
public inline fun <T> T.apply(block: T.() -> Unit): T
/** Calls the specified function [block] with `this` value as its receiver and returns its result. */
public inline fun <T, R> T.run(block: T.() -> R): R
/** Calls the specified function [block] with the given [receiver] as its receiver and returns its result. */
public inline fun <T, R> with(receiver: T, block: T.() -> R): R
/** Returns `this` value if it satisfies the given [predicate] or `null`, if it doesn't. */
public inline fun <T> T.takeIf(predicate: (T) -> Boolean): T?
/** Returns `this` value if it _does not_ satisfy the given [predicate] or `null`, if it does. */
public inline fun <T> T.takeUnless(predicate: (T) -> Boolean): T?
/** Executes the given function [action] specified number of [times]. */
public inline fun repeat(times: Int, action: (Int) -> Unit)
/** Throws an [IllegalArgumentException] if the [value] is false. */
public inline fun require(value: Boolean): Unit
/** Throws an [IllegalStateException] if the [value] is false. */
public inline fun check(value: Boolean): Unit
/** Throws an [IllegalStateException] with the given [message]. */
public inline fun error(message: Any): Nothing
/** Always throws [NotImplementedError] stating that operation is not implemented. */
public inline fun TODO(): Nothing
/** Returns a hash code value for the object or zero if the object is `null`. */
public inline fun Any?.hashCode(): Int
/** Returns a string representation of the object. Can be called with a null receiver, in which case it returns the string "null". */
public fun Any?.toString(): String

// file: kotlin/io/Console.kt
package kotlin.io

/** Prints the given [message] and the line separator to the standard output stream. */
public fun println(message: Any?)
/** Prints the line separator to the standard output stream. */
public fun println()
/** Prints the given [message] to the standard output stream. */
public fun print(message: Any?)
/** Reads a line of input from the standard input stream. */
public fun readln(): String
/** Reads a line of input from the standard input stream, or returns `null` at its end. */
public fun readlnOrNull(): String?
/** Reads a line of input from the standard input stream. */
public fun readLine(): String?

// file: kotlin/collections/Collections.kt
package kotlin.collections

/** An iterator over a collection or another entity that can be represented as a sequence of elements. */
public interface Iterator<out T> {
    /** Returns the next element in the iteration. */
    public operator fun next(): T
    /** Returns `true` if the iteration has more elements. */
    public operator fun hasNext(): Boolean
}

/** Classes that inherit from this interface can be represented as a sequence of elements that can be iterated over. */
public interface Iterable<out T> {
    /** Returns an iterator over the elements of this object. */
    public operator fun iterator(): Iterator<T>
}

/** A generic collection of elements. */
public interface Collection<out E> : Iterable<E> {
    /** Returns the size of the collection. */
    public val size: Int
    /** Returns `true` if the collection is empty (contains no elements), `false` otherwise. */
    public fun isEmpty(): Boolean
    /** Checks if the specified element is contained in this collection. */
    public operator fun contains(element: @UnsafeVariance E): Boolean
}

/** A generic ordered collection of elements. */
public interface List<out E> : Collection<E> {
    /** Returns the element at the specified index in the list. */
    public operator fun get(index: Int): E
    /** Returns the index of the first occurrence of the specified element in the list, or -1. */
    public fun indexOf(element: @UnsafeVariance E): Int
    /** Returns a view of the portion of this list between the specified [fromIndex] (inclusive) and [toIndex] (exclusive). */
    public fun subList(fromIndex: Int, toIndex: Int): List<E>
}

/** A generic collection of elements that supports adding and removing elements. */
public interface MutableCollection<E> : Collection<E> {
    /** Adds the specified element to the collection. */
    public fun add(element: E): Boolean
    /** Removes a single instance of the specified element from this collection, if it is present. */
    public fun remove(element: E): Boolean
    /** Removes all elements from this collection. */
    public fun clear(): Unit
}

/** A generic ordered collection of elements that supports adding and removing elements. */
public interface MutableList<E> : List<E>, MutableCollection<E> {
    /** Replaces the element at the specified position in this list with the specified element. */
    public operator fun set(index: Int, element: E): E
    /** Removes an element at the specified [index] from the list. */
    public fun removeAt(index: Int): E
}

/** A generic unordered collection of elements that does not support duplicate elements. */
public interface Set<out E> : Collection<E>

/** A generic unordered collection of elements that does not support duplicate elements, and supports adding and removing elements. */
public interface MutableSet<E> : Set<E>, MutableCollection<E>

/** A collection that holds pairs of objects (keys and values) and supports efficiently retrieving the value corresponding to each key. */
public interface Map<K, out V> {
    /** Returns the number of key/value pairs in the map. */
    public val size: Int
    /** Returns `true` if the map is empty (contains no elements), `false` otherwise. */
    public fun isEmpty(): Boolean
    /** Returns `true` if the map contains the specified [key]. */
    public fun containsKey(key: K): Boolean
    /** Returns the value corresponding to the given [key], or `null` if such a key is not present in the map. */
    public operator fun get(key: K): V?
    /** Returns a read-only [Set] of all keys in this map. */
    public val keys: Set<K>
    /** Returns a read-only [Collection] of all values in this map. */
    public val values: Collection<V>
    /** Returns a read-only [Set] of all key/value pairs in this map. */
    public val entries: Set<Map.Entry<K, V>>

    /** Represents a key/value pair held by a [Map]. */
    public interface Entry<out K, out V> {
        /** Returns the key of this key/value pair. */
        public val key: K
        /** Returns the value of this key/value pair. */
        public val value: V
    }
}

/** A modifiable collection that holds pairs of objects (keys and values). */
public interface MutableMap<K, V> : Map<K, V> {
    /** Associates the specified [value] with the specified [key] in the map. */
    public fun put(key: K, value: V): V?
    /** Removes the specified key and its corresponding value from this map. */
    public fun remove(key: K): V?
    /** Removes all elements from this map. */
    public fun clear(): Unit
}

/** A [MutableList] backed by a resizable array. */
public open class ArrayList<E>() : MutableList<E>
/** A [MutableMap] backed by a hash table. */
public open class HashMap<K, V>() : MutableMap<K, V>
/** A [MutableSet] backed by a hash table. */
public open class HashSet<E>() : MutableSet<E>
/** A hash table based [MutableMap] that preserves the insertion order. */
public open class LinkedHashMap<K, V>() : HashMap<K, V>()

/** Returns a new read-only list of given elements. */
public fun <T> listOf(vararg elements: T): List<T>
/** Returns a new [MutableList] with the given elements. */
public fun <T> mutableListOf(vararg elements: T): MutableList<T>
/** Returns an empty read-only list. */
public fun <T> emptyList(): List<T>
/** Returns a new read-only set with the given elements. */
public fun <T> setOf(vararg elements: T): Set<T>
/** Returns a new [MutableSet] with the given elements. */
public fun <T> mutableSetOf(vararg elements: T): MutableSet<T>
/** Returns a new read-only map with the specified contents, given as a list of pairs. */
public fun <K, V> mapOf(vararg pairs: Pair<K, V>): Map<K, V>
/** Returns the value of the property named like the delegated property in this map. */
public inline operator fun <V, V1 : V> Map<in String, V>.getValue(thisRef: Any?, property: kotlin.reflect.KProperty<*>): V1
/** Returns a new [MutableMap] with the specified contents, given as a list of pairs. */
public fun <K, V> mutableMapOf(vararg pairs: Pair<K, V>): MutableMap<K, V>
/** Returns an array containing the specified elements. */
public fun <T> arrayOf(vararg elements: T): Array<T>

/** Performs the given [action] on each element. */
public inline fun <T> Iterable<T>.forEach(action: (T) -> Unit): Unit
/** Performs the given [action] on each element, providing sequential index with the element. */
public inline fun <T> Iterable<T>.forEachIndexed(action: (index: Int, T) -> Unit): Unit
/** Returns a list containing the results of applying the given [transform] function to each element in the original collection. */
public inline fun <T, R> Iterable<T>.map(transform: (T) -> R): List<R>
/** Returns a list containing only elements matching the given [predicate]. */
public inline fun <T> Iterable<T>.filter(predicate: (T) -> Boolean): List<T>
/** Returns the first element. */
public fun <T> List<T>.first(): T
/** Returns the first element, or `null` if the list is empty. */
public fun <T> List<T>.firstOrNull(): T?
/** Returns the last element. */
public fun <T> List<T>.last(): T
/** Creates a string from all the elements separated using [separator]. */
public fun <T> Iterable<T>.joinToString(separator: CharSequence = ", ", prefix: CharSequence = "", postfix: CharSequence = ""): String
/** Returns `true` if the collection is not empty. */
public inline fun <T> Collection<T>.isNotEmpty(): Boolean
/** Returns a list of all elements sorted according to natural sort order of the value returned by specified [selector] function. */
public inline fun <T, R : Comparable<R>> Iterable<T>.sortedBy(selector: (T) -> R?): List<T>
/** Groups elements of the original collection by the key returned by the given [keySelector] function. */
public inline fun <T, K> Iterable<T>.groupBy(keySelector: (T) -> K): Map<K, List<T>>
/** Returns a [List] containing all elements. */
public fun <T> Iterable<T>.toList(): List<T>
/** Returns a new [MutableList] filled with all elements of this collection. */
public fun <T> Iterable<T>.toMutableList(): MutableList<T>
/** Returns the sum of all elements in the collection. */
public fun Iterable<Int>.sum(): Int
/** Returns a lazy [Iterable] that wraps each element of the original collection into an [IndexedValue]. */
public fun <T> Iterable<T>.withIndex(): Iterable<IndexedValue<T>>
/** Data class representing a value from a collection or sequence, along with its index in that collection or sequence. */
public data class IndexedValue<out T>(public val index: Int, public val value: T)
/** Returns the key component of the map entry. */
public inline operator fun <K, V> Map.Entry<K, V>.component1(): K
/** Returns the value component of the map entry. */
public inline operator fun <K, V> Map.Entry<K, V>.component2(): V
/** Returns an [Iterator] over the entries in the [Map]. */
public inline operator fun <K, V> Map<out K, V>.iterator(): Iterator<Map.Entry<K, V>>
/** Performs the given [action] on each entry. */
public inline fun <K, V> Map<out K, V>.forEach(action: (Map.Entry<K, V>) -> Unit): Unit
/** Returns the value for the given key, or the result of [defaultValue] if there is no such key. */
public inline fun <K, V> Map<K, V>.getOrElse(key: K, defaultValue: () -> V): V
/** Returns the value for the given key. If the key is not found in the map, calls [defaultValue], puts its result into the map and returns it. */
public inline fun <K, V> MutableMap<K, V>.getOrPut(key: K, defaultValue: () -> V): V

// file: kotlin/text/Strings.kt
package kotlin.text

/** A mutable sequence of characters. */
public class StringBuilder : CharSequence, Appendable {
    public override val length: Int
    public override fun get(index: Int): Char
    public override fun subSequence(startIndex: Int, endIndex: Int): CharSequence
    /** Appends the string representation of the specified object [value] to this string builder and returns this instance. */
    public fun append(value: Any?): StringBuilder
    public override fun toString(): String
}

/** An object to which char sequences and values can be appended. */
public interface Appendable

/** Represents a compiled regular expression. */
public class Regex(pattern: String) {
    /** Indicates whether the regular expression matches the entire [input]. */
    public infix fun matches(input: CharSequence): Boolean
    /** Replaces all occurrences of this regular expression in the specified [input] string with specified [replacement] expression. */
    public fun replace(input: CharSequence, replacement: String): String
}

/** Builds a new string by populating a newly created [StringBuilder] using the provided [builderAction] and then converting it to [String]. */
public inline fun buildString(builderAction: StringBuilder.() -> Unit): String
/** Returns `true` if this char sequence is empty or consists solely of whitespace characters. */
public fun CharSequence.isBlank(): Boolean
/** Returns `true` if this char sequence is not empty and contains some characters except of whitespace characters. */
public inline fun CharSequence.isNotBlank(): Boolean
/** Returns `true` if this char sequence is empty (contains no characters). */
public inline fun CharSequence.isEmpty(): Boolean
/** Returns `true` if this char sequence is not empty. */
public inline fun CharSequence.isNotEmpty(): Boolean
public inline fun CharSequence?.isNullOrEmpty(): Boolean
/** Returns a string with leading and trailing whitespace removed. */
public inline fun String.trim(): String
/** Splits this char sequence to a list of strings around occurrences of the specified [delimiters]. */
public fun CharSequence.split(vararg delimiters: String, ignoreCase: Boolean = false, limit: Int = 0): List<String>
/** Returns a copy of this string converted to upper case using Unicode mapping rules of the invariant locale. */
public inline fun String.uppercase(): String
/** Returns a copy of this string converted to lower case using Unicode mapping rules of the invariant locale. */
public inline fun String.lowercase(): String
/** Parses the string as an [Int] number and returns the result. */
public fun String.toInt(): Int
/** Parses the string as an [Int] number and returns the result or `null` if the string is not a valid representation of a number. */
public fun String.toIntOrNull(): Int?
/** Parses the string as a [Double] number and returns the result. */
public fun String.toDouble(): Double
/** Returns `true` if this char sequence contains the specified [other] sequence of characters as a substring. */
public operator fun CharSequence.contains(other: CharSequence, ignoreCase: Boolean = false): Boolean
/** Returns `true` if this string starts with the specified prefix. */
public fun String.startsWith(prefix: String, ignoreCase: Boolean = false): Boolean
/** Returns `true` if this string ends with the specified suffix. */
public fun String.endsWith(suffix: String, ignoreCase: Boolean = false): Boolean
/** Returns a new string with all occurrences of [oldValue] replaced with [newValue]. */
public fun String.replace(oldValue: String, newValue: String, ignoreCase: Boolean = false): String
/** Splits this char sequence to a list of lines delimited by any of the following character sequences: CRLF, LF or CR. */
public fun CharSequence.lines(): List<String>
/** Returns a string containing the first [n] characters from this string, or the entire string if this string is shorter. */
public fun String.take(n: Int): String
/** Returns a string containing the characters of the original char sequence in reverse order. */
public inline fun String.reversed(): String
/** Returns a copy of this string having its first letter titlecased, or the original string if it's empty or already starts with a title case letter. */
public fun String.replaceFirstChar(transform: (Char) -> CharSequence): String
/** Returns a string produced by padding the start of this string to [length]. */
public fun String.padStart(length: Int, padChar: Char = ' '): String

// file: kotlin/ranges/Ranges.kt
package kotlin.ranges

/** A progression of values of type `Int`. */
public open class IntProgression : Iterable<Int> {
    /** The first element in the progression. */
    public val first: Int
    /** The last element in the progression. */
    public val last: Int
    /** The step of the progression. */
    public val step: Int
    override fun iterator(): IntIterator
}

/** A range of values of type `Int`. */
public class IntRange(start: Int, endInclusive: Int) : IntProgression() {
    public val start: Int
    public val endInclusive: Int
    /** Checks whether the specified [value] belongs to the range. */
    public operator fun contains(value: Int): Boolean
}

/** A range of values of type `Long`. */
public class LongRange(start: Long, endInclusive: Long) : Iterable<Long>
/** A range of values of type `Char`. */
public class CharRange(start: Char, endInclusive: Char) : Iterable<Char>
/** An iterator over a sequence of values of type `Int`. */
public abstract class IntIterator : Iterator<Int> {
    override final fun next(): Int
    public abstract fun nextInt(): Int
}

/** Returns a range from this value up to but excluding the specified [to] value. */
public infix fun Int.until(to: Int): IntRange
/** Returns a progression from this value down to the specified [to] value with the step -1. */
public infix fun Int.downTo(to: Int): IntProgression
/** Returns a progression that goes over the same range with the given step. */
public infix fun IntProgression.step(step: Int): IntProgression
/** Ensures that this value lies in the specified range [minimumValue]..[maximumValue]. */
public fun Int.coerceIn(minimumValue: Int, maximumValue: Int): Int
/** Ensures that this value is not less than the specified [minimumValue]. */
public fun <T : Comparable<T>> T.coerceAtLeast(minimumValue: T): T

// file: kotlin/properties/Delegates.kt
package kotlin.properties

/** Base interface that can be used for implementing property delegates of read-only properties. */
public fun interface ReadOnlyProperty<in T, out V> {
    /** Returns the value of the property for the given object. */
    public operator fun getValue(thisRef: T, property: kotlin.reflect.KProperty<*>): V
}

/** Base interface that can be used for implementing property delegates of read-write properties. */
public interface ReadWriteProperty<in T, V> {
    /** Returns the value of the property for the given object. */
    public operator fun getValue(thisRef: T, property: kotlin.reflect.KProperty<*>): V
    /** Sets the value of the property for the given object. */
    public operator fun setValue(thisRef: T, property: kotlin.reflect.KProperty<*>, value: V)
}

/** Standard property delegates. */
public object Delegates {
    /** Returns a property delegate for a read/write property with a non-`null` value that is initialized not during object construction time but at a later time. */
    public fun <T : Any> notNull(): ReadWriteProperty<Any?, T>
    /** Returns a property delegate for a read/write property that calls a specified callback function when changed. */
    public inline fun <T> observable(initialValue: T, crossinline onChange: (property: kotlin.reflect.KProperty<*>, oldValue: T, newValue: T) -> Unit): ReadWriteProperty<Any?, T>
    /** Returns a property delegate for a read/write property that calls a specified callback function when changed, allowing the callback to veto the modification. */
    public inline fun <T> vetoable(initialValue: T, crossinline onChange: (property: kotlin.reflect.KProperty<*>, oldValue: T, newValue: T) -> Boolean): ReadWriteProperty<Any?, T>
}

// file: kotlin/reflect/KProperty.kt
package kotlin.reflect

/** Represents a property, such as a named `val` or `var` declaration. */
public interface KProperty<out V> {
    /** The name of this property. */
    public val name: String
}
