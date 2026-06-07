from rich.console import Console
from rich.markdown import Markdown

console = Console()

def render_markdown(content: str):
    """Renders markdown content to the ANSI-compatible terminal."""
    console.print(Markdown(content))
