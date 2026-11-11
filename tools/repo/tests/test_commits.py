import repo.commits as commitmsg


def test_accepts_valid_conventional_commit_subject():
    assert commitmsg.message_errors("feat(core): persist the session cursor\n") == []
    assert commitmsg.message_errors("docs: record the commit message convention\n") == []
    assert commitmsg.message_errors("docs: record the commit message convention\n\n") == []


def test_accepts_wrapped_body_trailer_and_unsplittable_line():
    text = "\n".join(
        [
            "feat(proto): reserve field 7",
            "",
            "Callers omit the field.",
            "https://example.com/" + ("a" * 80),
            "",
            "BREAKING CHANGE: callers omit field 7.",
            "Signed-off-by: WhaleRein <dev@example.com>",
            "",
        ]
    )
    assert commitmsg.message_errors(text) == []


def test_parser_ignores_comments_and_git_scissors_suffix():
    text = "\n".join(
        [
            "# comment",
            "fix(access): reject a digest conflict before the host runs",
            "# ------------------------ >8 ------------------------",
            "this commented diff is ignored even though it is far too long to wrap",
        ]
    )
    assert commitmsg.message_errors(text) == []


def test_rejects_empty_or_comment_only_commit_message():
    assert "empty" in commitmsg.message_errors("\n\n")[0]
    assert "empty" in commitmsg.message_errors("# just a comment\n")[0]


def test_rejects_header_without_conventional_commit_shape():
    for text in (
        "feature(core): add a cursor",
        "feat(api): add a cursor",
        "feat(core):missing space",
        "feat(core):  two spaces",
    ):
        assert commitmsg.message_errors(text)


def test_rejects_subject_over_limit_or_with_trailing_period():
    long_subject = "feat(core): " + ("persist " * 12)
    errors = commitmsg.message_errors("feat(core): persist the cursor.")
    assert "period" in errors[0]
    errors = commitmsg.message_errors(long_subject)
    assert "72" in errors[0]


def test_rejects_body_without_required_separator_or_wrapping():
    missing_blank = "feat(core): persist the cursor\nThe cursor is the resume point.\n"
    assert any("blank line" in item for item in commitmsg.message_errors(missing_blank))
    long_body = "feat(core): persist the cursor\n\n" + ("word " * 20) + "\n"
    assert any("body line" in item for item in commitmsg.message_errors(long_body))
    broken = "feat(proto): reserve field 7\n\nBREAKING CHANGE:\n"
    assert any("BREAKING CHANGE" in item for item in commitmsg.message_errors(broken))


def test_generated_merge_revert_fixup_and_squash_subjects_skip_format_check():
    for text in (
        'Merge branch "main"\n',
        'Revert "feat(core): persist the cursor"\n',
        "fixup! feat(core): persist the cursor\n",
        "squash! docs: record the convention\n",
    ):
        assert commitmsg.message_errors(text) == []


def test_commit_check_rejects_private_key_material_in_message():
    text = "feat(core): persist the cursor\n\n-----BEGIN PRIVATE KEY-----\n"
    assert any("private key" in item for item in commitmsg.message_errors(text))
    merge = 'Merge branch "main"\n-----BEGIN OPENSSH PRIVATE KEY-----\n'
    assert any("private key" in item for item in commitmsg.message_errors(merge))


def test_commit_check_rejects_sensitive_staged_paths_and_added_key_blocks():
    assert commitmsg.staged_errors(["README.md", ".env.example", "config.template"], "") == []
    problems = commitmsg.staged_errors(
        [".env", "certs/dev.crt", "id_ed25519", "secrets/demo.pem"],
        "",
    )
    assert len(problems) == 4
    patch = "\n".join(
        [
            "--- a/note.txt",
            "+++ b/note.txt",
            "------BEGIN PRIVATE KEY-----",
            "+-----BEGIN RSA PRIVATE KEY-----",
        ]
    )
    assert commitmsg.staged_errors(["note.txt"], patch) == ["staged diff contains a private key"]
    assert commitmsg.staged_errors(["note.txt"], "------BEGIN PRIVATE KEY-----\n") == []
