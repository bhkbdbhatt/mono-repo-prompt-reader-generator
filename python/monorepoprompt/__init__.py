"""Thin Python SDK over the monorepoprompt CLI binary.

Every method shells out to the compiled Rust binary so behaviour matches the
CLI exactly. Nothing is reimplemented in Python.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

__all__ = [
    "MonorepoPrompt",
    "MonorepoPromptError",
    "BinaryNotFound",
    "Selection",
    "PromptResult",
    "find_binary",
]

_DEFAULT_BINARY_NAMES = ("monorepoprompt", "monorepoprompt.exe")


class MonorepoPromptError(RuntimeError):
    """Raised when the underlying CLI fails."""


class BinaryNotFound(MonorepoPromptError):
    """Raised when the monorepoprompt binary cannot be located."""


def find_binary(explicit: str | os.PathLike[str] | None = None) -> str:
    """Locate the CLI binary.

    Search order: explicit argument, MONOREPROMPT_BIN env var, the workspace
    target/debug and target/release directories, then PATH.
    """
    if explicit:
        path = Path(explicit)
        if not path.is_file():
            raise BinaryNotFound(f"binary not found at {path}")
        return str(path)

    env = os.environ.get("MONOREPROMPT_BIN")
    if env:
        return find_binary(env)

    root = Path(__file__).resolve().parents[2]
    for profile in ("release", "debug"):
        for name in _DEFAULT_BINARY_NAMES:
            candidate = root / "target" / profile / name
            if candidate.is_file():
                return str(candidate)

    for name in _DEFAULT_BINARY_NAMES:
        found = shutil.which(name)
        if found:
            return found

    raise BinaryNotFound(
        "monorepoprompt binary not found. Build it with `cargo build --release` "
        "or set MONOREPROMPT_BIN."
    )


@dataclass
class Selection:
    """A file chosen for the code context, with its assigned detail level."""

    path: str
    package: str
    detail_level: str
    score: float

    @classmethod
    def from_json(cls, raw: dict[str, Any]) -> "Selection":
        return cls(
            path=raw["path"],
            package=raw.get("package", ""),
            detail_level=raw.get("detail_level", "codemap"),
            score=float(raw.get("score", 0.0)),
        )


@dataclass
class PromptResult:
    """Assembled prompt plus the accounting needed to debug budgets."""

    prompt: str
    token_count: int
    budget: int
    selected: list[Selection] = field(default_factory=list)
    dropped: list[str] = field(default_factory=list)

    @property
    def within_budget(self) -> bool:
        return self.token_count <= self.budget


class MonorepoPrompt:
    """Programmatic access to the layered context engine.

    >>> mrp = MonorepoPrompt(".")
    >>> result = mrp.build("trace the auth flow")
    >>> result.within_budget
    True
    """

    def __init__(
        self,
        root: str | os.PathLike[str],
        *,
        binary: str | os.PathLike[str] | None = None,
        budget: int | None = None,
        role: str | None = None,
        config: str | os.PathLike[str] | None = None,
    ) -> None:
        self.root = Path(root).resolve()
        if not self.root.is_dir():
            raise MonorepoPromptError(f"not a directory: {self.root}")
        self.binary = find_binary(binary)
        self.budget = budget
        self.role = role
        self.config = str(config) if config else None

    # -- process plumbing -------------------------------------------------

    def _global_flags(self) -> list[str]:
        flags: list[str] = []
        if self.budget is not None:
            flags += ["--budget", str(self.budget)]
        if self.role:
            flags += ["--role", self.role]
        if self.config:
            flags += ["--config", self.config]
        return flags

    def _run(self, args: list[str], *, capture_json: bool = False) -> subprocess.CompletedProcess[str]:
        cmd = [self.binary, *self._global_flags(), *args, str(self.root)]
        proc = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
        if proc.returncode != 0:
            raise MonorepoPromptError(
                f"command failed ({proc.returncode}): {' '.join(cmd)}\n{proc.stderr.strip()}"
            )
        if capture_json:
            return subprocess.CompletedProcess(cmd, 0, proc.stdout, proc.stderr)
        return proc

    # -- stage 1 ----------------------------------------------------------

    def scan(self) -> dict[str, Any]:
        """Stage 1. Return the repo manifest as a dict."""
        proc = self._run(["scan"], capture_json=True)
        try:
            return json.loads(proc.stdout)
        except json.JSONDecodeError as exc:  # pragma: no cover - defensive
            raise MonorepoPromptError(f"could not parse manifest: {exc}") from exc

    # -- stage 2 ----------------------------------------------------------

    def architecture_map(self, *, mermaid: bool = False) -> str:
        """Stage 2. Return the architecture map as Markdown."""
        args = ["map"]
        if mermaid:
            args.append("--mermaid")
        proc = self._run(args)
        return proc.stdout

    # -- stage 3 ----------------------------------------------------------

    def select_files(self, task: str, budget: int | None = None) -> list[Selection]:
        """Stage 3. Rank files against the task and return the selection."""
        with self._temp_file("selected.json") as tmp:
            args = ["build", "--task", task, "--selection", str(tmp), "--output", os.devnull]
            if budget is not None:
                args += ["--budget", str(budget)]
            self._run(args, capture_json=True)
            raw = json.loads(tmp.read_text(encoding="utf-8"))
        return [Selection.from_json(item) for item in raw]

    # -- stage 4 ----------------------------------------------------------

    def build(self, task: str, *, budget: int | None = None) -> PromptResult:
        """Stage 4. Assemble the full layered prompt."""
        import tempfile

        with tempfile.TemporaryDirectory() as tmpdir:
            prompt_path = Path(tmpdir) / "prompt.md"
            selection_path = Path(tmpdir) / "selected.json"
            args = [
                "build",
                "--task",
                task,
                "--output",
                str(prompt_path),
                "--selection",
                str(selection_path),
                "--explain",
            ]
            if budget is not None:
                args += ["--budget", str(budget)]
            proc = self._run(args, capture_json=True)
            prompt = prompt_path.read_text(encoding="utf-8")
            selected = [
                Selection.from_json(item) for item in json.loads(selection_path.read_text("utf-8"))
            ]

        token_count = _parse_tokens(proc.stderr)
        effective_budget = budget if budget is not None else (self.budget or 60_000)
        return PromptResult(
            prompt=prompt,
            token_count=token_count,
            budget=effective_budget,
            selected=selected,
            dropped=_parse_dropped(proc.stderr),
        )

    # -- stage 5 ----------------------------------------------------------

    def next_phase(
        self,
        task: str,
        llm_response: str,
        *,
        state: dict[str, Any] | None = None,
        budget: int | None = None,
    ) -> PromptResult:
        """Stage 5. Feed a model reply back in and get the next prompt."""
        import tempfile

        with tempfile.TemporaryDirectory() as tmpdir:
            response_path = Path(tmpdir) / "response.md"
            prompt_path = Path(tmpdir) / "next.md"
            response_path.write_text(llm_response, encoding="utf-8")
            args = [
                "next",
                "--task",
                task,
                "--response",
                str(response_path),
                "--output",
                str(prompt_path),
            ]
            if state is not None:
                state_path = Path(tmpdir) / "state.json"
                state_path.write_text(json.dumps(state), encoding="utf-8")
                args += ["--state", str(state_path)]
            if budget is not None:
                args += ["--budget", str(budget)]
            proc = self._run(args, capture_json=True)
            prompt = prompt_path.read_text(encoding="utf-8")

        effective_budget = budget if budget is not None else (self.budget or 60_000)
        return PromptResult(
            prompt=prompt,
            token_count=_count_tokens(prompt),
            budget=effective_budget,
        )

    # -- helpers ----------------------------------------------------------

    class _TempFile:
        def __init__(self, name: str) -> None:
            import tempfile

            self._dir = tempfile.TemporaryDirectory()
            self._path = Path(self._dir.name) / name

        def __enter__(self) -> Path:
            return self._path

        def __exit__(self, *exc: object) -> None:
            self._dir.cleanup()

    def _temp_file(self, name: str) -> "_TempFile":
        return MonorepoPrompt._TempFile(name)


def _parse_tokens(stderr: str) -> int:
    """Extract the reported token count from the CLI's stderr summary."""
    import re

    match = re.search(r"prompt:\s*(\d+)\s*tokens", stderr)
    return int(match.group(1)) if match else 0


def _parse_dropped(stderr: str) -> list[str]:
    import re

    match = re.search(r"dropped:\s*(.+)", stderr)
    if not match:
        return []
    return [p.strip() for p in match.group(1).split(",") if p.strip()]


def _count_tokens(text: str) -> int:
    """Rough fallback count, ~4 chars per token. Prefer the CLI's tiktoken number."""
    return max(1, len(text) // 4) if text else 0