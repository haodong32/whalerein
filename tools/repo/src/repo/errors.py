"""Stable error types and exit codes for the repo manager."""

from __future__ import annotations

EXIT_OK = 0
EXIT_FAILURE = 1
EXIT_USAGE = 2
EXIT_TOOL_MISSING = 3
EXIT_NOT_FOUND = 4

THRESHOLD_PERCENT = 90.0


class RepoError(Exception):
    """Base error with a stable machine-readable code and exit status."""

    code = "repo_error"
    exit_code = EXIT_FAILURE

    def __init__(self, message: str) -> None:
        super().__init__(message)
        self.message = message


class UsageError(RepoError):
    code = "usage_error"
    exit_code = EXIT_USAGE


class ToolMissingError(RepoError):
    code = "tool_missing"
    exit_code = EXIT_TOOL_MISSING


class ProjectNotFoundError(RepoError):
    code = "project_not_found"
    exit_code = EXIT_NOT_FOUND


class CommandFailedError(RepoError):
    code = "command_failed"
    exit_code = EXIT_FAILURE
