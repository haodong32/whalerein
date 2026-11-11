import pytest

from repo.runners.coverage import (
    extract_json,
    parse_go_coverprofile,
    parse_llvm_cov_json,
)

GO_PROFILE = """mode: set
example.com/a/pkg/a.go:1.1,1.10 2 1
example.com/a/pkg/a.go:2.1,2.10 2 0
example.com/a/other/b.go:1.1,1.10 1 1
"""


def test_parser_extracts_package_coverage_from_go_coverprofile():
    parsed = parse_go_coverprofile(GO_PROFILE)
    assert parsed["example.com/a/pkg"] == {"covered": 2, "total": 4}
    assert parsed["example.com/a/other"] == {"covered": 1, "total": 1}
    assert parse_go_coverprofile("mode: set\nbad line") == {}


def test_parser_extracts_line_coverage_from_llvm_json():
    data = {"data": [{"totals": {"lines": {"covered": 8, "count": 10, "percent": 80.0}}}]}
    assert parse_llvm_cov_json(data) == (8, 10, 80.0)
    with pytest.raises(ValueError):
        parse_llvm_cov_json({})
    with pytest.raises(ValueError):
        parse_llvm_cov_json({"data": []})
    data = {"data": [{"totals": {"lines": {"covered": 1, "count": 2}}}]}
    assert parse_llvm_cov_json(data)[2] == 50.0


def test_json_extractor_finds_payload_after_tool_output():
    assert extract_json('warning\n{"a": 1}\n') == {"a": 1}
    assert extract_json("[1,2]") == [1, 2]
    with pytest.raises(ValueError):
        extract_json("")
    with pytest.raises(ValueError):
        extract_json("no json")
    with pytest.raises(ValueError):
        extract_json("{bad")
