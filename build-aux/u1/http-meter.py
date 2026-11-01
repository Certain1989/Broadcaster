#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Broadcaster contributors
# SPDX-License-Identifier: CC0-1.0
"""Loopback-only, nonproduction HTTP server with response-body byte accounting."""

import argparse
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer


class MeteredHandler(SimpleHTTPRequestHandler):
    def copyfile(self, source, outputfile):
        sent = 0
        while True:
            chunk = source.read(64 * 1024)
            if not chunk:
                break
            outputfile.write(chunk)
            sent += len(chunk)
        self.log_message("U1_BODY_BYTES %d", sent)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("port", type=int)
    parser.add_argument("directory")
    args = parser.parse_args()
    handler = partial(MeteredHandler, directory=args.directory)
    server = ThreadingHTTPServer(("127.0.0.1", args.port), handler)
    server.serve_forever()


if __name__ == "__main__":
    main()
