// Phase 0 placeholder. WS-F replaces this with the real app shell (see docs/PLAN.md).
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

type AppInfo = { name: string; version: string; api_version: number; platform: string };

function App() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    invoke<AppInfo>("app_info").then(setInfo).catch((e) => setErr(String(e)));
  }, []);

  return (
    <main className="container">
      <h1>Almond</h1>
      <p>Local-first meeting notes — Phase 0 skeleton.</p>
      {info && (
        <p>
          v{info.version} · API v{info.api_version} · {info.platform}
        </p>
      )}
      {err && <p>Not running inside Tauri: {err}</p>}
    </main>
  );
}

export default App;
