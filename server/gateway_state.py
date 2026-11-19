"""Modes, glossary and text-only history for the threaded gateway."""

import difflib
import json
import logging
import os
import re
import sqlite3
import tempfile
import threading
import tomllib
import unicodedata
from contextlib import contextmanager
from datetime import datetime, timezone

NAME = re.compile(r"^[a-z0-9-]{1,32}$")


class Modes:
    def __init__(self, path, prompt):
        self.path = path
        self.builtins = {
            "clean": {"description": "Punctuation, numbers, no filler words", "prompt": prompt, "drift_guard": True},
            "raw": {"description": "As heard", "drift_guard": True},
        }
        self.current = self.builtins.copy()
        self.lock = threading.Lock()

    def read(self):
        with self.lock:
            try:
                with self.path.open("rb") as file:
                    config = tomllib.load(file)
            except FileNotFoundError:
                config = {}
            except (OSError, ValueError) as error:
                logging.warning("Cannot read modes; keeping the last valid version: %s", error)
                return self.current.copy()
            modes = {}
            for name, mode in config.items():
                if (not NAME.fullmatch(name) or not isinstance(mode, dict)
                        or not isinstance(mode.get("description", ""), str)
                        or ("prompt" in mode and not isinstance(mode["prompt"], str))
                        or not isinstance(mode.get("drift_guard", True), bool)):
                    logging.warning("Skipping invalid mode %r", name)
                    continue
                modes[name] = mode.copy()
            for name, mode in self.builtins.items():
                modes.setdefault(name, mode)
            self.current = modes
            return modes.copy()

    def resolve(self, name):
        modes = self.read()
        name = name if name in modes else "clean"
        return name, modes[name]


def glossary_entries(path):
    """[(term, [heard-as spellings])] from glossary lines `Term` or `Term: heard, heard`."""
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except FileNotFoundError:
        return []
    entries = []
    for line in lines:
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        # Hints follow ": " so a plain term may still contain a colon (Re:Zero).
        term, _, heard = line.partition(": ") if ": " in line else (line, "", "")
        term = term.strip()
        if term:
            entries.append((term, [h.strip() for h in heard.split(",") if h.strip()]))
    return entries


def read_glossary(path):
    return [term for term, _ in glossary_entries(path)]


def read_glossary_hints(path):
    """{term: [heard-as spellings]} for the lines that carry any."""
    return {term: heard for term, heard in glossary_entries(path) if heard}


def line_term(line):
    return line.partition(": ")[0].strip()


def validate_term(term):
    if not isinstance(term, str):
        raise ValueError("term must be a string.")
    if any(unicodedata.category(char) in {"Cc", "Zl", "Zp"} for char in term):
        raise ValueError("A term must be on one line without control characters.")
    term = term.strip()
    if not 1 <= len(term) <= 64 or "," in term or ": " in term or term.startswith("#"):
        raise ValueError("A term must be 1–64 characters, without commas, a colon before a space or a leading #.")
    return term


class Glossary:
    def __init__(self, path):
        self.path = path
        self.lock = threading.Lock()

    def read(self):
        with self.lock:
            return read_glossary(self.path)

    def read_hints(self):
        with self.lock:
            return read_glossary_hints(self.path)

    def write(self, term, remove=False):
        term = validate_term(term)
        with self.lock:
            try:
                contents = self.path.read_text(encoding="utf-8")
            except FileNotFoundError:
                contents = ""
            lines = contents.splitlines(keepends=True)
            matches = [i for i, line in enumerate(lines)
                       if not line.lstrip().startswith("#") and line_term(line).casefold() == term.casefold()]
            changed = bool(matches) if remove else not matches
            if changed:
                if remove:
                    contents = "".join(line for i, line in enumerate(lines) if i not in matches)
                else:
                    contents += ("\n" if contents and not contents.endswith("\n") else "") + term + "\n"
                # Same-directory rename makes readers see either complete version.
                name = None
                try:
                    with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=self.path.parent,
                                                     prefix=".glossary-", delete=False) as file:
                        name = file.name
                        file.write(contents)
                        file.flush()
                        os.fsync(file.fileno())
                    os.replace(name, self.path)
                finally:
                    if name and os.path.exists(name):
                        os.unlink(name)
            return read_glossary(self.path), changed


def integer(params, name, default=None, maximum=None):
    values = params.get(name)
    if values is None:
        return default
    if len(values) != 1 or not re.fullmatch(r"[0-9]+", values[0]):
        raise ValueError(f"{name} must be a positive integer.")
    try:
        value = int(values[0])
    except ValueError:
        raise ValueError(f"{name} must be a positive integer.") from None
    if value < 1 or (maximum is not None and value > maximum):
        raise ValueError(f"{name} must be between 1 and {maximum}." if maximum else f"{name} must be positive.")
    return value


def parameter(params, name, default=""):
    values = params.get(name, [default])
    if len(values) != 1:
        raise ValueError(f"Send {name} only once.")
    return values[0]


def utc_time(value):
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


