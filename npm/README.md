# armin-opencode

[ARMIN](https://github.com/bastian-seifert/armin) — queryable decision memory
for AI coding agents — packaged for [opencode](https://opencode.ai).

```bash
npx armin-opencode install      # or: npm i -g armin-opencode && armin install
```

Prefer a global install over one-off `npx` runs: `npx` caches are evicted.

`armin install` downloads the `armin-engine` sidecar binary for your platform
from the [GitHub releases](https://github.com/bastian-seifert/armin/releases)
and registers the plugin in your global opencode config as a pinned npm tuple:

```jsonc
{
  "plugin": [["armin-opencode@<version>", { "enabled": true }]]
}
```

opencode then installs the package — with its `@opencode-ai/plugin` SDK
dependency — into its own plugin cache on next start. Tuple options are the
schema-safe config channel (opencode strips unknown top-level keys like the
legacy `"armin": {...}` section; options are passed straight to the plugin).
`ARMIN_ENABLED=1` remains as a per-environment escape hatch.

Verify a install (registration form, engine binary, opencode's plugin cache,
SDK resolution, module import):

```bash
armin doctor
```

See the [main repo](https://github.com/bastian-seifert/armin) for what the
agent gets (durable decisions/rules/open items, scoped reasoning-state brief,
compaction survival, live status page).
