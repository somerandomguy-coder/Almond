"""Minimal Ollama chat client with a sqlite response cache (one cache file per data folder, so
cached prompts built from private transcripts never leave the private folder)."""

from __future__ import annotations

import hashlib
import json
import sqlite3
import threading
import time
from pathlib import Path

import requests

from vb.paths import MODEL, OLLAMA


class Ollama:
    def __init__(self, cache: Path | None, model: str = MODEL, url: str = OLLAMA, num_ctx: int = 8192):
        self.model, self.url, self.num_ctx = model, url.rstrip("/"), num_ctx
        self.db = None
        self.lock = threading.Lock()
        if cache is not None:
            cache.parent.mkdir(parents=True, exist_ok=True)
            self.db = sqlite3.connect(str(cache), check_same_thread=False)
            self.db.execute("create table if not exists c (k text primary key, v text, ms real)")
        self.calls = 0
        self.hits = 0
        self.ms = 0.0
        self.last_ms = 0.0  # latency of the last request when it was first made (cache-independent)

    def chat(self, system: str, user: str, schema: dict | None = None, temperature: float = 0.0,
             seed: int | None = None, model: str | None = None, num_predict: int | None = None) -> str:
        model = model or self.model
        body = {"model": model, "stream": False, "think": False,
                "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
                "options": {"temperature": temperature, "num_ctx": self.num_ctx}}
        if seed is not None:
            body["options"]["seed"] = seed
        if num_predict:
            body["options"]["num_predict"] = num_predict
        if schema is not None:
            body["format"] = schema
        key = hashlib.sha256(json.dumps(body, sort_keys=True, ensure_ascii=False).encode()).hexdigest()
        if self.db is not None:
            with self.lock:
                row = self.db.execute("select v, ms from c where k=?", (key,)).fetchone()
            if row:
                self.hits += 1
                self.last_ms = row[1]
                return row[0]
        t0 = time.perf_counter()
        for attempt in range(3):
            try:
                r = requests.post(f"{self.url}/api/chat", json=body, timeout=600)
                r.raise_for_status()
                out = r.json()["message"]["content"]
                break
            except Exception:  # noqa: BLE001 - transient server hiccups
                if attempt == 2:
                    raise
                time.sleep(3)
        ms = (time.perf_counter() - t0) * 1000
        self.calls += 1
        self.ms += ms
        self.last_ms = ms
        if self.db is not None:
            with self.lock:
                self.db.execute("insert or replace into c values (?,?,?)", (key, out, ms))
                self.db.commit()
        return out

    def json(self, system: str, user: str, schema: dict, **kw) -> dict | None:
        raw = self.chat(system, user, schema=schema, **kw)
        try:
            a, b = raw.find("{"), raw.rfind("}")
            return json.loads(raw[a:b + 1])
        except Exception:  # noqa: BLE001
            return None
