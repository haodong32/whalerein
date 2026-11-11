from repo.errors import (
    CommandFailedError,
    ProjectNotFoundError,
    RepoError,
    ToolMissingError,
    UsageError,
)


def test_error_codes_map_to_stable_process_exit_codes():
    assert RepoError("x").exit_code == 1
    assert UsageError("x").exit_code == 2
    assert UsageError("x").code == "usage_error"
    assert ToolMissingError("x").exit_code == 3
    assert ProjectNotFoundError("x").exit_code == 4
    assert CommandFailedError("x").code == "command_failed"
