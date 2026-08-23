# 0003. Transactional Font Extraction and Concurrency Safety

## Context
When extracting fonts concurrently from multiple MKV files in a release directory, worker threads frequently encounter identical font attachments (e.g. common subtitle fonts across 12-24 episodes). Simultaneous writes to the same destination font files can cause corrupted font binaries, race conditions, and disk I/O thrashing.

## Decision
1. **Two-Phase Font Extraction**: We establish a transactional `FontManager` in `mkv_fonts.py` that arbitrates font claims across worker threads:
   - **Claim Phase (`claim`)**: Threads register intent to extract a font by name and size. If another worker already extracted or claimed the font, duplicate extraction is suppressed.
   - **Staging**: Extracted fonts are written to isolated temporary staging paths in `.font_staging/`.
   - **Promote Phase (`promote`)**: On successful extraction, staged fonts are atomically moved to the destination fonts directory with content hash verification (`hash_file` using BLAKE2b) and collision renaming.
   - **Release Phase (`release`)**: On job failure or worker crash, unpromoted claims are cleanly released so subsequent workers can retry.
2. **Staging Cleanup**: Post-batch completion, staging directories are automatically pruned without leaving orphaned artifacts.
