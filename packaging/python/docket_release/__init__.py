"""Entry point for the universal GitHub release selector wheel."""


def main():
    try:
        from docket_cli import main as native_main
    except ImportError:
        raise SystemExit(
            "Docket provides prebuilt binaries for macOS 13+ and Linux, "
            "on x86_64 and ARM64. This platform is not supported."
        )
    native_main()
