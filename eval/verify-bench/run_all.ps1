# Reproduce the benchmark end to end (Windows PowerShell, from eval/verify-bench).
# Heavy jobs run strictly one after another (GPU / Ollama).
#   .\run_all.ps1 -Stage data      # transcribe + summaries + filter + assemble (needs the private recordings)
#   .\run_all.ps1 -Stage encoders  # Almond rules, SummaC, MiniCheck, SymAI, SymAI-VI
#   .\run_all.ps1 -Stage llm       # Almond current (verify.nli), judges, QA, MiniCheck+MT  (gemma4:12b)
#   .\run_all.ps1 -Stage metrics
param([string]$Stage = "all", [string[]]$Datasets = @("synthetic", "personal"))
$repo = Resolve-Path ..\..
$private = if ($env:VB_PRIVATE) { $env:VB_PRIVATE } else { "C:\Users\Nam\Projects\Deploying\SWE\Almond\personal_record\bench" }
$bench = Join-Path $repo "target\release\examples\bench.exe"

function DsPath($ds) { if ($ds -eq "synthetic") { Join-Path $PSScriptRoot "out" } else { $private } }

if ($Stage -in "all", "data") {
    uv run python -m vb.models
    uv run python -m vb.transcribe
    uv run python -m vb.build summarize
    uv run python -m vb.build filter
    uv run python -m vb.build assemble 10
}
if ($Stage -in "all", "encoders", "llm") {
    Push-Location $repo; cargo build --release -p almond-verify --example bench; Pop-Location
}
if ($Stage -in "all", "encoders") {
    foreach ($ds in $Datasets) {
        $d = DsPath $ds
        New-Item -ItemType Directory -Force "$d\results" | Out-Null
        & $bench --in "$d\$ds.json" --out "$d\results\almond_symbolic.jsonl" --config symbolic
        foreach ($s in "summac", "minicheck", "symai", "symai_vi") { uv run python -m vb.run $s $ds }
    }
}
if ($Stage -in "all", "llm") {
    foreach ($ds in $Datasets) {
        $d = DsPath $ds
        & $bench --in "$d\$ds.json" --out "$d\results\almond_full.jsonl" --config full --model gemma4:12b --cache "$d\cache\almond-judge"
        foreach ($s in "judge", "cove", "qa", "selfcheck", "minicheck_mt") { uv run python -m vb.run $s $ds }
    }
}
if ($Stage -in "all", "metrics") { uv run python -m vb.metrics }
