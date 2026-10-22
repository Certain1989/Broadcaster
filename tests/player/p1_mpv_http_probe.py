#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: GPL-3.0-or-later
"""Opt-in local mpv 0.41 probe; never uses live media or authentic credentials.

Run: python3 tests/player/p1_mpv_http_probe.py [--mpv /path/to/mpv]
A fake mpv executable argv check runs in Rust CI separately. This probe
tests actual mpv HTTP behavior and MUST be executed before P1 sign-off.
"""
import argparse
import io
import re
import shutil
import subprocess
import tempfile
import threading
import wave
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit


AGENT = "Broadcaster-P1-Probe/1"


def synthetic_wav():
    payload = io.BytesIO()
    with wave.open(payload, "wb") as sound:
        sound.setnchannels(1)
        sound.setsampwidth(2)
        sound.setframerate(8000)
        sound.writeframes(b"\x00\x00" * 8000)
    return payload.getvalue()


class FixtureServer(ThreadingHTTPServer):
    def __init__(self, role, observations, wav_data, segment=None):
        self.role = role
        self.observations = observations
        self.wav_data = wav_data
        self.segment = segment
        self.peer = None
        super().__init__(("127.0.0.1", 0), FixtureHandler)

    @property
    def origin(self):
        return f"http://127.0.0.1:{self.server_port}"


class FixtureHandler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass  # Do not emit untrusted requests or header contents.

    def do_GET(self):
        fixture = self.server
        path = urlsplit(self.path).path
        fixture.observations.append(
            (fixture.role, path, self.headers.get("User-Agent"),
             self.headers.get("Referer"))
        )
        if path == "/redirect":
            self.send_response(302)
            self.send_header("Location", fixture.peer.origin + "/clip.wav")
            self.end_headers()
            return
        if path == "/clip.wav":
            mime, body = "audio/wav", fixture.wav_data
        elif path == "/master.m3u8" and fixture.segment is not None:
            body = (
                "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:1\n"
                "#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:1.0,\n"
                + fixture.peer.origin + "/segment.ts\n#EXT-X-ENDLIST\n"
            ).encode()
            mime = "application/vnd.apple.mpegurl"
        elif path == "/segment.ts" and fixture.segment is not None:
            mime, body = "video/mp2t", fixture.segment
        else:
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type", mime)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        try:
            self.wfile.write(body)
        except BrokenPipeError:
            pass  # mpv may close an HTTP stream after obtaining enough data.


def make_segment(folder):
    if not shutil.which("ffmpeg"):
        return None
    target = folder / "fixture.ts"
    result = subprocess.run(
        ["ffmpeg", "-nostdin", "-hide_banner", "-loglevel", "error",
         "-f", "lavfi", "-i",
         "sine=frequency=440:sample_rate=44100:duration=0.6",
         "-c:a", "mp2", "-b:a", "64k", "-f", "mpegts", "-y", str(target)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        timeout=15, check=False,
    )
    return target.read_bytes() if result.returncode == 0 else None


def assert_request(observations, role, path, agent, referer):
    matching = [r for r in observations if r[0] == role and r[1] == path]
    if not matching:
        raise AssertionError(f"missing request {role}:{path}")
    for _, _, ua, ref in matching:
        if agent is not None and ua != agent:
            raise AssertionError(f"wrong User-Agent at {role}:{path}")
        if referer is not None and ref != referer:
            raise AssertionError(f"wrong Referer at {role}:{path}")
    return matching


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mpv", default="mpv", help="mpv 0.41 executable")
    args = parser.parse_args()
    version = subprocess.run(
        [args.mpv, "--version"], capture_output=True, text=True,
        timeout=10, check=True,
    ).stdout
    if not re.search(r"\bmpv 0\.41(?:\.|\b)", version):
        raise SystemExit("NOT RUN: this fixture requires mpv 0.41")
    with tempfile.TemporaryDirectory(prefix="broadcaster-p1-http-") as tmp:
        observations = []
        wav_data = synthetic_wav()
        segment = make_segment(Path(tmp))
        first = FixtureServer("origin", observations, wav_data, segment)
        second = FixtureServer("other-origin", observations, wav_data, segment)
        first.peer, second.peer = second, first
        threads = [
            threading.Thread(target=server.serve_forever, daemon=True)
            for server in (first, second)
        ]
        for thread in threads:
            thread.start()
        try:
            referer = first.origin + "/page"

            def play(path, imported):
                flags = (
                    [f"--user-agent={AGENT}", f"--referrer={referer}"]
                    if imported else []
                )
                command = [
                    args.mpv, "--no-config", "--no-video", "--ao=null",
                    "--cache=no", "--really-quiet", *flags, "--",
                    first.origin + path,
                ]
                before = len(observations)
                result = subprocess.run(
                    command, stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL, timeout=20, check=False,
                )
                if result.returncode != 0:
                    raise AssertionError(
                        f"mpv playback failed (status {result.returncode}; output suppressed)"
                    )
                return observations[before:]

            redirect = play("/redirect", imported=True)
            assert_request(redirect, "origin", "/redirect", AGENT, referer)
            other = assert_request(redirect, "other-origin", "/clip.wav", None, None)
            print(
                "PASS redirect; cross-origin User-Agent forwarded:",
                any(item[2] == AGENT for item in other),
                "Referer forwarded:",
                any(item[3] == referer for item in other),
            )

            no_hints = play("/clip.wav", imported=False)
            base = assert_request(no_hints, "origin", "/clip.wav", None, None)
            if any(row[2] == AGENT or row[3] == referer for row in base):
                raise AssertionError("imported hint leaked into new mpv process")
            print("PASS fresh process without imported headers")

            if segment is None:
                print("NOT RUN HLS segment: ffmpeg unavailable or fixture creation failed")
            else:
                hls = play("/master.m3u8", imported=True)
                assert_request(hls, "origin", "/master.m3u8", AGENT, referer)
                segments = assert_request(hls, "other-origin", "/segment.ts", None, None)
                print(
                    "PASS HLS media fetch; cross-origin segment User-Agent forwarded:",
                    any(item[2] == AGENT for item in segments),
                    "Referer forwarded:",
                    any(item[3] == referer for item in segments),
                )

            again = play("/clip.wav", imported=True)
            assert_request(again, "origin", "/clip.wav", AGENT, referer)
            print("PASS imported headers restored in subsequent fresh process")
        finally:
            for server in (first, second):
                server.shutdown()
                server.server_close()
            for thread in threads:
                thread.join()


if __name__ == "__main__":
    main()
