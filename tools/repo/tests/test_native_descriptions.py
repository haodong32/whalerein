"""Keep native descriptions authored beside tests and reject missing metadata."""

import importlib.util
from pathlib import Path

import pytest


@pytest.fixture
def descriptions():
    path = Path(__file__).resolve().parents[3] / "tests/integration/drivers/go_descriptions.py"
    spec = importlib.util.spec_from_file_location("go_descriptions", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_reads_adjacent_native_comments_without_attaching_unrelated_comments(
    descriptions, tmp_path
):
    (tmp_path / "fixture_test.go").write_text("""package fixture
// When the pool closes, operations fail.
// They never report success.
func TestClosedPool(t *testing.T) {}
// This documents a helper, not the following case.
type helper struct{}
func TestUndocumented(t *testing.T) {}
""")
    assert descriptions.source_descriptions(tmp_path) == {
        "TestClosedPool": "When the pool closes, operations fail. They never report success."
    }


def test_export_rejects_compiled_case_without_authored_description(descriptions, tmp_path):
    output = tmp_path / "tests/integration/fixtures"
    output.mkdir(parents=True)
    (output / "store-cases.txt").write_text("TestMissingDescription\n")
    source = tmp_path / "access/internal/store"
    source.mkdir(parents=True)
    (source / "fixture_test.go").write_text(
        "package store\nfunc TestMissingDescription(t *testing.T) {}\n"
    )
    with pytest.raises(ValueError, match="Missing native test descriptions"):
        descriptions.export(tmp_path)
