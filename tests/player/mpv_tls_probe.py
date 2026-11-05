#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: GPL-3.0-or-later
"""Verify packaged mpv rejects untrusted localhost TLS by default.

Uses only a synthetic WAV and a temporary, self-signed local HTTPS server.
An explicit insecure playback of the SAME URL is a test-only positive control;
no application options or persistent trust stores are changed.
"""
import argparse
import shutil
import sys
import ssl
import subprocess
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit

from mpv_http_probe import expected_mpv_version, synthetic_wav


class TLSMediaHandler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        self.server.paths.append(urlsplit(self.path).path)
        if urlsplit(self.path).path != "/clip.wav":
            self.send_error(404)
            return
        data = self.server.audio
        self.send_response(200)
        self.send_header("Content-Type", "audio/wav")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        try:
            self.wfile.write(data)
        except BrokenPipeError:
            pass


class TLSMediaServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, cert, key):
        super().__init__(("127.0.0.1", 0), TLSMediaHandler)
        self.paths = []
        self.audio = synthetic_wav()
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.load_cert_chain(str(cert), str(key))
        self.socket = ctx.wrap_socket(self.socket, server_side=True)

    def handle_error(self, request, client_address):
        # A certificate-rejecting TLS client can reset an *established*
        # connection after the server-side handshake. This is not reliable
        # proof of why the client rejected it; mpv's own diagnostic is the oracle.
        if isinstance(sys.exc_info()[1], ConnectionResetError):
            return
        super().handle_error(request, client_address)


def has_certificate_verification_diagnostic(result):
    """mpv logs to stdout by default; do not discard either output channel.

    mpv 0.42 common/msg.c:198–203 selects stdout unless force_stderr.
    stream/stream_curl.c:805–812 emits this specific certificate error.
    """
    return any("TLS certificate verification failed." in (output or "")
               for output in (result.stdout, result.stderr))


def assert_tls_evidence(rejected, negative_paths, permitted, positive_paths):
    """Require mpv's specific verification diagnostic AND the live control GET."""
    if rejected.returncode == 0:
        raise AssertionError("mpv accepted untrusted self-signed HTTPS by default")
    if negative_paths:
        raise AssertionError("default TLS request reached the untrusted HTTPS media")
    # mpv 0.42 stream_curl.c emits this only for libcurl's certificate
    # verification failure, rather than generic DNS/connection/player errors.
    if not has_certificate_verification_diagnostic(rejected):
        raise AssertionError("mpv failed without its certificate-verification diagnostic")
    if permitted.returncode != 0 or "/clip.wav" not in positive_paths:
        raise AssertionError("explicit test-only TLS bypass failed to GET the same HTTPS media")


def make_certificate(folder):
    if not shutil.which("openssl"):
        raise AssertionError("TLS negative test requires openssl")
    cert, key = folder / "cert.pem", folder / "key.pem"
    cmd = ["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
           "-days", "1", "-subj", "/CN=localhost",
           "-addext", "subjectAltName=IP:127.0.0.1",
           "-keyout", str(key), "-out", str(cert)]
    subprocess.run(cmd, capture_output=True, timeout=20, check=True)
    return cert, key


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mpv", default="mpv")
    parser.add_argument("--expect-version", default="0.42", choices=("0.42",))
    args = parser.parse_args()
    version = subprocess.run([args.mpv, "--version"], capture_output=True,
                             text=True, timeout=10, check=True).stdout
    if not expected_mpv_version(version, args.expect_version):
        raise AssertionError("wrong packaged mpv version for TLS probe")

    with tempfile.TemporaryDirectory(prefix="broadcaster-mpv-tls-") as tmp:
        cert, key = make_certificate(Path(tmp))
        server = TLSMediaServer(cert, key)
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            url = f"https://127.0.0.1:{server.server_port}/clip.wav"
            base = [args.mpv, "--no-config", "--no-video", "--ao=null",
                    "--cache=no", "--msg-level=all=error", "--curl-enabled=yes"]

            def play(extra):
                # Only synthetic localhost data is used; never display raw
                # stdout/stderr, URLs or credentials in CI. Inspect a fixed
                # upstream diagnostic and retain output in memory only.
                return subprocess.run([*base, *extra, "--", url],
                                      stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                      text=True, errors="replace", timeout=20,
                                      check=False)

            rejected = play([])
            negative_paths = tuple(server.paths)
            # Always exercise the same-origin positive control before checking
            # the negative oracle; unrelated player failures cannot pass.
            permitted = play(["--tls-verify=no"])
            positive_paths = tuple(server.paths[len(negative_paths):])
            # Only emit fixed booleans and return codes; no raw mpv output,
            # actual URLs or credential-bearing data leave this test process.
            print("TLS oracle:",
                  "negative_nonzero=", rejected.returncode != 0,
                  "negative_no_get=", not bool(negative_paths),
                  "certificate_diagnostic=", has_certificate_verification_diagnostic(rejected),
                  "control_exit_ok=", permitted.returncode == 0,
                  "control_media_get=", "/clip.wav" in positive_paths,
                  flush=True)
            assert_tls_evidence(rejected, negative_paths, permitted, positive_paths)
            print("PASS mpv certificate-rejection diagnostic and explicit test-only same-URL TLS bypass GET")
        finally:
            server.shutdown()
            server.server_close()
            worker.join(timeout=5)


if __name__ == "__main__":
    main()
