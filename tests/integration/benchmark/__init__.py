"""Container benchmark fixture.

The package emits raw observed facts only. Attempt classification,
percentiles, exemplar selection and verdicts belong to
``repo.benchmark.model``; the fixture never re-implements those rules.

``main.py`` is the container entry point. The remaining modules are pure
helpers so the fixture logic can be imported and unit-tested without a
container, Docker or a Linux host.
"""
