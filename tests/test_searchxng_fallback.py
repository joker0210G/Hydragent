from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from adapters.utils.searchxng import extract_results_from_duckduckgo_html


def test_extract_results_from_duckduckgo_html() -> None:
    html = """
    <html><body>
    <a rel="nofollow" href="https://www.rust-lang.org/" class="result__a">Rust Programming Language</a>
    <a class="result__snippet">Systems programming language with memory safety.</a>
    <a rel="nofollow" href="https://doc.rust-lang.org/" class="result__a">Rust Documentation</a>
    <a class="result__snippet">Official reference and guides.</a>
    </body></html>
    """

    results = extract_results_from_duckduckgo_html(html, max_results=2)

    assert len(results) == 2
    assert results[0]["title"] == "Rust Programming Language"
    assert results[0]["url"] == "https://www.rust-lang.org/"
    assert "memory safety" in results[0]["content"].lower()


if __name__ == "__main__":
    test_extract_results_from_duckduckgo_html()
    print("searchxng fallback ok")
