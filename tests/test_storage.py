import json
from pathlib import Path

from conftest import CLAUDE_FILE, CODEX_FILE, FORBIDDEN

from human_history.storage import search as store
from human_history.storage.indexer import index_all
from human_history.utils.git import locate

ALL = store.Filters(project="all")


def keys(rows):
    return [r.key for r in rows]


def test_index_and_scoped_search(config, db):
    stats = index_all(db, config)
    assert stats.parsed == 3 and stats.skipped_lines == 2

    # user-only vs assistant-only vs both
    assert keys(store.search(db, "SQLAlchemy", "user", ALL)) == ["claude:11111111-aaaa-4bbb-8ccc-000000000001"]
    assert store.search(db, "SQLAlchemy", "assistant", ALL) == []
    assert keys(store.search(db, "joinedload", "assistant", ALL)) == ["claude:11111111-aaaa-4bbb-8ccc-000000000001"]
    assert store.search(db, "joinedload", "user", ALL) == []
    both = store.search(db, "approval", "both", ALL)
    assert len(both) == 2
    hit = store.search(db, "joinedload", "both", ALL)[0]
    assert hit.hit_role == "assistant" and store.HL_START in hit.snippet and hit.hits == 1

    # prefix matching of the last word, phrase search, AND semantics
    assert store.search(db, "joinedl", "both", ALL)
    assert store.search(db, '"composite index"', "assistant", ALL)
    assert not store.search(db, "composite SQLAlchemy", "both", ALL)


def test_forbidden_content_never_indexed(config, db):
    index_all(db, config)
    for marker in FORBIDDEN:
        for scope in ("user", "assistant", "both"):
            assert store.search(db, marker, scope, ALL) == [], marker
        assert db.execute("SELECT COUNT(*) FROM messages WHERE text LIKE ?", (f"%{marker}%",)).fetchone()[0] == 0
    # files.state is the only other place derived text lives; it must not carry tool output either
    for (state,) in db.execute("SELECT state FROM files"):
        assert not any(m in state for m in FORBIDDEN)


def test_titles_counts_and_fuzzy_title_search(config, db):
    index_all(db, config)
    rows = {r.key: r for r in store.list_sessions(db, ALL)}
    claude = rows["claude:11111111-aaaa-4bbb-8ccc-000000000001"]
    assert (claude.title, claude.user_count, claude.assistant_count) == ("Approvals perf", 4, 3)
    assert rows["codex:22222222-bbbb-4ccc-8ddd-000000000002"].title == "Approval workflow"  # session_index
    assert rows["codex:33333333-cccc-4ddd-8eee-000000000003"].title == "Rename the legacy module"  # 1st prompt
    assert keys(store.search(db, "aprvl wrkflw", "title", ALL)) == ["codex:22222222-bbbb-4ccc-8ddd-000000000002"]
    assert store.search(db, "perf", "title", ALL)[0].title == "Approvals perf"
    assert store.fuzzy_score("xyz", "Approvals perf") is None


def test_incremental_append_and_rewrite(config, db, logs: Path):
    index_all(db, config)
    path = logs / "claude" / CLAUDE_FILE
    with path.open("a") as fh:  # completes the trailing partial line + one more prompt
        fh.write(
            "\n"
            + json.dumps(
                {
                    "type": "user",
                    "uuid": "u-new",
                    "timestamp": "2026-10-01T11:00:00Z",
                    "cwd": "/work/app",
                    "sessionId": "11111111-aaaa-4bbb-8ccc-000000000001",
                    "message": {"role": "user", "content": "One more"},
                }
            )
            + "\n"
        )
    stats = index_all(db, config)
    assert stats.parsed == 1 and stats.messages == 2
    assert index_all(db, config).parsed == 0  # nothing changed -> nothing parsed

    # rewrite (shorter file): old messages are dropped, not duplicated
    path.write_text(
        json.dumps(
            {
                "type": "user",
                "uuid": "z",
                "timestamp": "2026-10-03T10:00:00Z",
                "cwd": "/work/app",
                "sessionId": "11111111-aaaa-4bbb-8ccc-000000000001",
                "message": {"role": "user", "content": "Fresh start"},
            }
        )
        + "\n"
    )
    index_all(db, config)
    msgs = store.messages(db, "claude:11111111-aaaa-4bbb-8ccc-000000000001")
    assert [m["text"] for m in msgs] == ["Fresh start"]
    assert store.search(db, "SQLAlchemy", "both", ALL) == []


