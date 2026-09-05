"""Synthetic preparation timings, NOT media throughput measurements.

Run: python bench_synthetic.py [--source-tree /path/to/baseline] [--runs 5]
Uses temporary files, no media tools or third-party dependencies.
"""

import argparse
import inspect
import statistics
import sys
import tempfile
import time
from pathlib import Path
from typing import Any


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-tree", type=Path, default=Path(__file__).resolve().parent)
    parser.add_argument("--releases", type=int, default=50)
    parser.add_argument("--fonts", type=int, default=100)
    parser.add_argument("--non-fonts", type=int, default=50)
    parser.add_argument("--subs", type=int, default=5000)
    parser.add_argument("--runs", type=int, default=5)
    args = parser.parse_args()
    if args.runs < 1 or min(args.releases, args.fonts, args.non_fonts, args.subs) < 0:
        parser.error("runs must be positive; workload sizes must be nonnegative")
    if not all((args.source_tree / name).is_file() for name in
               ("mkv_assets.py", "mkv_domain.py", "mkv_scan.py", "mkv_fonts.py")):
        parser.error("source-tree must contain the MKV Python source modules")

    sys.path.insert(0, str(args.source_tree.resolve()))
    from mkv_assets import SubIndex, prepare_release_directory
    from mkv_domain import EpisodeInfo
    from mkv_scan import ParsedFile

    # The baseline API predates the optional cache; all other inputs are identical.
    supports_cache = "font_cache" in inspect.signature(prepare_release_directory).parameters
    print(f"SYNTHETIC preparation only: {args.source_tree.resolve()}")
    print(f"releases={args.releases}, fonts={args.fonts}, non-fonts={args.non_fonts}, "
          f"subtitles in one bucket={args.subs}, runs={args.runs}")
    samples: dict[str, list[float]] = {"shared fonts": [], "subtitle insertion": []}
    with tempfile.TemporaryDirectory(prefix="mkv_bench_") as temp:
        root = Path(temp)
        fonts = root / "fonts"
        fonts.mkdir()
        for count, suffix in ((args.fonts, ".ttf"), (args.non_fonts, ".txt")):
            for i in range(count):
                (fonts / f"file{i}{suffix}").touch()
        releases = [root / f"Season{i}" for i in range(args.releases)]
        for release in releases:
            release.mkdir()

        for _ in range(args.runs):
            options: dict[str, Any] = {"fonts_dir_cache": {}, "scan_cache": {}}
            if supports_cache:
                options["font_cache"] = {}
            start = time.perf_counter()
            for release in releases:
                prepared = prepare_release_directory(release, root, [], **options)
                assert prepared.resolver.fonts.count == args.fonts
            samples["shared fonts"].append(time.perf_counter() - start)

            # Fresh Path objects keep their lazy hash computation inside each run.
            items = [ParsedFile(root / f"sub{i}.ass", f"sub{i}.ass", f"sub{i}",
                                EpisodeInfo("1", "E", "01")) for i in range(args.subs)]
            index = SubIndex()
            start = time.perf_counter()
            for item in items:
                index.add(item)
            samples["subtitle insertion"].append(time.perf_counter() - start)
            assert index.indexed_count == args.subs

    for name, times in samples.items():
        print(f"{name}: median {statistics.median(times) * 1000:.2f} ms "
              f"(min {min(times) * 1000:.2f}, max {max(times) * 1000:.2f})")


if __name__ == "__main__":
    main()
