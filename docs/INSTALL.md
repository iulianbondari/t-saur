# Installing T-saur

T-saur is a single command-line binary, `tsaur`. It needs no account, service or configuration
file, and it makes no network connection unless you run `tsaur volumes serve` or `fetch`.

## From a release package

1. Download `tsaur-<version>-<platform>-lite.zip` and `SHA256SUMS` from the release page. The
   **lite** package is the recommended one (every archive it writes opens in every build; like
   every build it contains one LGPL-3.0 component, see `THIRD-PARTY-NOTICES.md`). The full build, which recompresses JPEGs, is built from source; see
   `DISTRIBUTION-POLICY.md`.
2. Check the download before unpacking:
   * Linux/macOS: `sha256sum -c SHA256SUMS --ignore-missing`
   * Windows (PowerShell): `Get-FileHash tsaur-*.zip -Algorithm SHA256`, compare with `SHA256SUMS`
3. Unpack and put the binary on your `PATH`, or call it by its path.
4. `tsaur --version` prints the version; `tsaur --help` the commands; `GUIDE.md` walks through
   the first archive, volumes and transfers.

Packages exist for the platforms listed on the release page. Only the platforms on which the test
suite was actually run are declared verified in `V1-CONTRACT.md` §5.

## From source

Requirements: Rust 1.87 or newer (`rustup` installs it), a C toolchain for the compression
libraries (MSVC Build Tools on Windows, `build-essential` on Debian/Ubuntu, Xcode command-line
tools on macOS). No CMake or NASM is needed.

```bash
git clone https://github.com/iulianbondari/t-saur.git
cd t-saur/tsaur
cargo build --release                        # full build, Lepton JPEG recompression, LGPL notice applies
cargo build --release --no-default-features  # lite build, no Lepton (the LGPL notice applies to both builds)
./target/release/tsaur --version
```

Run the checks the project runs before a release: `cargo test` (either build), then from the
repository root `python tools/check_guide.py` to execute the guide against your binary.

## Uninstalling

Delete the binary. T-saur writes nothing outside the directories you name on the command line:
no registry entries, no configuration, no cache.