class History:
    columns = "time device client mode audio_seconds asr_seconds cleanup_seconds cleanup raw text language".split()

    def __init__(self, state, modes):
        self.path = state / "history.sqlite3"
        with self.connect() as db:
            db.execute("PRAGMA journal_mode=WAL")
            db.executescript("""
                CREATE TABLE IF NOT EXISTS history (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    time TEXT NOT NULL, device TEXT NOT NULL, client TEXT NOT NULL, mode TEXT NOT NULL,
                    audio_seconds REAL NOT NULL, asr_seconds REAL NOT NULL, cleanup_seconds REAL NOT NULL,
                    cleanup TEXT NOT NULL, raw TEXT NOT NULL, text TEXT NOT NULL
                );
                CREATE VIRTUAL TABLE IF NOT EXISTS history_fts USING fts5(
                    raw, text, content='history', content_rowid='id'
                );
                CREATE TRIGGER IF NOT EXISTS history_insert AFTER INSERT ON history BEGIN
                    INSERT INTO history_fts(rowid, raw, text) VALUES (new.id, new.raw, new.text);
                END;
                CREATE TRIGGER IF NOT EXISTS history_delete AFTER DELETE ON history BEGIN
                    INSERT INTO history_fts(history_fts, rowid, raw, text)
                    VALUES ('delete', old.id, old.raw, old.text);
                END;
                CREATE TRIGGER IF NOT EXISTS history_update AFTER UPDATE ON history BEGIN
                    INSERT INTO history_fts(history_fts, rowid, raw, text)
                    VALUES ('delete', old.id, old.raw, old.text);
                    INSERT INTO history_fts(rowid, raw, text) VALUES (new.id, new.raw, new.text);
                END;
                CREATE TABLE IF NOT EXISTS imports (name TEXT PRIMARY KEY);
            """)
            # Serialize inspection and ALTER across concurrent startups.
            db.execute('BEGIN IMMEDIATE')
            if 'language' not in {row['name'] for row in db.execute('PRAGMA table_info(history)')}:
                db.execute('ALTER TABLE history ADD COLUMN language TEXT')
        legacy = state / "history.jsonl"
        if legacy.exists():
            with self.connect() as db:
                # The marker also prevents duplicates if a crash happens before the rename.
                db.execute("BEGIN IMMEDIATE")
                if not db.execute("SELECT 1 FROM imports WHERE name='jsonl'").fetchone():
                    with legacy.open(encoding="utf-8") as file:
                        for line in file:
                            if not line.strip():
                                continue
                            old = json.loads(line)
                            if not old.get("raw", "").strip():
                                continue
                            record = {name: old[name] for name in self.columns if name not in {"time", "device", "mode", "language"}}
                            record['language'] = None
                            record.update(time=utc_time(old["timestamp"]), device="unknown",
                                          mode=modes.resolve(old.get("pipeline", "clean"))[0])
                            self.insert(db, record)
                    db.execute("INSERT INTO imports VALUES ('jsonl')")
            legacy.replace(state / "history.jsonl.imported")

    @contextmanager
    def connect(self):
        # Each request owns its connection; no connection crosses worker threads.
        db = sqlite3.connect(self.path, timeout=30)
        db.row_factory = sqlite3.Row
        try:
            with db:
                yield db
        finally:
            db.close()

    def insert(self, db, record):
        columns = ", ".join(self.columns)
        placeholders = ", ".join("?" for _ in self.columns)
        return db.execute(f"INSERT INTO history ({columns}) VALUES ({placeholders})",
                          [record.get(name) if name == 'language' else record[name] for name in self.columns]).lastrowid

    def add(self, record):
        with self.connect() as db:
            return self.insert(db, record)

    def search(self, params):
        limit = integer(params, "limit", 50, 200)
        before = integer(params, "before", maximum=2**63 - 1)
        clauses, values = [], []
        if before is not None:
            clauses.append("id < ?")
            values.append(before)
        for name in ("device", "mode"):
            if name in params:
                value = parameter(params, name)
                if not NAME.fullmatch(value):
                    raise ValueError(f"Invalid {name} filter.")
                clauses.append(f"{name} = ?")
                values.append(value)
        words = re.findall(r"[^\W_]+", parameter(params, "q"), flags=re.UNICODE)
        if words:
            clauses.append("id IN (SELECT rowid FROM history_fts WHERE history_fts MATCH ?)")
            values.append(" AND ".join('"' + word + '"*' for word in words))
        where = " WHERE " + " AND ".join(clauses) if clauses else ""
        with self.connect() as db:
            rows = [dict(row) for row in db.execute("SELECT * FROM history" + where + " ORDER BY id DESC LIMIT ?",
                                                   values + [limit + 1])]
        return {"data": rows[:limit], "next_before": rows[limit - 1]["id"] if len(rows) > limit else None}

    def corrections(self, params, glossary):
        limit = integer(params, "limit", 100, 500)
        min_count = integer(params, "min_count", 1)
        groups = {}

        def trim(word):
            start, end = 0, len(word)
            while start < end and unicodedata.category(word[start]).startswith("P"):
                start += 1
            while end > start and unicodedata.category(word[end - 1]).startswith("P"):
                end -= 1
            return word[start:end]

        with self.connect() as db:
            rows = db.execute("SELECT id, time, raw, text FROM history WHERE cleanup='ok' ORDER BY id")
            scanned = 0
            for row in rows:
                scanned += 1
                raw, final = row["raw"].split(), row["text"].split()
                matcher = difflib.SequenceMatcher(None, [trim(w).casefold() for w in raw],
                                                  [trim(w).casefold() for w in final], autojunk=False)
                for tag, a, b, c, d in matcher.get_opcodes():
                    if tag != "replace" or b - a > 4 or d - c > 4:
                        continue
                    source, target = trim(" ".join(raw[a:b])), trim(" ".join(final[c:d]))
                    if not any(char.isalpha() for char in target):
                        continue
                    key = (source.lower(), target)
                    group = groups.setdefault(key, {"raw": source, "final": target, "count": 0})
                    group.update(count=group["count"] + 1, last_id=row["id"], last_time=row["time"])
        terms = {term.casefold() for term in glossary}
        data = sorted((group for group in groups.values() if group["count"] >= min_count),
                      key=lambda group: (-group["count"], -group["last_id"]))[:limit]
        for group in data:
            group["in_glossary"] = group["final"].casefold() in terms
        return {"data": data, "entries_scanned": scanned}
