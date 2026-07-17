# Personal hooks

The portable runtime ships no opinionated hooks. Keep personal automation in a
separate configuration repository, preferably as an installable plugin with
its agent definitions, prompts, scripts, and tests together.

Only trusted plugins may execute command hooks. Review a plugin before
installing it with `grok plugin install <source> --trust`.
