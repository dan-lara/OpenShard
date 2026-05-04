#!/bin/bash
# Generates a self-signed CA and a server certificate signed by that CA.
# The agent uses ca.crt to verify the server; the server loads server.crt + server.key.
set -e

cd "$(dirname "$0")"

echo "Generating CA key and self-signed certificate..."
openssl req -x509 -newkey rsa:4096 -nodes \
    -keyout ca.key -out ca.crt \
    -days 3650 \
    -config cert.conf

echo "Generating server key and CSR..."
openssl req -newkey rsa:4096 -nodes \
    -keyout server.key -out server.csr \
    -config cert.conf

echo "Signing server certificate with CA..."
openssl x509 -req \
    -in server.csr \
    -CA ca.crt -CAkey ca.key -CAcreateserial \
    -out server.crt \
    -days 3650 \
    -extfile cert.ext

rm server.csr ca.srl

echo "Done. Files generated: ca.crt, ca.key, server.crt, server.key"
