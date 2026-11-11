from pathlib import Path

import pytest

from repo.errors import RepoError
from repo.paths import ROOT_ENV, find_root, is_skipped


def test_finds_repository_root_from_package_location():
    root = find_root()
    assert (root / "AGENTS.md").is_file()


def test_uses_root_environment_path_when_configured(tmp_path, monkeypatch, repo_tree):
    monkeypatch.setenv(ROOT_ENV, str(tmp_path))
    assert find_root() == tmp_path.resolve()


def test_rejects_nonexistent_root_environment_path(tmp_path, monkeypatch):
    monkeypatch.setenv(ROOT_ENV, str(tmp_path / "missing"))
    with pytest.raises(RepoError):
        find_root()


def test_rejects_search_path_without_repository_markers(tmp_path, monkeypatch):
    monkeypatch.delenv(ROOT_ENV, raising=False)
    with pytest.raises(RepoError):
        find_root(tmp_path)


@pytest.mark.parametrize(
    "rel,expected",
    [
        ("node_modules/x", True),
        ("target/x", True),
        ("core/src/lib.rs", False),
        (".agents/x", True),
        ("docs/plans/x.md", False),
    ],
    ids=[
        "ignores-node-modules",
        "ignores-rust-target",
        "keeps-rust-source",
        "ignores-agent-dir",
        "tracks-local-plans",
    ],
)
def test_skips_only_configured_repository_paths(repo_tree, rel, expected):
    assert is_skipped(repo_tree / rel, repo_tree) is expected


def test_skips_paths_outside_repository_root(tmp_path):
    assert is_skipped(Path("/elsewhere/file"), tmp_path) is True
