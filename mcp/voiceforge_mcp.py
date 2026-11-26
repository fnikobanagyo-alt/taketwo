# /// script
# requires-python = ">=3.10"
# dependencies = ["mcp>=1.2,<2"]
# ///
"""Former name of taketwo_mcp.py, kept so existing "voiceforge" MCP entries keep working."""
import runpy
from pathlib import Path

runpy.run_path(str(Path(__file__).with_name("taketwo_mcp.py")), run_name="__main__")
