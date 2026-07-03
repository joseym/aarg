# Browser harness for the AARG wasm core

A one-page demo that runs `aarg-wasm`'s deterministic functions
(`validate`, `analyze_gap`, `project_ats`, `check_claims`) entirely in the
browser: no network, no server, no API key. The third panel demonstrates the
point: AARG's never-fabricate check running client-side.

## Build and run

The wasm bundle (`pkg/`) is generated, not committed. Build it, then serve the
directory over HTTP (ES modules + wasm can't load over `file://`):

```bash
# from the repo root
wasm-pack build crates/aarg-wasm --target web --out-dir www/pkg --out-name aarg_wasm
cd crates/aarg-wasm/www
python3 -m http.server 8000
# open http://localhost:8000
```

Panels 1 and 2 work out of the box with the embedded sample. Panel 3 wants a
canonical draft. Paste one from a real build (`~/aarg/builds/NN/canonical.json`),
project its ATS payload, then "Tamper & check" to watch a fabricated skill get
flagged.
