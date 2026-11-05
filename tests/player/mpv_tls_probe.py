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
        self.handshake_errors = []
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.load_cert_chain(str(cert), str(key))
        self.socket = ctx.wrap_socket(self.socket, server_side=True)

    def get_request(self):
        try:
            return super().get_request()
        except ssl.SSLError as exc:
            self.handshake_errors.append(str(exc))
            raise


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
                    "--cache=no", "--really-quiet", "--curl-enabled=yes"]

            def play(extra):
                return subprocess.run([*base, *extra, "--", url],
                                      stdout=subprocess.DEVNULL,
                                      stderr=subprocess.DEVNULL,
                                      timeout=20, check=False)

            rejected = play([])
            if rejected.returncode == 0:
                raise AssertionError("mpv accepted an untrusted self-signed HTTPS source by default")
            if server.paths:
                raise AssertionError("default-TLS request reached untrusted HTTPS media")
            if not server.handshake_errors:
                raise AssertionError("TLS verification check had no observed handshake failure")
            permitted = play(["--tls-verify=no"])
            if permitted.returncode != 0 or "/clip.wav" not in server.paths:
                raise AssertionError("test-only TLS bypass control could not play the same HTTPS media")
            print("PASS default TLS rejects untrusted certificate; explicit test-only bypass plays same synthetic URL")
        finally:
            server.shutdown()
            server.server_close()
            worker.join(timeout=5)


if __name__ == "__main__":
    main()
