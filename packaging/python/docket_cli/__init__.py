"""Launch the native Docket binary shipped in this platform wheel."""

import os
from pathlib import Path
import sys


def main():
    binary = Path(__file__).resolve().parent / "bin" / "docket"
    if not binary.is_file():
        sys.exit("Docket's native binary is missing. Reinstall the platform wheel.")
    try:
        os.execv(str(binary), [str(binary), *sys.argv[1:]])
    except OSError as error:
        sys.exit("Could not start Docket: {}".format(error))
