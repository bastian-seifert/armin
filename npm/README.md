# armin-opencode

[ARMIN](https://github.com/bastian-seifert/armin) — queryable decision memory
for AI coding agents — packaged for [opencode](https://opencode.ai).

```bash
npx armin-opencode install      # or: npm i -g armin-opencode && armin install
export ARMIN_ENABLED=1
```

`armin install` downloads the `armin-engine` sidecar binary for your platform
from the [GitHub releases](https://github.com/bastian-seifert/armin/releases)
and registers the plugin in your global opencode config. Enable it per
environment with `ARMIN_ENABLED=1`.

See the [main repo](https://github.com/bastian-seifert/armin) for what the
agent gets (durable decisions/rules/open items, scoped reasoning-state brief,
compaction survival, live status page).
