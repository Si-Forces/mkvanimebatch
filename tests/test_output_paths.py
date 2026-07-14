"""Tests for remux output path handling shared by both variants."""

import importlib
from pathlib import Path

import pytest


REMUX_MODULES = [
    pytest.param(importlib.import_module("mkv_remux"), id="mkv_remux"),
    pytest.param(importlib.import_module("mkv_remux_fast"), id="mkv_remux_fast"),
]


@pytest.mark.parametrize("rmod", REMUX_MODULES)
def test_output_override_preserves_source_subdirectories(rmod, tmp_path):
    input_dir = tmp_path / "Anime"
    mkv_dir = input_dir / "Season 2" / "Arc"
    output_dir = tmp_path / "Remuxed"

    assert rmod.resolve_output_dir(mkv_dir, input_dir, output_dir) == (
        output_dir / "Season 2" / "Arc"
    )


@pytest.mark.parametrize("rmod", REMUX_MODULES)
def test_default_output_stays_next_to_source_directory(rmod, tmp_path):
    input_dir = tmp_path / "Anime"
    mkv_dir = input_dir / "Season 1"

    assert rmod.resolve_output_dir(mkv_dir, input_dir, None) == mkv_dir / "Output"


@pytest.mark.parametrize("rmod", REMUX_MODULES)
def test_zero_byte_output_is_not_complete(rmod, tmp_path):
    output = tmp_path / "episode.mkv"
    output.touch()

    assert rmod.is_completed_output(output) is False


@pytest.mark.parametrize("rmod", REMUX_MODULES)
def test_nonempty_output_is_complete(rmod, tmp_path):
    output = tmp_path / "episode.mkv"
    output.write_bytes(b"mkv")

    assert rmod.is_completed_output(output) is True
