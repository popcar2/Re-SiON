#!/usr/bin/env python3
# Serves this folder over HTTP with extensionless page URLs: /songs serves songs.html, and so on.
# Usage: python3 serve.py [port] (default 8000), then open http://localhost:8000/
import os, sys
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer

class Handler(SimpleHTTPRequestHandler):
    def translate_path(self, path):
        # The page wins over a folder of the same name (/songs is songs.html, not a songs/ folder).
        local = super().translate_path(path)
        if not local.endswith(os.sep) and os.path.isfile(local + ".html"):
            return local + ".html"
        return local

port = int(sys.argv[1]) if len(sys.argv) > 1 else 8000
root = os.path.dirname(os.path.abspath(__file__))
print(f"Serving {root} at http://localhost:{port}/")
ThreadingHTTPServer(("", port), partial(Handler, directory=root)).serve_forever()