def test_deleted_log_removes_session(config, db, logs: Path):
    index_all(db, config)
    (logs / "codex" / "sessions" / CODEX_FILE).unlink()
    assert index_all(db, config).removed_sessions == 1
    assert "codex:22222222-bbbb-4ccc-8ddd-000000000002" not in keys(store.list_sessions(db, ALL))


def _make_repo(tmp_path: Path) -> tuple[Path, Path, Path]:
    """main checkout + linked worktree sharing one .git, and an unrelated directory."""
    main = tmp_path / "repo"
    (main / ".git" / "worktrees" / "wt").mkdir(parents=True)
    (main / ".git" / "worktrees" / "wt" / "commondir").write_text("../..\n")
    (main / ".git" / "worktrees" / "wt" / "HEAD").write_text("ref: refs/heads/feature\n")
    (main / ".git" / "HEAD").write_text("ref: refs/heads/main\n")
    (main / "src").mkdir()
    wt = tmp_path / "wt"
    wt.mkdir()
    (wt / ".git").write_text(f"gitdir: {main / '.git' / 'worktrees' / 'wt'}\n")
    other = tmp_path / "other"
    other.mkdir()
    return main, wt, other


def test_git_locate(tmp_path: Path):
    main, wt, other = _make_repo(tmp_path)
    assert locate(str(main / "src")).worktree == str(main)
    assert locate(str(wt)).worktree == str(wt)
    assert locate(str(wt)).repo == locate(str(main)).repo
    assert locate(str(other)).repo is None


def test_worktree_vs_repository_filter(config, db, logs: Path, tmp_path: Path):
    main, wt, other = _make_repo(tmp_path)
    claude_dir = logs / "claude"
    for name, cwd in (("main", main / "src"), ("wt", wt), ("other", other), ("gone", wt / "deleted-subdir")):
        (claude_dir / name).mkdir()
        (claude_dir / name / f"{name}.jsonl").write_text(
            json.dumps(
                {
                    "type": "user",
                    "uuid": name,
                    "timestamp": "2026-10-05T10:00:00Z",
                    "cwd": str(cwd),
                    "sessionId": name,
                    "message": {"role": "user", "content": f"hello from {name}"},
                }
            )
            + "\n"
        )
    index_all(db, config)

    def ids(project, cwd):
        return sorted(
            r.session_id
            for r in store.list_sessions(db, store.Filters(project=project, cwd=str(cwd)))
            if not r.session_id.startswith(("1111", "2222", "3333"))
        )

    # A deleted subdirectory still resolves to the nearest existing ancestor checkout.
    assert ids("current-worktree", wt) == ["gone", "wt"]
    assert ids("current-worktree", main) == ["main"]
    assert ids("repository", wt) == ["gone", "main", "wt"]
    assert ids("repository", main) == ["gone", "main", "wt"]
    assert ids("all", wt) == ["gone", "main", "other", "wt"]
    # exact cwd filter is distinct from worktree filtering
    assert [r.session_id for r in store.list_sessions(db, store.Filters(exact_cwd=str(main / "src")))] == ["main"]


def test_inline_filters(config, db):
    index_all(db, config)
    text, f = store.split_filters("approval agent:codex since:2026-10-01", ALL)
    assert text == "approval" and f.agent == "codex" and f.since == "2026-10-01"
    rows = store.search(db, text, "both", f)
    assert keys(rows) == ["codex:22222222-bbbb-4ccc-8ddd-000000000002"]
    _, f = store.split_filters("branch:legacy", ALL)
    assert keys(store.list_sessions(db, f)) == ["codex:33333333-cccc-4ddd-8eee-000000000003"]


def test_favorites_pinned_first(config, db):
    index_all(db, config)
    last = store.list_sessions(db, ALL)[-1]
    assert store.toggle_favorite(db, last.key) is True
    first = store.list_sessions(db, ALL)[0]
    assert first.key == last.key and first.favorite
    assert store.toggle_favorite(db, last.key) is False


def test_fts_query_is_injection_safe():
    assert store.fts_query('foo" OR bar NEAR(') == '"foo" "OR" "bar" "NEAR("*'
    assert store.fts_query("   ") is None
