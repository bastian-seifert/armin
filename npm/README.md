# armin-opencode

[ARMIN](https://github.com/bastian-seifert/armin) — queryable decision memory
for AI coding agents — packaged for [opencode](https://opencode.ai).

```bash
npx armin-opencode install      # or: npm i -g armin-opencode && armin install
```

Prefer a global install over one-off `npx` runs: the plugin registration
points at the package's install location, and `npx` caches are evicted.

`armin install` downloads the `armin-engine` sidecar binary for your platform
from the [GitHub releases](https://github.com/bastian-seifert/armin/releases)
and registers + enables the plugin in your global opencode config
(`"armin": { "enabled": true }`). `ARMIN_ENABLED=1` remains as a
per-environment escape hatch.

See the [main repo](https://github.com/bastian-seifert/armin) for what the
agent gets (durable decisions/rules/open items, scoped reasoning-state brief,
compaction survival, live status page).
