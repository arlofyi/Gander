package fyi.arlo.gander.core.net

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import org.yaml.snakeyaml.LoaderOptions
import org.yaml.snakeyaml.Yaml
import org.yaml.snakeyaml.constructor.SafeConstructor
import java.io.File
import java.math.BigInteger

/**
 * Reads `protocol/openapi.yaml` from the repository (path passed by Gradle as the system
 * property `gander.openapi`, see app/build.gradle.kts), so the DTO tests always see the current
 * protocol document and a drift fails the build.
 */
object ProtocolExamples {
    private val document: Map<String, Any?> by lazy {
        val path = System.getProperty("gander.openapi")
            ?: error("system property gander.openapi is not set (run through Gradle)")
        val file = File(path)
        check(file.isFile) { "protocol document not found at $path" }
        @Suppress("UNCHECKED_CAST")
        Yaml(SafeConstructor(LoaderOptions())).load<Any>(file.readText()) as Map<String, Any?>
    }

    @Suppress("UNCHECKED_CAST")
    private fun map(value: Any?): Map<String, Any?> = value as Map<String, Any?>

    private val components get() = map(document["components"])

    /** components.examples: name -> (x-gander-schema, value as JSON). */
    val examples: Map<String, Pair<String, JsonElement>> by lazy {
        map(components["examples"]).mapValues { (name, raw) ->
            val example = map(raw)
            val schema = example["x-gander-schema"] as? String ?: error("example $name has no x-gander-schema")
            schema to toJson(example["value"])
        }
    }

    fun exampleJson(name: String): String =
        examples[name]?.second?.toString() ?: error("no example named $name")

    /** The description of [axis] (`dx` or `dy`) of the `scroll` input message in components.schemas.InputMessage. */
    @Suppress("UNCHECKED_CAST")
    fun scrollAxisDescription(axis: String): String {
        val variants = map(map(components["schemas"])["InputMessage"])["oneOf"] as List<Any?>
        val scroll = variants.map(::map).single { map(map(it["properties"])["t"])["const"] == "scroll" }
        return map(map(scroll["properties"])[axis])["description"] as String
    }

    /** The `enum` list of components.schemas.<name>. */
    fun schemaEnum(name: String): List<String> {
        val schema = map(map(components["schemas"])[name])
        @Suppress("UNCHECKED_CAST")
        return (schema["enum"] as List<Any?>).map { it.toString() }
    }

    private fun toJson(value: Any?): JsonElement = when (value) {
        null -> JsonNull
        is Map<*, *> -> JsonObject(value.entries.associate { (k, v) -> k.toString() to toJson(v) })
        is List<*> -> JsonArray(value.map(::toJson))
        is String -> JsonPrimitive(value)
        is Boolean -> JsonPrimitive(value)
        is Int -> JsonPrimitive(value.toLong())
        is Long -> JsonPrimitive(value)
        is BigInteger -> JsonPrimitive(value.toLong())
        is Double -> JsonPrimitive(value)
        else -> error("unexpected YAML value type ${value.javaClass.name}; quote timestamps in examples")
    }
}
