"""CLI/request validation and deterministic integration shard partitioning."""

import importlib.util
import zlib
from pathlib import Path

import pytest

from repo import cli, testing
from repo.errors import UsageError


@pytest.fixture(scope="module")
def sharding():
    path = Path(__file__).resolve().parents[3] / "tests/integration/sharding.py"
    spec = importlib.util.spec_from_file_location("integration_sharding", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


@pytest.mark.parametrize(
    "shard",
    ["0/1", "0/2", "1/2", "7/8", "00/02"],
)
def test_valid_shard_spellings(sharding, shard):
    index, count = sharding.parse_shard(shard)
    assert 0 <= index < count


@pytest.mark.parametrize(
    "shard",
    ["", "0", "1/0", "2/2", "-1/2", "a/2", "1/2/3"],
)
def test_invalid_shard_spellings(sharding, shard):
    with pytest.raises(ValueError):
        sharding.parse_shard(shard)


@pytest.mark.parametrize("count", range(1, 9))
def test_partition_is_a_stable_exact_cover(sharding, count):
    identities = [f"cases/test_{number}.py::test_case_{number}" for number in range(500)]
    shards = [sharding.partition(identities, index, count) for index in range(count)]
    flattened = [identity for shard in shards for identity in shard]
    assert sorted(flattened) == sorted(identities)
    assert len(flattened) == len(set(flattened)) == len(identities)
    for identity in identities:
        owners = [index for index, shard in enumerate(shards) if identity in shard]
        assert owners == [zlib.crc32(identity.encode("utf-8")) % count]


def test_partition_is_repeatable_and_accepts_case_objects(sharding):
    from types import SimpleNamespace

    items = [SimpleNamespace(nodeid=f"case::{name}") for name in ("a", "b", "c", "d")]
    first = sharding.partition(items, 0, 2)
    second = sharding.partition(items, 0, 2)
    assert [item.nodeid for item in first] == [item.nodeid for item in second]
    assert all(sharding.bucket(item.nodeid, 2) == 0 for item in first)


@pytest.mark.parametrize(
    "candidate",
    [
        testing.TestRequest(parallel=1),
        testing.TestRequest(suite="all", parallel=2),
        testing.TestRequest(suite="integration", parallel=2, list_only=True),
        testing.TestRequest(suite="integration", parallel=0),
        testing.TestRequest(suite="integration", parallel=9),
    ],
)
def test_parallel_request_validation(candidate):
    with pytest.raises(UsageError):
        candidate.validate()


@pytest.mark.parametrize("parallel", [1, 2, 8, None])
def test_parallel_is_valid_only_for_integration(parallel):
    request = testing.TestRequest(suite="integration", parallel=parallel)
    request.validate()
    assert request.parallel == parallel


def test_parser_exposes_parallel_and_defaults_to_single_container():
    parser = cli.build_parser()
    parsed = parser.parse_known_args(["test", "integration", "--parallel", "3"])
    assert parsed[0].parallel == 3
    default = parser.parse_known_args(["test", "integration"])[0]
    assert default.parallel is None


def test_parser_help_documents_parallel_limits(capsys):
    with pytest.raises(SystemExit) as raised:
        cli.build_parser().parse_known_args(["test", "--help"])
    assert raised.value.code == 0
    assert "--parallel" in capsys.readouterr().out
