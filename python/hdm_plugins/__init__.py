"""Hydra's extraction plugins.

Hydra's engine is Rust; this package is the part that reads pages. Parsing
messy real-world HTML, recognising media, and delegating to yt-dlp are jobs
Python is genuinely better at, and keeping them in a separate process means a
malformed page or a crashing extractor cannot take the daemon down with it.

The daemon fetches; this package only parses. That keeps cookies, proxies and
authentication in one place rather than reimplementing them here.
"""

__version__ = "0.1.0"
