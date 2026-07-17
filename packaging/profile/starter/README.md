# Starter Grok Build profile

This directory is a portable, credential-free profile template. Copy it to a
directory you control and point `GROK_HOME` at that directory:

```sh
cp -R profiles/starter my-grok-profile
export GROK_HOME="$PWD/my-grok-profile"
grok --help
```

The distribution process rejects inline credentials and machine-specific
paths in this profile. Keep API credentials in environment variables or use
the normal login flow after copying the profile.

The `agents`, `hooks`, `plugins`, and `skills` directories are intentionally
empty. Keep personal automation in a separate configuration repository or
installable plugin, then enable it only after copying this profile. Do not put
`auth.json`, sessions, managed policy, or machine-generated cache files into a
profile that will be shared.
