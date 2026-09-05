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
    parser.add_argument("--chapters", type=int, default=100)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument(
        "--bench-scheduling",
        action="store_true",
        help="Measure inspection scheduling at 1k/10k items with media mocked; compare using --source-tree",
    )
    args = parser.parse_args()
    if args.runs < 1 or min(args.releases, args.fonts, args.non_fonts, args.subs, args.chapters) < 0:
        parser.error("runs must be positive; workload sizes must be nonnegative")
    if not all((args.source_tree / name).is_file() for name in
               ("mkv_assets.py", "mkv_domain.py", "mkv_scan.py", "mkv_fonts.py", "mkv_manifest.py")):
        parser.error("source-tree must contain the MKV Python source modules")
    if args.bench_scheduling:
        run_scheduling_benchmark(args.source_tree, runs=args.runs)
        return

    sys.path.insert(0, str(args.source_tree.resolve()))
    import mkv_scan
    from mkv_assets import SubIndex, prepare_release_directory
    from mkv_domain import EpisodeInfo
    from mkv_manifest import generate_inspection_manifest
    from mkv_scan import ParsedFile, PersistentInspectCache

    # The baseline API predates the optional cache; all other inputs are identical.
    supports_cache = "font_cache" in inspect.signature(prepare_release_directory).parameters
    print(f"SYNTHETIC preparation only: {args.source_tree.resolve()}")
    print(f"releases={args.releases}, fonts={args.fonts}, non-fonts={args.non_fonts}, "
          f"subtitles in one bucket={args.subs}, shared chapter XMLs={args.chapters}, runs={args.runs}")
    samples: dict[str, list[float]] = {"shared fonts": [], "subtitle insertion": [], "warm manifest": []}
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

        # Warm probe cache isolates directory loading from subprocess/media costs.
        cache = PersistentInspectCache(cache_file=root / "inspect_cache.json")
        for release in releases:
            video = release / "Show - 01.mkv"
            video.touch()
            (release / "Show - 01.ass").touch()
            cache.put(video, {"tracks": [], "attachments": []})
        cache.save()
        for i in range(args.chapters):
            (root / f"chapter{i}.xml").write_text("<Chapters></Chapters>", encoding="utf-8")
        def unexpected_probe(*_args: Any, **_kwargs: Any) -> Any:
            raise AssertionError("Synthetic warm manifest must not invoke media tools")
        mkv_scan.run_mkvmerge_json = unexpected_probe
        for _ in range(args.runs):
            start = time.perf_counter()
            manifest = generate_inspection_manifest(root, persistent_cache=cache)
            samples["warm manifest"].append(time.perf_counter() - start)
            assert len(manifest["items"]) == args.releases
            assert len(manifest["subtitle_candidates"]) == args.releases
            assert all("inspection_error" not in item for item in manifest["items"])

    for name, times in samples.items():
        print(f"{name}: median {statistics.median(times) * 1000:.2f} ms "
              f"(min {min(times) * 1000:.2f}, max {max(times) * 1000:.2f})")


def run_scheduling_benchmark(source_tree: Path, runs: int = 3, workers: int = 4) -> None:
    import tracemalloc
    from concurrent.futures import ThreadPoolExecutor

    sys.path.insert(0, str(source_tree.resolve()))
    import mkv_scan
    from mkv_scan import inspect_videos

    # Media tools are mocked so no external subprocesses or files are required
    mkv_scan.run_mkvmerge_json = lambda *a, **k: {"tracks": []}

    print(f"SYNTHETIC task scheduling benchmark: {source_tree.resolve()}")
    print(f"workers={workers}, runs={runs}; media mocked, timing excludes tracemalloc overhead")

    for count in (1000, 10000):
        videos = [Path(f"/mock/ep_{i:05d}.mkv") for i in range(count)]
        with ThreadPoolExecutor(max_workers=workers) as pool:
            inspect_videos(videos[:16], pool, force_refresh=True)
            times: list[float] = []
            peaks: list[int] = []
            for _ in range(runs):
                t0 = time.perf_counter()
                result = inspect_videos(videos, pool, force_refresh=True)
                times.append(time.perf_counter() - t0)
                assert len(result) == count and all(value.video is not None for value in result.values())
                del result
                tracemalloc.start()
                result = inspect_videos(videos, pool, force_refresh=True)
                _, peak = tracemalloc.get_traced_memory()
                tracemalloc.stop()
                peaks.append(peak)
                del result
            print(f"{count} items: median {statistics.median(times) * 1000:.2f} ms; "
                  f"median peak Python allocations {statistics.median(peaks) / 1024**2:.2f} MiB")
    print("Not OS RSS or media throughput. Result maps remain O(N).")


if __name__ == "__main__":
    main()
