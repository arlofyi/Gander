package fyi.arlo.gander.core.data.pairing

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File
import java.security.MessageDigest

/**
 * The SAS against `protocol/test-vectors/sas.json`, the file the Rust host's `sas_vectors.rs`
 * runs too. Gradle passes its repository path as `gander.sasVectors` and re-runs this test when the
 * file changes, so the host and the client cannot drift apart unnoticed.
 */
class SasTest {
    private val vectors: List<JsonObject> by lazy {
        val path = System.getProperty("gander.sasVectors") ?: error("system property gander.sasVectors is not set (run through Gradle)")
        val root = Json.parseToJsonElement(File(path).readText(Charsets.UTF_8)).jsonObject
        assertEquals("vector file version", 1, root.getValue("version").jsonPrimitive.int)
        root.getValue("vectors").jsonArray.map { it.jsonObject }
    }

    private fun JsonObject.text(key: String) = getValue(key).jsonPrimitive.content

    private fun hex(text: String): ByteArray = ByteArray(text.length / 2) { text.substring(it * 2, it * 2 + 2).toInt(16).toByte() }

    private fun hex(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it.toInt() and 0xFF) }

    private fun vector(id: String) = vectors.single { it.text("id") == id }

    private fun derive(v: JsonObject): SasCode =
        Sas.derive(hex(v.text("host_spki_der_hex")), hex(v.text("device_nonce_hex")), hex(v.text("host_nonce_hex")))

    @Test
    fun theFileHasTheVectorsTheSpecNames() {
        val ids = vectors.map { it.text("id") }
        assertEquals("ids are unique", ids.size, ids.toSet().size)
        listOf("sas-basic", "sas-mitm-other-spki", "sas-swapped-nonces", "sas-zero-nonces", "sas-leading-zero", "sas-reduced-modulo")
            .forEach { assertTrue("vector $it is in sas.json", it in ids) }
    }

    @Test
    fun everyVectorGivesTheExpectedDigestBitsAndCode() {
        for (v in vectors) {
            val id = v.text("id")
            val digest = Sas.digest(hex(v.text("host_spki_der_hex")), hex(v.text("device_nonce_hex")), hex(v.text("host_nonce_hex")))
            assertEquals("$id sha256", v.text("sha256_hex"), hex(digest))
            assertEquals("$id first 20 bits", v.getValue("first_20_bits").jsonPrimitive.int, Sas.first20Bits(digest))
            val sas = derive(v)
            assertEquals("$id digits", v.text("sas_digits"), sas.digits)
            assertEquals("$id display", v.text("sas"), sas.display)
            assertTrue("$id matches its own display form", sas.matches(v.text("sas")))
            assertTrue("$id matches its digits", sas.matches(v.text("sas_digits")))
        }
    }

    @Test
    fun theWireFormsOfTheNoncesDecodeToTheRawBytes() {
        for (v in vectors) {
            assertArrayEquals(hex(v.text("device_nonce_hex")), Base64Url.decodeFixed(v.text("device_nonce_b64url"), Sas.NONCE_BYTES))
            assertArrayEquals(hex(v.text("host_nonce_hex")), Base64Url.decodeFixed(v.text("host_nonce_b64url"), Sas.NONCE_BYTES))
        }
    }

    @Test
    fun theHostIdIsTheHashOfTheSpkiTheCodeIsDerivedFrom() {
        for (v in vectors) {
            val spki = hex(v.text("host_spki_der_hex"))
            val id = Base64Url.encode(MessageDigest.getInstance("SHA-256").digest(spki))
            assertEquals("${v.text("id")} host_id", v.text("host_id"), id)
        }
    }

    @Test
    fun aManInTheMiddleWithAnotherCertificateGetsAnotherCode() {
        val honest = derive(vector("sas-basic"))
        val mitm = derive(vector("sas-mitm-other-spki"))
        assertEquals("same nonces", vector("sas-basic").text("device_nonce_hex"), vector("sas-mitm-other-spki").text("device_nonce_hex"))
        assertNotEquals(honest, mitm)
    }

    @Test
    fun nonceOrderMatters() {
        assertNotEquals(derive(vector("sas-basic")), derive(vector("sas-swapped-nonces")))
    }

    @Test
    fun theCodeIsAlwaysSixDigitsGroupedThreeAndThree() {
        assertEquals("079 515", SasCode(79_515).display)
        assertEquals("079515", SasCode(79_515).digits)
        assertEquals("000 000", SasCode(0).display)
        assertEquals("999 999", SasCode(999_999).display)
    }

    @Test
    fun matchingIgnoresTheGroupingSpaceAndNothingElse() {
        val sas = SasCode(482_913)
        assertTrue(sas.matches("482 913"))
        assertTrue(sas.matches("482913"))
        assertFalse(sas.matches("482  913"))
        assertFalse(sas.matches("482-913"))
        assertFalse(sas.matches("482 914"))
        assertFalse(sas.matches(""))
        assertFalse(sas.matches("4829130"))
    }

    @Test
    fun nonceLengthsAreChecked() {
        try {
            Sas.derive(ByteArray(91), ByteArray(15), ByteArray(16))
            error("a 15 byte nonce must be refused")
        } catch (_: IllegalArgumentException) {
            // expected
        }
    }
}
