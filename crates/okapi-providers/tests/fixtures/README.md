# Local WSS test certificates

These files were generated solely for the loopback TLS tests in `http_tls_tests.rs`.
They are public test material, never production credentials. The server certificate
is valid only for DNS name `localhost`; its private test key is deliberately included.
The test CA signing key was discarded. Certificates expire in September 2046.

Tests explicitly trust this CA on their own clients. Production clients keep normal
certificate and hostname verification; the CA is never installed in a system store.
