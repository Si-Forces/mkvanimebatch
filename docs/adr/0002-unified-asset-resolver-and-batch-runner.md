# 0002. Asset Resolver and Runner Architecture

## Context
`mkv_remux.py` and `mkv_extract.py` previously had asset resolution logic and discovery scattered across multiple ad-hoc helper functions and parameter-heavy tuple returns. We evaluated whether to combine asset resolution and merge execution loops into a unified multi-tool `BatchRunner`/`PipelineStrategy` framework.

## Decision
1. **AssetResolver in `mkv_assets.py`**: We consolidate all asset discovery, indexing, and resolution rules (subtitles, chapters, covers, fonts) into `AssetResolver`. It owns local directory inspection, multi-level parent directory traversal, explicit `--assets-dir` fallback chains, language/track type scoring, and embedded source fallback behind `resolve_for(video, metadata)`. Local subtitles residing in the same release directory next to the MKV file take absolute precedence over any ancestor or external fallback subtitles during remux.
2. **Decoupled Runners**: We reject / defer introducing an artificial `BatchRunner` / `PipelineStrategy` abstraction layer. `mkv_remux.py` and `mkv_extract.py` have distinct operational pipelines (transcoding, subtitle shifting, font transaction management, mkvmerge vs mkvextract command topologies). Each runner keeps its own explicit execution loop and collaborators.
