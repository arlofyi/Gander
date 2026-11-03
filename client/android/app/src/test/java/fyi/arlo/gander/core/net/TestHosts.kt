package fyi.arlo.gander.core.net

import mockwebserver3.MockWebServer
import okhttp3.tls.HandshakeCertificates
import okhttp3.tls.HeldCertificate
import java.net.InetAddress

/**
 * Test fixtures. Every key pair is generated per run (ECDSA P-256, like the host's identity):
 * nothing secret is committed. Servers bind 127.0.0.1 only, never a wildcard address.
 */
object TestHosts {
    val LOOPBACK: InetAddress = InetAddress.getByName("127.0.0.1")

    /** A self-signed host certificate, as the Gander host generates at first run. */
    fun selfSigned(name: String = "gander-test-host"): HeldCertificate =
        HeldCertificate.Builder()
            .ecdsa256()
            .commonName(name)
            .addSubjectAlternativeName("127.0.0.1")
            .addSubjectAlternativeName("localhost")
            .build()

    /** A CA and a leaf it signed: what a "system-trusted" (publicly or user-trusted) server presents. */
    class CaSigned(val ca: HeldCertificate, val leaf: HeldCertificate)

    fun caSigned(): CaSigned {
        val ca = HeldCertificate.Builder().ecdsa256().commonName("Test Root CA").certificateAuthority(0).build()
        val leaf = HeldCertificate.Builder()
            .ecdsa256()
            .commonName("trusted.example")
            .addSubjectAlternativeName("127.0.0.1")
            .addSubjectAlternativeName("localhost")
            .signedBy(ca)
            .build()
        return CaSigned(ca, leaf)
    }

    /** A started HTTPS MockWebServer on 127.0.0.1 presenting [held] (plus [chain] intermediates). */
    fun httpsServer(held: HeldCertificate, vararg chain: java.security.cert.X509Certificate): MockWebServer {
        val serverCerts = HandshakeCertificates.Builder().heldCertificate(held, *chain).build()
        return MockWebServer().apply {
            useHttps(serverCerts.sslSocketFactory())
            start(LOOPBACK, 0)
        }
    }

    fun pinOf(held: HeldCertificate): SpkiPin = SpkiPin.of(held.certificate)

    fun clients(
        server: MockWebServer,
        pin: SpkiPin,
        token: BearerToken? = BearerToken("test-token-not-a-secret"),
        addresses: List<String> = listOf("127.0.0.1"),
    ): GanderHttpClients = GanderHttpClients(
        pin = pin,
        endpoint = HostEndpoint(addresses, server.port),
        appVersion = "0.0.0-test",
        token = { token },
    )
}
